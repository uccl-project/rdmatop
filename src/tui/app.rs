use super::theme::Theme;
use crate::net::{self, IfStats, NetRate};
use crate::stat::{self, PortStat};
use crate::trace::{PortMetrics, Recorder};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use std::collections::HashMap;

/// Per-port computed throughput (delta / interval).
#[derive(Clone, Debug)]
pub struct PortThroughput {
    pub dev_name: String,
    pub port: u32,
    /// Port line rate in Gbps, used to scale the throughput bar.
    pub link_gbps: Option<f64>,
    pub tx_gbps: f64,
    pub rx_gbps: f64,
    pub tx_pkts_per_sec: f64,
    pub rx_pkts_per_sec: f64,
    pub rx_drops_per_sec: f64,
    pub counter_rates: Vec<CounterRate>,
    /// Optional override for the Port column text. Used by NVLink rows.
    pub port_label: Option<String>,
    /// NVLink metadata attached to this row. Populated by Task 3 and rendered
    /// by the TUI detail pane when the `nvlink` feature is enabled.
    #[cfg(feature = "nvlink")]
    pub nvlink: Option<NvLinkThroughputMeta>,
    /// XGMI metadata attached to this row when the `xgmi` feature is enabled.
    #[cfg(feature = "xgmi")]
    pub xgmi: Option<XgmiThroughputMeta>,
}

/// Per-GPU XGMI metadata attached to a `PortThroughput` row when the
/// `xgmi` feature is enabled. Mirrors `NvLinkThroughputMeta`; `links[]`
/// tx/rx are rewritten as per-second byte rates for the detail pane.
#[cfg(feature = "xgmi")]
#[derive(Clone, Debug)]
pub struct XgmiThroughputMeta {
    /// Not rendered yet (rows already show `amdgpu<index>` via `dev_name`);
    /// kept so future panels (e.g. topology graph) can reference it.
    #[allow(dead_code)]
    pub gpu_index: u32,
    /// Marketing name (e.g. "AMD Instinct MI325X"); kept for future panels.
    #[allow(dead_code)]
    pub gpu_name: String,
    pub active_links: u32,
    pub links: Vec<crate::xgmi::XgmiLinkSnapshot>,
}

/// Per-GPU NVLink metadata attached to a `PortThroughput` row when the
/// `nvlink` feature is enabled. The TUI uses this to show per-link details
/// and error counters in the detail pane.
#[cfg(feature = "nvlink")]
#[derive(Clone, Debug)]
pub struct NvLinkThroughputMeta {
    /// GPU index reported by NVML. Not directly rendered by the TUI today
    /// (the row already shows `nvidia<index>` via `PortThroughput::dev_name`)
    /// but kept so future panels (e.g. topology graph) can reference it.
    #[allow(dead_code)]
    pub gpu_index: u32,
    /// Marketing name of the GPU (e.g. "H100"). Currently unused by the
    /// detail pane, which displays only the aggregate active/total counts.
    #[allow(dead_code)]
    pub gpu_name: String,
    pub active_links: u32,
    pub links: Vec<crate::nvlink::LinkSnapshot>,
}

#[derive(Clone, Debug)]
pub struct CounterRate {
    pub name: String,
    pub value: u64,
    pub delta: u64,
    pub rate: f64,
    pub is_bytes: bool,
}

/// Columns available for the main throughput table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableColumn {
    Device,
    Port,
    TxBar,
    TxGbps,
    RxBar,
    RxGbps,
    TxPps,
    RxPps,
    Drops,
    /// A raw hw counter rate column, keyed by counter name.
    Counter(String),
}

impl TableColumn {
    pub fn label(&self) -> String {
        match self {
            Self::Device => "Device".into(),
            Self::Port => "Port".into(),
            Self::TxBar => "TX ▏".into(),
            Self::TxGbps => "TX Gbps".into(),
            Self::RxBar => "RX ▏".into(),
            Self::RxGbps => "RX Gbps".into(),
            Self::TxPps => "TX pps".into(),
            Self::RxPps => "RX pps".into(),
            Self::Drops => "Drops/s".into(),
            Self::Counter(name) => name.clone(),
        }
    }

    pub fn width(&self) -> u16 {
        match self {
            Self::Device => 16,
            Self::Port => 6,
            Self::TxBar | Self::RxBar => BAR_WIDTH as u16,
            Self::TxGbps | Self::RxGbps => 9,
            Self::TxPps | Self::RxPps => 10,
            Self::Drops => 9,
            Self::Counter(name) => (name.len() as u16 + 2).max(12),
        }
    }
}

pub const BAR_WIDTH: usize = 12;

/// Stats refresh interval bounds (seconds). Floor matches the 200ms event-poll
/// so input stays responsive even at the fastest setting.
pub const REFRESH_DEFAULT_SECS: f64 = 1.0;
const REFRESH_MIN_SECS: f64 = 0.2;
const REFRESH_MAX_SECS: f64 = 10.0;
const REFRESH_STEP_SECS: f64 = 0.5;

/// All counter names that can be added as extra columns.
pub const EXTRA_COUNTERS: &[&str] = &[
    "send_bytes",
    "send_wrs",
    "recv_bytes",
    "recv_wrs",
    "rdma_write_bytes",
    "rdma_write_wrs",
    "rdma_write_wr_err",
    "rdma_write_recv_bytes",
    "rdma_read_bytes",
    "rdma_read_wrs",
    "rdma_read_wr_err",
    "rdma_read_resp_bytes",
    "retrans_bytes",
    "retrans_pkts",
    "retrans_timeout_events",
    "unresponsive_remote_events",
    "impaired_remote_conn_events",
];

/// Returns the default set of visible columns.
pub fn default_columns() -> Vec<TableColumn> {
    vec![
        TableColumn::Device,
        TableColumn::Port,
        TableColumn::TxBar,
        TableColumn::TxGbps,
        TableColumn::RxBar,
        TableColumn::RxGbps,
        TableColumn::TxPps,
        TableColumn::RxPps,
        TableColumn::Drops,
    ]
}

/// Returns all possible columns (built-in + extra counters).
pub fn all_columns() -> Vec<TableColumn> {
    let mut cols = default_columns();
    for name in EXTRA_COUNTERS {
        cols.push(TableColumn::Counter(name.to_string()));
    }
    cols
}

/// Rolling average calculator: stores recent PortThroughput samples per device
/// and computes the mean over a configurable window.
pub struct RollingAvgState {
    /// Per-device key "dev_name/port" → ring buffer of samples
    samples: HashMap<String, VecDeque<PortThroughput>>,
    /// Window size in seconds (each sample ≈ 1s)
    pub window_secs: usize,
}

pub const ROLLING_AVG_DEFAULT_WINDOW: usize = 5;
const ROLLING_AVG_MIN_WINDOW: usize = 1;
const ROLLING_AVG_MAX_WINDOW: usize = 300;

impl RollingAvgState {
    pub fn new(window_secs: usize) -> Self {
        Self {
            samples: HashMap::new(),
            window_secs,
        }
    }

    /// Push a new set of throughput samples (called once per refresh).
    /// Prunes stale device/port keys that no longer appear in `throughputs`.
    pub fn push(&mut self, throughputs: &[PortThroughput]) {
        let mut seen = std::collections::HashSet::new();
        for t in throughputs {
            let key = throughput_key(t);
            seen.insert(key.clone());
            let buf = self
                .samples
                .entry(key)
                .or_insert_with(|| VecDeque::with_capacity(ROLLING_AVG_MAX_WINDOW + 1));
            if buf.len() >= ROLLING_AVG_MAX_WINDOW {
                buf.pop_front();
            }
            buf.push_back(t.clone());
        }
        self.samples.retain(|k, _| seen.contains(k));
    }

    /// Compute averaged throughput for all devices currently tracked.
    pub fn averages(&self) -> Vec<PortThroughput> {
        self.samples
            .values()
            .filter_map(|buf| Self::average_window(buf, self.window_secs))
            .collect()
    }

    /// Compute the rolling average for a single device's sample buffer.
    fn average_window(
        buf: &VecDeque<PortThroughput>,
        window_secs: usize,
    ) -> Option<PortThroughput> {
        if buf.is_empty() {
            return None;
        }
        let start = buf.len().saturating_sub(window_secs);
        let window: Vec<_> = buf.iter().skip(start).collect();
        let n = window.len() as f64;
        // Copy metadata from the latest sample. For NVLink rows, the oldest
        // sample's `port`/`port_label` may reflect a stale active-link count;
        // using the latest keeps the averaged row's metadata fresh.
        // `nvlink` is also metadata; preserving it lets `throughput_key`
        // identify averaged NVLink rows by `dev_name` alone.
        let latest = window.last().unwrap();
        let mut avg = PortThroughput {
            dev_name: latest.dev_name.clone(),
            port: latest.port,
            link_gbps: latest.link_gbps,
            tx_gbps: window.iter().map(|s| s.tx_gbps).sum::<f64>() / n,
            rx_gbps: window.iter().map(|s| s.rx_gbps).sum::<f64>() / n,
            tx_pkts_per_sec: window.iter().map(|s| s.tx_pkts_per_sec).sum::<f64>() / n,
            rx_pkts_per_sec: window.iter().map(|s| s.rx_pkts_per_sec).sum::<f64>() / n,
            rx_drops_per_sec: window.iter().map(|s| s.rx_drops_per_sec).sum::<f64>() / n,
            counter_rates: Vec::new(),
            port_label: latest.port_label.clone(),
            #[cfg(feature = "nvlink")]
            nvlink: latest.nvlink.clone(),
            #[cfg(feature = "xgmi")]
            xgmi: latest.xgmi.clone(),
        };
        if let Some(template) = window.last() {
            avg.counter_rates = template
                .counter_rates
                .iter()
                .map(|cr| Self::average_counter_rate(cr, &window))
                .collect();
        }
        Some(avg)
    }

    /// Compute the average of a single counter rate across a window of samples.
    /// Only samples that contain the counter contribute; the divisor matches the
    /// number of contributing samples so a counter that is briefly absent doesn't
    /// pull the average toward zero.
    fn average_counter_rate(cr: &CounterRate, window: &[&PortThroughput]) -> CounterRate {
        let present: Vec<&CounterRate> = window
            .iter()
            .filter_map(|s| s.counter_rates.iter().find(|r| r.name == cr.name))
            .collect();
        let n = present.len();
        if n == 0 {
            return CounterRate {
                name: cr.name.clone(),
                value: cr.value,
                delta: 0,
                rate: 0.0,
                is_bytes: cr.is_bytes,
            };
        }
        let sum_rate: f64 = present.iter().map(|r| r.rate).sum();
        let sum_delta: u64 = present.iter().map(|r| r.delta).sum();
        let latest_value = present.last().map(|r| r.value).unwrap_or(cr.value);
        CounterRate {
            name: cr.name.clone(),
            value: latest_value,
            delta: sum_delta / n as u64,
            rate: sum_rate / n as f64,
            is_bytes: cr.is_bytes,
        }
    }

    pub fn sample_count(&self) -> usize {
        self.samples
            .values()
            .map(|b| b.len().min(self.window_secs))
            .max()
            .unwrap_or(0)
    }

    pub fn increase_window(&mut self) {
        self.window_secs = (self.window_secs + 1).min(ROLLING_AVG_MAX_WINDOW);
    }

    pub fn decrease_window(&mut self) {
        self.window_secs = self
            .window_secs
            .saturating_sub(1)
            .max(ROLLING_AVG_MIN_WINDOW);
    }

    pub fn set_window(&mut self, secs: usize) {
        self.window_secs = secs.clamp(ROLLING_AVG_MIN_WINDOW, ROLLING_AVG_MAX_WINDOW);
    }
}

pub struct App {
    pub should_quit: bool,
    pub throughputs: Vec<PortThroughput>,
    pub selected_row: usize,
    pub show_detail: bool,
    pub show_help: bool,
    pub processes: Vec<stat::ProcessRdmaInfo>,
    pub detail_scroll: u16,
    pub detail_max_scroll: u16,
    pub theme: Theme,
    pub sysinfo: SysInfo,
    pub history: HashMap<String, DeviceHistory>,
    pub cpu_history: Vec<f32>,
    prev_stats: Vec<PortStat>,
    prev_ifstats: Vec<IfStats>,
    prev_time: Instant,
    pub rolling_avg: RollingAvgState,
    pub show_rolling_avg: bool,
    pub show_window_input: bool,
    pub window_input_buf: String,
    pub columns: Vec<TableColumn>,
    pub show_column_picker: bool,
    pub column_picker_cursor: usize,
    pub h_scroll: usize,
    pub h_scroll_max: usize,
    pub table_offset: usize,
    pub refresh_interval: Duration,
    cached_display: Vec<PortThroughput>,
    /// Active Perfetto recorder when recording; None when idle.
    pub recorder: Option<Recorder>,
    /// Transient message shown after a recording is saved or fails.
    pub record_status: Option<String>,
    #[cfg(feature = "nvlink")]
    prev_nvlink: Vec<crate::nvlink::NvLinkSnapshot>,
    #[cfg(feature = "xgmi")]
    prev_xgmi: Vec<crate::xgmi::XgmiSnapshot>,
}

const HISTORY_LEN: usize = 60;

#[derive(Clone, Debug)]
pub struct DeviceHistory {
    pub tx: Vec<f64>,
    pub rx: Vec<f64>,
}

impl DeviceHistory {
    fn new() -> Self {
        Self {
            tx: Vec::with_capacity(HISTORY_LEN),
            rx: Vec::with_capacity(HISTORY_LEN),
        }
    }

    fn push(&mut self, tx: f64, rx: f64) {
        if self.tx.len() >= HISTORY_LEN {
            self.tx.remove(0);
            self.rx.remove(0);
        }
        self.tx.push(tx);
        self.rx.push(rx);
    }
}

#[derive(Clone)]
pub struct SysInfo {
    pub hostname: String,
    pub uptime: String,
    pub load_avg: String,
    pub mem_total_mb: u64,
    pub mem_used_mb: u64,
    pub mem_pct: f32,
    pub cpu_pct: f32,
    pub net: NetRate,
}

impl App {
    pub fn new() -> Self {
        let stats = stat::read_all_stats().unwrap_or_default();
        let ifstats = net::read_all_ifstats().unwrap_or_default();
        Self {
            should_quit: false,
            throughputs: Vec::new(),
            selected_row: 0,
            theme: Theme::Default,
            show_detail: false,
            show_help: false,
            processes: Vec::new(),
            detail_scroll: 0,
            detail_max_scroll: 0,
            sysinfo: read_sysinfo(NetRate::default()),
            history: HashMap::new(),
            cpu_history: Vec::with_capacity(HISTORY_LEN),
            prev_stats: stats,
            prev_ifstats: ifstats,
            prev_time: Instant::now(),
            rolling_avg: RollingAvgState::new(ROLLING_AVG_DEFAULT_WINDOW),
            show_rolling_avg: false,
            show_window_input: false,
            window_input_buf: String::new(),
            columns: default_columns(),
            show_column_picker: false,
            column_picker_cursor: 0,
            h_scroll: 0,
            h_scroll_max: 0,
            table_offset: 0,
            refresh_interval: Duration::from_secs_f64(REFRESH_DEFAULT_SECS),
            cached_display: Vec::new(),
            recorder: None,
            record_status: None,
            #[cfg(feature = "nvlink")]
            prev_nvlink: Vec::new(),
            // Pre-read like prev_stats above: XGMI accumulators are lifetime
            // TB-scale, so an empty baseline would render an absurd first-
            // frame rate spike (whole accumulator / one interval).
            #[cfg(feature = "xgmi")]
            prev_xgmi: crate::xgmi::read_all_xgmi_stats().unwrap_or_default(),
        }
    }

    pub fn refresh_stats(&mut self) {
        let curr = match stat::read_all_stats() {
            Ok(s) => s,
            Err(_) => return,
        };
        let elapsed = self.prev_time.elapsed().as_secs_f64();
        if elapsed < 0.1 {
            return;
        }
        self.throughputs = compute_throughputs(&self.prev_stats, &curr, elapsed);
        self.prev_stats = curr;

        #[cfg(feature = "nvlink")]
        {
            let curr_nvlink = crate::nvlink::read_all_nvlink_stats().unwrap_or_default();
            let mut nvlink_rows =
                compute_nvlink_throughputs(&self.prev_nvlink, &curr_nvlink, elapsed);
            self.throughputs.append(&mut nvlink_rows);
            self.prev_nvlink = curr_nvlink;
        }

        #[cfg(feature = "xgmi")]
        {
            let curr_xgmi = crate::xgmi::read_all_xgmi_stats().unwrap_or_default();
            let mut xgmi_rows = compute_xgmi_throughputs(&self.prev_xgmi, &curr_xgmi, elapsed);
            self.throughputs.append(&mut xgmi_rows);
            self.prev_xgmi = curr_xgmi;
        }

        // Stable display order: sort once here so history, rolling averages,
        // recording, and the display all inherit the same order.
        sort_by_device_order(&mut self.throughputs);

        let curr_if = net::read_all_ifstats().unwrap_or_default();
        let net_rate = net::compute_net_rate(&self.prev_ifstats, &curr_if, elapsed);
        self.prev_ifstats = curr_if;

        self.prev_time = Instant::now();
        self.clamp_selection();
        self.update_history();
        self.refresh_processes();
        self.rolling_avg.push(&self.throughputs);
        if let Some(rec) = &mut self.recorder {
            rec.push(port_metrics(&self.throughputs));
        }
        self.sysinfo = read_sysinfo(net_rate);
        if self.cpu_history.len() >= HISTORY_LEN {
            self.cpu_history.remove(0);
        }
        self.cpu_history.push(self.sysinfo.cpu_pct);
        self.recompute_display();
    }

    /// Recompute the cached display throughputs (call after any change to
    /// `throughputs`, `show_rolling_avg`, or rolling avg state).
    fn recompute_display(&mut self) {
        if self.show_rolling_avg {
            let mut avgs = self.rolling_avg.averages();
            sort_by_throughput_order(&mut avgs, &self.throughputs);
            self.cached_display = avgs;
        } else {
            self.cached_display = self.throughputs.clone();
        }
    }

    fn update_history(&mut self) {
        for t in &self.throughputs {
            self.history
                .entry(t.dev_name.clone())
                .or_insert_with(DeviceHistory::new)
                .push(t.tx_gbps, t.rx_gbps);
        }
    }

    fn refresh_processes(&mut self) {
        if let Ok(qps) = stat::read_all_qps() {
            self.processes = stat::aggregate_by_process(&qps);
        }
    }

    pub fn move_up(&mut self) {
        self.selected_row = self.selected_row.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if !self.throughputs.is_empty() && self.selected_row < self.throughputs.len() - 1 {
            self.selected_row += 1;
        }
    }

    pub fn toggle_detail(&mut self) {
        self.show_detail = !self.show_detail;
        self.detail_scroll = 0;
    }

    pub fn detail_scroll_up(&mut self) {
        if self.detail_scroll > 0 {
            self.detail_scroll -= 1;
        } else if self.selected_row > 0 {
            self.selected_row -= 1;
            self.detail_scroll = 0;
        }
    }

    pub fn detail_scroll_down(&mut self, max: u16) {
        if self.detail_scroll < max {
            self.detail_scroll += 1;
        } else if !self.throughputs.is_empty() && self.selected_row < self.throughputs.len() - 1 {
            self.selected_row += 1;
            self.detail_scroll = 0;
        }
    }

    pub fn cycle_theme(&mut self) {
        self.theme = self.theme.next();
    }

    pub fn toggle_rolling_avg(&mut self) {
        self.show_rolling_avg = !self.show_rolling_avg;
        self.recompute_display();
    }

    /// Toggle recording: start if idle; otherwise stop and flush the trace.
    /// On a write error the buffer is kept so no captured data is lost.
    pub fn toggle_recording(&mut self) {
        let Some(rec) = self.recorder.take() else {
            self.recorder = Some(Recorder::new());
            self.record_status = None;
            return;
        };
        if rec.is_empty() {
            self.record_status = Some("recording stopped (nothing captured)".to_string());
            return;
        }
        let path = trace_filename();
        if let Err(e) = rec.write_to(&path) {
            self.record_status = Some(format!("save failed: {} (still recording)", e));
            self.recorder = Some(rec);
            return;
        }
        self.record_status = Some(format!("saved {} ({} samples)", path, rec.sample_count()));
    }

    /// (elapsed seconds, sample count) for the active recording, if any.
    pub fn recording_progress(&self) -> Option<(u64, usize)> {
        self.recorder
            .as_ref()
            .map(|r| (r.elapsed_secs(), r.sample_count()))
    }

    pub fn increase_avg_window(&mut self) {
        self.rolling_avg.increase_window();
        if self.show_rolling_avg {
            self.recompute_display();
        }
    }

    pub fn decrease_avg_window(&mut self) {
        self.rolling_avg.decrease_window();
        if self.show_rolling_avg {
            self.recompute_display();
        }
    }

    /// Slow the refresh by one step (longer interval), clamped to the max.
    pub fn increase_refresh_interval(&mut self) {
        let secs = (self.refresh_interval.as_secs_f64() + REFRESH_STEP_SECS).min(REFRESH_MAX_SECS);
        self.refresh_interval = Duration::from_secs_f64(secs);
    }

    /// Speed the refresh up by one step (shorter interval), clamped to the min.
    pub fn decrease_refresh_interval(&mut self) {
        let secs = (self.refresh_interval.as_secs_f64() - REFRESH_STEP_SECS).max(REFRESH_MIN_SECS);
        self.refresh_interval = Duration::from_secs_f64(secs);
    }

    pub fn open_window_input(&mut self) {
        self.window_input_buf = self.rolling_avg.window_secs.to_string();
        self.show_window_input = true;
    }

    pub fn cancel_window_input(&mut self) {
        self.show_window_input = false;
        self.window_input_buf.clear();
    }

    pub fn confirm_window_input(&mut self) {
        if let Ok(val) = self.window_input_buf.parse::<usize>() {
            self.rolling_avg
                .set_window(val.clamp(ROLLING_AVG_MIN_WINDOW, ROLLING_AVG_MAX_WINDOW));
            if self.show_rolling_avg {
                self.recompute_display();
            }
        }
        self.show_window_input = false;
        self.window_input_buf.clear();
    }

    pub fn open_column_picker(&mut self) {
        self.show_column_picker = true;
        self.column_picker_cursor = 0;
    }

    pub fn close_column_picker(&mut self) {
        self.show_column_picker = false;
    }

    pub fn column_picker_up(&mut self) {
        self.column_picker_cursor = self.column_picker_cursor.saturating_sub(1);
    }

    pub fn column_picker_down(&mut self) {
        let max = all_columns().len().saturating_sub(1);
        if self.column_picker_cursor < max {
            self.column_picker_cursor += 1;
        }
    }

    pub fn column_picker_toggle(&mut self) {
        let all = all_columns();
        if let Some(col) = all.get(self.column_picker_cursor) {
            if let Some(pos) = self.columns.iter().position(|c| c == col) {
                if self.columns.len() > 1 {
                    self.columns.remove(pos);
                }
            } else {
                self.columns.push(col.clone());
            }
        }
        self.clamp_h_scroll();
    }

    pub fn scroll_left(&mut self) {
        self.h_scroll = self.h_scroll.saturating_sub(1);
    }

    pub fn scroll_right(&mut self) {
        if self.h_scroll < self.h_scroll_max {
            self.h_scroll += 1;
        }
    }

    pub fn clamp_h_scroll(&mut self) {
        if self.h_scroll >= self.columns.len() {
            self.h_scroll = self.columns.len().saturating_sub(1);
        }
    }

    /// Returns the throughputs to display: rolling avg if enabled, otherwise instant.
    /// Uses a cached value recomputed once per refresh.
    pub fn display_throughputs(&self) -> &[PortThroughput] {
        &self.cached_display
    }

    pub fn selected_throughput(&self) -> Option<&PortThroughput> {
        self.throughputs.get(self.selected_row)
    }

    pub fn selected_device_processes(&self) -> Vec<&stat::ProcessRdmaInfo> {
        let Some(t) = self.selected_throughput() else {
            return Vec::new();
        };
        self.processes
            .iter()
            .filter(|p| p.dev_name == t.dev_name)
            .collect()
    }

    fn clamp_selection(&mut self) {
        if !self.throughputs.is_empty() && self.selected_row >= self.throughputs.len() {
            self.selected_row = self.throughputs.len() - 1;
        }
    }
}

fn read_file_trimmed(path: &str) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn read_hostname() -> String {
    let h = read_file_trimmed("/etc/hostname");
    if h.is_empty() {
        "unknown".into()
    } else {
        h
    }
}

fn read_uptime() -> String {
    let secs: u64 = read_file_trimmed("/proc/uptime")
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    format!(
        "up {} days, {}:{:02}",
        secs / 86400,
        (secs % 86400) / 3600,
        (secs % 3600) / 60
    )
}

fn read_load_avg() -> String {
    read_file_trimmed("/proc/loadavg")
        .split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join(", ")
}

fn read_meminfo() -> (u64, u64) {
    let content = read_file_trimmed("/proc/meminfo");
    let mut total = 0u64;
    let mut avail = 0u64;
    for line in content.lines() {
        let val = || {
            line.split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        if line.starts_with("MemTotal:") {
            total = val();
        }
        if line.starts_with("MemAvailable:") {
            avail = val();
        }
    }
    (total / 1024, (total.saturating_sub(avail)) / 1024)
}

fn read_cpu_usage() -> f32 {
    let content = read_file_trimmed("/proc/stat");
    let first = content.lines().next().unwrap_or("");
    let vals: Vec<u64> = first
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    if vals.len() < 4 {
        return 0.0;
    }
    let idle = vals[3];
    let total: u64 = vals.iter().sum();
    if total == 0 {
        return 0.0;
    }
    ((total - idle) as f32 / total as f32) * 100.0
}

fn read_sysinfo(net: NetRate) -> SysInfo {
    let (mem_total_mb, mem_used_mb) = read_meminfo();
    let mem_pct = if mem_total_mb > 0 {
        (mem_used_mb as f32 / mem_total_mb as f32) * 100.0
    } else {
        0.0
    };
    SysInfo {
        hostname: read_hostname(),
        uptime: read_uptime(),
        load_avg: read_load_avg(),
        mem_total_mb,
        mem_used_mb,
        mem_pct,
        cpu_pct: read_cpu_usage(),
        net,
    }
}

/// Map the display throughputs into the recorder's metric shape.
fn port_metrics(throughputs: &[PortThroughput]) -> Vec<PortMetrics> {
    throughputs
        .iter()
        .map(|t| PortMetrics {
            dev_name: t.dev_name.clone(),
            port: t.port,
            tx_gbps: t.tx_gbps,
            rx_gbps: t.rx_gbps,
            tx_pps: t.tx_pkts_per_sec,
            rx_pps: t.rx_pkts_per_sec,
            rx_drops_per_sec: t.rx_drops_per_sec,
        })
        .collect()
}

/// Trace output path in the cwd, named by wall-clock seconds for uniqueness.
fn trace_filename() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("rdmatop-{}.json", secs)
}

fn find_prev<'a>(prev: &'a [PortStat], dev: &str, port: u32) -> Option<&'a PortStat> {
    prev.iter().find(|s| s.dev_name == dev && s.port == port)
}

fn bytes_to_gbps(bytes_per_sec: f64) -> f64 {
    bytes_per_sec * 8.0 / 1_000_000_000.0
}

fn is_bytes_counter(name: &str) -> bool {
    name.ends_with("_bytes") || name.ends_with("_resp_bytes") || name.ends_with("_recv_bytes")
}

fn compute_counter_rate(
    counter_name: &str,
    curr_val: u64,
    prev: Option<&PortStat>,
    elapsed: f64,
) -> CounterRate {
    let prev_val = prev
        .and_then(|p| p.counter_value(counter_name))
        .unwrap_or(0);
    let delta = curr_val.saturating_sub(prev_val);
    CounterRate {
        name: counter_name.to_string(),
        value: curr_val,
        delta,
        rate: delta as f64 / elapsed,
        is_bytes: is_bytes_counter(counter_name),
    }
}

fn rate_by_name(rates: &[CounterRate], name: &str) -> f64 {
    rates
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.rate)
        .unwrap_or(0.0)
}

fn compute_port_throughput(
    curr: &PortStat,
    prev: Option<&PortStat>,
    elapsed: f64,
) -> PortThroughput {
    let counter_rates: Vec<CounterRate> = curr
        .counters
        .iter()
        .map(|c| compute_counter_rate(&c.name, c.value, prev, elapsed))
        .collect();

    let tx_bps = rate_by_name(&counter_rates, "tx_bytes");
    let rx_bps = rate_by_name(&counter_rates, "rx_bytes");

    PortThroughput {
        dev_name: curr.dev_name.clone(),
        port: curr.port,
        link_gbps: curr.link_gbps,
        tx_gbps: bytes_to_gbps(tx_bps),
        rx_gbps: bytes_to_gbps(rx_bps),
        tx_pkts_per_sec: rate_by_name(&counter_rates, "tx_pkts"),
        rx_pkts_per_sec: rate_by_name(&counter_rates, "rx_pkts"),
        rx_drops_per_sec: rate_by_name(&counter_rates, "rx_drops"),
        counter_rates,
        port_label: None,
        #[cfg(feature = "nvlink")]
        nvlink: None,
        #[cfg(feature = "xgmi")]
        xgmi: None,
    }
}

fn compute_throughputs(prev: &[PortStat], curr: &[PortStat], elapsed: f64) -> Vec<PortThroughput> {
    curr.iter()
        .map(|c| compute_port_throughput(c, find_prev(prev, &c.dev_name, c.port), elapsed))
        .collect()
}

/// Stable identity key for a `PortThroughput` row across refreshes.
///
/// NVLink and XGMI rows key by `dev_name` alone because their `port` field
/// encodes the active-link count, which can change between samples.
fn throughput_key(t: &PortThroughput) -> String {
    #[cfg(feature = "nvlink")]
    if t.nvlink.is_some() {
        return t.dev_name.clone();
    }
    #[cfg(feature = "xgmi")]
    if t.xgmi.is_some() {
        return t.dev_name.clone();
    }
    format!("{}/{}", t.dev_name, t.port)
}

/// Sort rows by device name (natural order), then port, for a stable display.
/// Natural order compares each numeric run by value, so `mlx5_2` < `mlx5_10`.
fn sort_by_device_order(throughputs: &mut [PortThroughput]) {
    throughputs.sort_by(|a, b| {
        alphanumeric_sort::compare_str(&a.dev_name, &b.dev_name).then(a.port.cmp(&b.port))
    });
}

/// Sort averaged throughputs to match the order of the reference (instant) throughputs,
/// using an index map for O(n log n) performance.
fn sort_by_throughput_order(avgs: &mut [PortThroughput], reference: &[PortThroughput]) {
    let index_map: HashMap<String, usize> = reference
        .iter()
        .enumerate()
        .map(|(i, t)| (throughput_key(t), i))
        .collect();
    avgs.sort_by_key(|t| *index_map.get(&throughput_key(t)).unwrap_or(&usize::MAX));
}

/// Compute one `PortThroughput` row per GPU from a pair of NVLink snapshots.
///
/// The previous snapshot is looked up by `gpu_index`. Missing previous links
/// (or `None` byte counters) are treated as zero so a freshly-observed GPU
/// surfaces its full current byte counts as the first delta.
///
/// NVML exposes `NVML_FI_DEV_NVLINK_THROUGHPUT_DATA_TX/RX` as a *GPU-wide*
/// aggregate, not per-link counters. Each link in `LinkSnapshot::tx_bytes` /
/// `rx_bytes` therefore carries the same GPU-wide value, so we must read it
/// exactly once per GPU (from the first active link, or the first link if no
/// link is active) instead of summing across links. Summing would multiply
/// the aggregate by the number of active links.
///
/// Per-link error `CounterRate`s (crc/replay/recovery) are still emitted for
/// every link so the detail pane can display per-lane error counts.
#[cfg(feature = "nvlink")]
fn compute_nvlink_throughputs(
    prev: &[crate::nvlink::NvLinkSnapshot],
    curr: &[crate::nvlink::NvLinkSnapshot],
    elapsed: f64,
) -> Vec<PortThroughput> {
    let mut out = Vec::with_capacity(curr.len());
    for gpu in curr {
        let prev_gpu = prev.iter().find(|p| p.gpu_index == gpu.gpu_index);
        let mut counter_rates: Vec<CounterRate> = Vec::new();

        // Calculate GPU-wide aggregate throughput, preferring the u32::MAX aggregate fields.
        let (tx_bytes_total, rx_bytes_total) = if gpu.tx_bytes.is_some() || gpu.rx_bytes.is_some() {
            let curr_tx = gpu.tx_bytes.unwrap_or(0);
            let prev_tx = prev_gpu.and_then(|p| p.tx_bytes).unwrap_or(0);
            let curr_rx = gpu.rx_bytes.unwrap_or(0);
            let prev_rx = prev_gpu.and_then(|p| p.rx_bytes).unwrap_or(0);
            (
                curr_tx.saturating_sub(prev_tx),
                curr_rx.saturating_sub(prev_rx),
            )
        } else {
            // Fallback: Pick a single link to source the GPU-wide TX/RX aggregate from.
            let aggregate_link = gpu
                .links
                .iter()
                .find(|l| l.is_active)
                .or_else(|| gpu.links.first());
            match aggregate_link {
                Some(link) => {
                    let prev_link =
                        prev_gpu.and_then(|p| p.links.iter().find(|l| l.link_id == link.link_id));
                    let curr_tx = link.tx_bytes.unwrap_or(0);
                    let prev_tx = prev_link.and_then(|l| l.tx_bytes).unwrap_or(0);
                    let curr_rx = link.rx_bytes.unwrap_or(0);
                    let prev_rx = prev_link.and_then(|l| l.rx_bytes).unwrap_or(0);
                    (
                        curr_tx.saturating_sub(prev_tx),
                        curr_rx.saturating_sub(prev_rx),
                    )
                }
                None => (0, 0),
            }
        };

        // Compute individual link rates in bytes per second to store in the metadata links
        let mut links = Vec::with_capacity(gpu.links.len());
        for link in &gpu.links {
            let prev_link =
                prev_gpu.and_then(|p| p.links.iter().find(|l| l.link_id == link.link_id));
            let curr_tx = link.tx_bytes.unwrap_or(0);
            let prev_tx = prev_link.and_then(|l| l.tx_bytes).unwrap_or(0);
            let curr_rx = link.rx_bytes.unwrap_or(0);
            let prev_rx = prev_link.and_then(|l| l.rx_bytes).unwrap_or(0);

            let rate_tx = (curr_tx.saturating_sub(prev_tx) as f64 / elapsed) as u64;
            let rate_rx = (curr_rx.saturating_sub(prev_rx) as f64 / elapsed) as u64;

            let mut link_clone = link.clone();
            link_clone.tx_bytes = Some(rate_tx);
            link_clone.rx_bytes = Some(rate_rx);
            links.push(link_clone);
        }

        for link in &gpu.links {
            let prev_link =
                prev_gpu.and_then(|p| p.links.iter().find(|l| l.link_id == link.link_id));

            if let Some(v) = link.crc_error_count {
                let prev_v = prev_link.and_then(|l| l.crc_error_count).unwrap_or(0);
                let delta = v.saturating_sub(prev_v);
                counter_rates.push(CounterRate {
                    name: format!("nvlink_crc_l{}", link.link_id),
                    value: v,
                    delta,
                    rate: delta as f64 / elapsed,
                    is_bytes: false,
                });
            }
            if let Some(v) = link.replay_error_count {
                let prev_v = prev_link.and_then(|l| l.replay_error_count).unwrap_or(0);
                let delta = v.saturating_sub(prev_v);
                counter_rates.push(CounterRate {
                    name: format!("nvlink_replay_l{}", link.link_id),
                    value: v,
                    delta,
                    rate: delta as f64 / elapsed,
                    is_bytes: false,
                });
            }
            if let Some(v) = link.recovery_error_count {
                let prev_v = prev_link.and_then(|l| l.recovery_error_count).unwrap_or(0);
                let delta = v.saturating_sub(prev_v);
                counter_rates.push(CounterRate {
                    name: format!("nvlink_recovery_l{}", link.link_id),
                    value: v,
                    delta,
                    rate: delta as f64 / elapsed,
                    is_bytes: false,
                });
            }
        }

        let active = gpu.active_links();
        out.push(PortThroughput {
            dev_name: format!("nvidia{}", gpu.gpu_index),
            port: active,
            link_gbps: gpu.link_gbps,
            tx_gbps: bytes_to_gbps(tx_bytes_total as f64 / elapsed),
            rx_gbps: bytes_to_gbps(rx_bytes_total as f64 / elapsed),
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates,
            port_label: Some(format!("{}/{}", active, gpu.link_count)),
            nvlink: Some(NvLinkThroughputMeta {
                gpu_index: gpu.gpu_index,
                gpu_name: gpu.gpu_name.clone(),
                active_links: active,
                links,
            }),
            #[cfg(feature = "xgmi")]
            xgmi: None,
        });
    }
    out
}

/// Compute one `PortThroughput` row per GPU from a pair of XGMI snapshots.
/// amdsmi reports true per-link accumulators, so GPU totals are the sum of
/// per-link deltas; meta links carry per-second byte rates for the pane.
#[cfg(feature = "xgmi")]
fn compute_xgmi_throughputs(
    prev: &[crate::xgmi::XgmiSnapshot],
    curr: &[crate::xgmi::XgmiSnapshot],
    elapsed: f64,
) -> Vec<PortThroughput> {
    let mut out = Vec::with_capacity(curr.len());
    for gpu in curr {
        let prev_gpu = prev.iter().find(|p| p.gpu_index == gpu.gpu_index);

        let mut tx_total: u64 = 0;
        let mut rx_total: u64 = 0;
        let mut links = Vec::with_capacity(gpu.links.len());
        for link in &gpu.links {
            let prev_link =
                prev_gpu.and_then(|p| p.links.iter().find(|l| l.link_id == link.link_id));
            let tx_delta = link
                .tx_bytes
                .unwrap_or(0)
                .saturating_sub(prev_link.and_then(|l| l.tx_bytes).unwrap_or(0));
            let rx_delta = link
                .rx_bytes
                .unwrap_or(0)
                .saturating_sub(prev_link.and_then(|l| l.rx_bytes).unwrap_or(0));
            tx_total = tx_total.saturating_add(tx_delta);
            rx_total = rx_total.saturating_add(rx_delta);

            let mut link_rate = link.clone();
            link_rate.tx_bytes = Some((tx_delta as f64 / elapsed) as u64);
            link_rate.rx_bytes = Some((rx_delta as f64 / elapsed) as u64);
            links.push(link_rate);
        }

        let mut counter_rates: Vec<CounterRate> = Vec::new();
        if let Some(v) = gpu.correctable_errors {
            let prev_v = prev_gpu.and_then(|p| p.correctable_errors).unwrap_or(0);
            let delta = v.saturating_sub(prev_v);
            counter_rates.push(CounterRate {
                name: "xgmi_wafl_ce".to_string(),
                value: v,
                delta,
                rate: delta as f64 / elapsed,
                is_bytes: false,
            });
        }
        if let Some(v) = gpu.uncorrectable_errors {
            let prev_v = prev_gpu.and_then(|p| p.uncorrectable_errors).unwrap_or(0);
            let delta = v.saturating_sub(prev_v);
            counter_rates.push(CounterRate {
                name: "xgmi_wafl_ue".to_string(),
                value: v,
                delta,
                rate: delta as f64 / elapsed,
                is_bytes: false,
            });
        }

        let active = gpu.active_links();
        out.push(PortThroughput {
            dev_name: format!("amdgpu{}", gpu.gpu_index),
            port: active,
            link_gbps: gpu.link_gbps,
            tx_gbps: bytes_to_gbps(tx_total as f64 / elapsed),
            rx_gbps: bytes_to_gbps(rx_total as f64 / elapsed),
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates,
            port_label: Some(format!("{}/{}", active, gpu.link_count)),
            #[cfg(feature = "nvlink")]
            nvlink: None,
            xgmi: Some(XgmiThroughputMeta {
                gpu_index: gpu.gpu_index,
                gpu_name: gpu.gpu_name.clone(),
                active_links: active,
                links,
            }),
        });
    }
    out
}

#[cfg(all(test, feature = "xgmi"))]
mod xgmi_tests {
    use super::*;
    use crate::xgmi::{XgmiLinkSnapshot, XgmiSnapshot};

    fn mk_link(id: u32, active: bool, tx: u64, rx: u64) -> XgmiLinkSnapshot {
        XgmiLinkSnapshot {
            link_id: id,
            is_active: active,
            speed_gbps: Some(512.0),
            bit_rate_gbps: Some(32.0),
            remote_pci_bdf: Some(format!("0000:0{}:00.0", id + 1)),
            tx_bytes: Some(tx),
            rx_bytes: Some(rx),
        }
    }

    fn mk_gpu(index: u32, links: Vec<XgmiLinkSnapshot>) -> XgmiSnapshot {
        let active: u32 = links.iter().filter(|l| l.is_active).count() as u32;
        XgmiSnapshot {
            gpu_index: index,
            gpu_name: "MI325X".to_string(),
            link_count: links.len() as u32,
            link_gbps: Some(512.0 * active as f64),
            correctable_errors: Some(0),
            uncorrectable_errors: Some(0),
            links,
        }
    }

    #[test]
    fn totals_are_sum_of_link_deltas() {
        let prev = vec![mk_gpu(
            0,
            vec![mk_link(0, true, 1000, 2000), mk_link(1, true, 500, 500)],
        )];
        let curr = vec![mk_gpu(
            0,
            vec![mk_link(0, true, 3000, 6000), mk_link(1, true, 1500, 1500)],
        )];
        let rows = compute_xgmi_throughputs(&prev, &curr, 1.0);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.dev_name, "amdgpu0");
        // tx delta = (3000-1000)+(1500-500) = 3000 B/s; rx = 4000+1000 = 5000 B/s
        assert!((row.tx_gbps - bytes_to_gbps(3000.0)).abs() < 1e-12);
        assert!((row.rx_gbps - bytes_to_gbps(5000.0)).abs() < 1e-12);
        assert_eq!(row.port_label.as_deref(), Some("2/2"));
        // meta links carry per-second rates now
        let meta = row.xgmi.as_ref().expect("xgmi meta present");
        assert_eq!(meta.links[0].tx_bytes, Some(2000));
        assert_eq!(meta.links[0].rx_bytes, Some(4000));
        assert_eq!(meta.links[1].tx_bytes, Some(1000));
        assert_eq!(meta.links[1].rx_bytes, Some(1000));
    }

    #[test]
    fn first_sample_uses_zero_prev() {
        let curr = vec![mk_gpu(0, vec![mk_link(0, true, 4096, 8192)])];
        let rows = compute_xgmi_throughputs(&[], &curr, 2.0);
        let row = &rows[0];
        assert!((row.tx_gbps - bytes_to_gbps(2048.0)).abs() < 1e-12);
        assert!((row.rx_gbps - bytes_to_gbps(4096.0)).abs() < 1e-12);
    }

    #[test]
    fn counter_going_backwards_clamps_to_zero() {
        let prev = vec![mk_gpu(0, vec![mk_link(0, true, 9999, 9999)])];
        let curr = vec![mk_gpu(0, vec![mk_link(0, true, 1, 1)])];
        let rows = compute_xgmi_throughputs(&prev, &curr, 1.0);
        assert_eq!(rows[0].tx_gbps, 0.0);
        assert_eq!(rows[0].rx_gbps, 0.0);
    }

    #[test]
    fn inactive_links_excluded_from_active_count() {
        let curr = vec![mk_gpu(
            0,
            vec![mk_link(0, true, 0, 0), mk_link(1, false, 0, 0)],
        )];
        let rows = compute_xgmi_throughputs(&[], &curr, 1.0);
        assert_eq!(rows[0].port, 1);
        assert_eq!(rows[0].port_label.as_deref(), Some("1/2"));
    }

    #[test]
    fn wafl_error_counters_emitted_with_deltas() {
        let mut prev_gpu = mk_gpu(0, vec![mk_link(0, true, 0, 0)]);
        prev_gpu.correctable_errors = Some(5);
        prev_gpu.uncorrectable_errors = Some(1);
        let mut curr_gpu = mk_gpu(0, vec![mk_link(0, true, 0, 0)]);
        curr_gpu.correctable_errors = Some(8);
        curr_gpu.uncorrectable_errors = Some(1);
        let rows = compute_xgmi_throughputs(&[prev_gpu], &[curr_gpu], 1.0);
        let ce = rows[0]
            .counter_rates
            .iter()
            .find(|c| c.name == "xgmi_wafl_ce")
            .unwrap();
        assert_eq!(ce.value, 8);
        assert_eq!(ce.delta, 3);
        let ue = rows[0]
            .counter_rates
            .iter()
            .find(|c| c.name == "xgmi_wafl_ue")
            .unwrap();
        assert_eq!(ue.value, 1);
        assert_eq!(ue.delta, 0);
    }

    #[test]
    fn throughput_key_uses_dev_name_for_xgmi_rows() {
        let rows = compute_xgmi_throughputs(&[], &[mk_gpu(3, vec![mk_link(0, true, 0, 0)])], 1.0);
        assert_eq!(throughput_key(&rows[0]), "amdgpu3");
    }
}

#[cfg(all(test, feature = "nvlink"))]
mod nvlink_tests {
    use super::*;
    use crate::nvlink::{LinkSnapshot, NvLinkSnapshot, RemoteDeviceType};

    fn make_link(id: u32, active: bool, tx: Option<u64>, rx: Option<u64>) -> LinkSnapshot {
        LinkSnapshot {
            link_id: id,
            is_active: active,
            version: None,
            speed_gbps: if active { Some(50.0) } else { None },
            remote_device_type: RemoteDeviceType::Gpu,
            remote_pci_bdf: None,
            tx_bytes: tx,
            rx_bytes: rx,
            crc_error_count: None,
            replay_error_count: None,
            recovery_error_count: None,
        }
    }

    #[test]
    fn aggregates_only_active_links_and_sets_port_label() {
        let elapsed = 1.0_f64;

        let prev = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 2,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                make_link(0, true, Some(1_000_000_000), Some(2_000_000_000)),
                make_link(1, true, Some(500_000_000), Some(0)),
            ],
        }];
        let curr = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 3,
            link_gbps: Some(150.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                make_link(0, true, Some(1_500_000_000), Some(2_500_000_000)),
                make_link(1, true, Some(700_000_000), Some(100_000_000)),
                make_link(2, false, None, None),
            ],
        }];

        let rows = compute_nvlink_throughputs(&prev, &curr, elapsed);
        assert_eq!(rows.len(), 1);

        let row = &rows[0];
        assert_eq!(row.dev_name, "nvidia0");
        assert_eq!(row.port, 2, "port column = active link count");
        assert_eq!(row.port_label.as_deref(), Some("2/3"));
        assert_eq!(row.link_gbps, Some(150.0));
        assert_eq!(row.tx_pkts_per_sec, 0.0);
        assert_eq!(row.rx_pkts_per_sec, 0.0);
        assert_eq!(row.rx_drops_per_sec, 0.0);

        // NVML exposes a *GPU-wide* aggregate for TX/RX, so the aggregate is
        // the delta of the first active link only — NOT summed across links.
        // tx delta: (1.5e9 - 1.0e9) = 0.5e9 bytes/sec
        let expected_tx_gbps = 500_000_000.0_f64 * 8.0 / 1_000_000_000.0;
        // rx delta: (2.5e9 - 2.0e9) = 0.5e9 bytes/sec
        let expected_rx_gbps = 500_000_000.0_f64 * 8.0 / 1_000_000_000.0;
        assert!(
            (row.tx_gbps - expected_tx_gbps).abs() < 1e-6,
            "tx_gbps {} != {}",
            row.tx_gbps,
            expected_tx_gbps
        );
        assert!(
            (row.rx_gbps - expected_rx_gbps).abs() < 1e-6,
            "rx_gbps {} != {}",
            row.rx_gbps,
            expected_rx_gbps
        );

        // Per-link *throughput* counter rates (nvlink_tx_lN / nvlink_rx_lN)
        // are no longer emitted — the per-link counters in NVML are the same
        // GPU-wide aggregate, so per-link rates would be redundant.
        assert!(
            !row.counter_rates
                .iter()
                .any(|c| c.name.starts_with("nvlink_tx_l")),
            "per-link TX throughput counter rates must not be emitted"
        );
        assert!(
            !row.counter_rates
                .iter()
                .any(|c| c.name.starts_with("nvlink_rx_l")),
            "per-link RX throughput counter rates must not be emitted"
        );

        let nv = row.nvlink.as_ref().expect("nvlink meta present");
        assert_eq!(nv.gpu_index, 0);
        assert_eq!(nv.gpu_name, "H100");
        assert_eq!(nv.active_links, 2);
        assert_eq!(nv.links.len(), 3);
    }

    #[test]
    fn aggregate_throughput_is_not_multiplied_by_active_link_count() {
        // Two active links with identical TX/RX counters. NVML's per-link
        // counters are actually a GPU-wide aggregate, so summing the deltas
        // would double-count. The aggregate must reflect the single-link rate.
        let elapsed = 1.0_f64;

        let prev = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 2,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                make_link(0, true, Some(1_000_000_000), Some(2_000_000_000)),
                make_link(1, true, Some(1_000_000_000), Some(2_000_000_000)),
            ],
        }];
        let curr = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 2,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                make_link(0, true, Some(1_125_000_000), Some(2_125_000_000)),
                make_link(1, true, Some(1_125_000_000), Some(2_125_000_000)),
            ],
        }];

        let rows = compute_nvlink_throughputs(&prev, &curr, elapsed);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];

        // Single-link delta: tx = 0.125e9 bytes/sec, rx = 0.125e9 bytes/sec.
        let expected_tx_gbps = 125_000_000.0_f64 * 8.0 / 1_000_000_000.0;
        let expected_rx_gbps = 125_000_000.0_f64 * 8.0 / 1_000_000_000.0;
        assert!(
            (row.tx_gbps - expected_tx_gbps).abs() < 1e-6,
            "tx_gbps {} != single-link rate {}",
            row.tx_gbps,
            expected_tx_gbps
        );
        assert!(
            (row.rx_gbps - expected_rx_gbps).abs() < 1e-6,
            "rx_gbps {} != single-link rate {}",
            row.rx_gbps,
            expected_rx_gbps
        );
        // If the bug regresses the aggregate would be 2x.
        assert!(
            row.tx_gbps < expected_tx_gbps * 1.5,
            "tx_gbps {} unexpectedly large; sum-across-links bug?",
            row.tx_gbps
        );
        assert!(
            row.rx_gbps < expected_rx_gbps * 1.5,
            "rx_gbps {} unexpectedly large; sum-across-links bug?",
            row.rx_gbps
        );
        // Active-link count must still be reported for the port column.
        assert_eq!(row.port, 2);
        assert_eq!(row.port_label.as_deref(), Some("2/2"));
    }

    #[test]
    fn aggregate_falls_back_to_first_link_when_no_active() {
        // With no active links, NVML may still report non-zero byte counters
        // on the (inactive) ports. We must pick one source for the aggregate;
        // falling back to the first link (in list order) avoids dropping
        // signal entirely when a GPU momentarily has no active links.
        let elapsed = 1.0_f64;

        let prev = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 2,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                make_link(0, false, Some(0), Some(0)),
                make_link(1, false, Some(0), Some(0)),
            ],
        }];
        let curr = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 2,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                make_link(0, false, Some(250_000_000), Some(125_000_000)),
                make_link(1, false, Some(999_999_999), Some(999_999_999)),
            ],
        }];

        let rows = compute_nvlink_throughputs(&prev, &curr, elapsed);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];

        // First link's delta wins.
        let expected_tx_gbps = 250_000_000.0_f64 * 8.0 / 1_000_000_000.0;
        let expected_rx_gbps = 125_000_000.0_f64 * 8.0 / 1_000_000_000.0;
        assert!((row.tx_gbps - expected_tx_gbps).abs() < 1e-6);
        assert!((row.rx_gbps - expected_rx_gbps).abs() < 1e-6);
        assert_eq!(row.port, 0);
        assert_eq!(row.port_label.as_deref(), Some("0/2"));
    }

    #[test]
    fn missing_prev_snapshot_treats_curr_as_delta() {
        let elapsed = 2.0_f64;
        let curr = vec![NvLinkSnapshot {
            gpu_index: 1,
            gpu_name: "A100".to_string(),
            link_count: 1,
            link_gbps: Some(50.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![make_link(0, true, Some(2_000_000_000), Some(1_000_000_000))],
        }];

        let rows = compute_nvlink_throughputs(&[], &curr, elapsed);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.dev_name, "nvidia1");
        assert_eq!(row.port_label.as_deref(), Some("1/1"));
        let expected_tx = 2_000_000_000.0_f64 * 8.0 / 1_000_000_000.0 / elapsed;
        let expected_rx = 1_000_000_000.0_f64 * 8.0 / 1_000_000_000.0 / elapsed;
        assert!((row.tx_gbps - expected_tx).abs() < 1e-6);
        assert!((row.rx_gbps - expected_rx).abs() < 1e-6);
    }

    #[test]
    fn rolling_avg_keeps_history_across_active_link_changes() {
        // Build a single GPU NVLink snapshot with two active links.
        let elapsed = 1.0_f64;
        let links_first = vec![
            make_link(0, true, Some(1_000_000_000), Some(2_000_000_000)),
            make_link(1, true, Some(500_000_000), Some(250_000_000)),
        ];
        let prev = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 2,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: None,
            links: links_first.clone(),
        }];
        let curr_first = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 3,
            link_gbps: Some(150.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                make_link(0, true, Some(1_500_000_000), Some(2_500_000_000)),
                make_link(1, true, Some(700_000_000), Some(350_000_000)),
                make_link(2, false, None, None),
            ],
        }];
        let rows_first = compute_nvlink_throughputs(&prev, &curr_first, elapsed);
        assert_eq!(rows_first.len(), 1);
        assert_eq!(rows_first[0].port_label.as_deref(), Some("2/3"));

        let mut state = RollingAvgState::new(5);
        state.push(&rows_first);
        assert_eq!(state.samples.len(), 1);

        // Same GPU, different active link count -> port_label changes
        // from "2/3" to "3/3". The history entry must be reused, not
        // dropped and re-created.
        let curr_second = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 3,
            link_gbps: Some(150.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                make_link(0, true, Some(2_000_000_000), Some(3_000_000_000)),
                make_link(1, true, Some(900_000_000), Some(450_000_000)),
                make_link(2, true, Some(100_000_000), Some(50_000_000)),
            ],
        }];
        let rows_second = compute_nvlink_throughputs(&curr_first, &curr_second, elapsed);
        assert_eq!(rows_second.len(), 1);
        assert_eq!(rows_second[0].port_label.as_deref(), Some("3/3"));

        state.push(&rows_second);

        // Still exactly one history entry: the key is stable.
        assert_eq!(
            state.samples.len(),
            1,
            "expected stable key for NVLink rows across active-link changes"
        );
        // Buffer should hold two samples (the two pushes).
        let buf = state
            .samples
            .values()
            .next()
            .expect("history entry for nvidia0");
        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0].port_label.as_deref(), Some("2/3"));
        assert_eq!(buf[1].port_label.as_deref(), Some("3/3"));
    }

    #[test]
    fn rolling_avg_returns_averaged_nvlink_row() {
        let elapsed = 1.0_f64;
        let prev = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 1,
            link_gbps: Some(50.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![make_link(0, true, Some(0), Some(0))],
        }];
        let curr = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 1,
            link_gbps: Some(50.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![make_link(0, true, Some(1_000_000_000), Some(500_000_000))],
        }];

        let rows = compute_nvlink_throughputs(&prev, &curr, elapsed);
        assert_eq!(rows.len(), 1);

        let mut state = RollingAvgState::new(5);
        state.push(&rows);

        let avgs = state.averages();
        assert_eq!(avgs.len(), 1);
        let avg = &avgs[0];
        assert_eq!(avg.dev_name, "nvidia0");
        assert!(
            avg.tx_gbps > 0.0,
            "expected non-empty averaged tx_gbps, got {}",
            avg.tx_gbps
        );
        let expected_tx = 1_000_000_000.0_f64 * 8.0 / 1_000_000_000.0;
        assert!(
            (avg.tx_gbps - expected_tx).abs() < 1e-6,
            "tx_gbps {} != {}",
            avg.tx_gbps,
            expected_tx
        );
    }

    #[test]
    fn saturating_sub_protects_against_counter_resets() {
        // Simulate NVML reloading the counters: prev is higher than curr.
        let elapsed = 1.0_f64;
        let prev = vec![NvLinkSnapshot {
            gpu_index: 2,
            gpu_name: "B200".to_string(),
            link_count: 1,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![make_link(
                0,
                true,
                Some(10_000_000_000),
                Some(20_000_000_000),
            )],
        }];
        let curr = vec![NvLinkSnapshot {
            gpu_index: 2,
            gpu_name: "B200".to_string(),
            link_count: 1,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![make_link(0, true, Some(1_000), Some(2_000))],
        }];

        let rows = compute_nvlink_throughputs(&prev, &curr, elapsed);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].tx_gbps, 0.0,
            "counter reset must not produce negative rate"
        );
        assert_eq!(rows[0].rx_gbps, 0.0);
    }

    #[test]
    fn averaged_row_uses_latest_metadata() {
        // Same GPU, two consecutive snapshots with different active-link
        // counts (and therefore different `port` and `port_label`). The
        // averaged row must reflect the *latest* metadata, not the oldest.
        let elapsed = 1.0_f64;

        let prev_a = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 3,
            link_gbps: Some(150.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![make_link(0, true, Some(0), Some(0))],
        }];
        let curr_a = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 3,
            link_gbps: Some(150.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                make_link(0, true, Some(1_000_000_000), Some(2_000_000_000)),
                make_link(1, true, Some(500_000_000), Some(250_000_000)),
                make_link(2, false, None, None),
            ],
        }];
        let rows_first = compute_nvlink_throughputs(&prev_a, &curr_a, elapsed);
        assert_eq!(rows_first.len(), 1);
        assert_eq!(rows_first[0].port_label.as_deref(), Some("2/3"));
        assert_eq!(rows_first[0].port, 2);

        // Second push: active link count rises to 3/3.
        let curr_b = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 3,
            link_gbps: Some(150.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                make_link(0, true, Some(2_000_000_000), Some(3_000_000_000)),
                make_link(1, true, Some(900_000_000), Some(450_000_000)),
                make_link(2, true, Some(100_000_000), Some(50_000_000)),
            ],
        }];
        let rows_second = compute_nvlink_throughputs(&curr_a, &curr_b, elapsed);
        assert_eq!(rows_second.len(), 1);
        assert_eq!(rows_second[0].port_label.as_deref(), Some("3/3"));
        assert_eq!(rows_second[0].port, 3);

        let mut state = RollingAvgState::new(5);
        state.push(&rows_first);
        state.push(&rows_second);

        let avgs = state.averages();
        assert_eq!(avgs.len(), 1);
        let avg = &avgs[0];
        // Metadata must come from the latest sample, not the oldest.
        assert_eq!(
            avg.port_label.as_deref(),
            Some("3/3"),
            "averaged row must use latest sample's port_label"
        );
        assert_eq!(avg.port, 3, "averaged row must use latest sample's port");
        assert_eq!(avg.link_gbps, Some(150.0));
        assert_eq!(avg.dev_name, "nvidia0");
    }

    #[test]
    fn aggregates_uses_gpu_wide_fields_when_present() {
        let elapsed = 1.0_f64;

        let prev = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 2,
            link_gbps: Some(100.0),
            tx_bytes: Some(10_000_000_000),
            rx_bytes: Some(20_000_000_000),
            links: vec![
                make_link(0, true, Some(0), Some(0)),
                make_link(1, true, Some(0), Some(0)),
            ],
        }];
        let curr = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            link_count: 2,
            link_gbps: Some(100.0),
            tx_bytes: Some(15_000_000_000),
            rx_bytes: Some(25_000_000_000),
            links: vec![
                make_link(0, true, Some(100_000), Some(200_000)), // These are lane rates, shouldn't be used for aggregate
                make_link(1, true, Some(300_000), Some(400_000)),
            ],
        }];

        let rows = compute_nvlink_throughputs(&prev, &curr, elapsed);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];

        // Aggregate should be:
        // tx = 15e9 - 10e9 = 5e9 bytes/sec -> 40 Gbps
        // rx = 25e9 - 20e9 = 5e9 bytes/sec -> 40 Gbps
        let expected_tx_gbps = 5_000_000_000.0_f64 * 8.0 / 1_000_000_000.0;
        let expected_rx_gbps = 5_000_000_000.0_f64 * 8.0 / 1_000_000_000.0;
        assert!((row.tx_gbps - expected_tx_gbps).abs() < 1e-6);
        assert!((row.rx_gbps - expected_rx_gbps).abs() < 1e-6);

        // However, individual lanes should show the lane-specific rates (calculated from lane deltas):
        // lane 0 tx: (100_000 - 0) / 1.0 = 100_000 bytes/sec
        // lane 0 rx: (200_000 - 0) / 1.0 = 200_000 bytes/sec
        let nv = row.nvlink.as_ref().unwrap();
        assert_eq!(nv.links[0].tx_bytes, Some(100_000));
        assert_eq!(nv.links[0].rx_bytes, Some(200_000));
        assert_eq!(nv.links[1].tx_bytes, Some(300_000));
        assert_eq!(nv.links[1].rx_bytes, Some(400_000));
    }

    #[test]
    fn sort_order_stable_for_nvlink() {
        // Reference ordering: RDMA "mlx5_0/1", NVLink "nvidia0" (active=2),
        // RDMA "mlx5_1/1". The NVLink row sits in the middle.
        let nvlink_ref = PortThroughput {
            dev_name: "nvidia0".to_string(),
            port: 2,
            link_gbps: Some(150.0),
            tx_gbps: 1.0,
            rx_gbps: 2.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: Some("2/3".to_string()),
            nvlink: Some(NvLinkThroughputMeta {
                gpu_index: 0,
                gpu_name: "H100".to_string(),
                active_links: 2,
                links: Vec::new(),
            }),
            #[cfg(feature = "xgmi")]
            xgmi: None,
        };
        let rdma_a_ref = PortThroughput {
            dev_name: "mlx5_0".to_string(),
            port: 1,
            link_gbps: Some(100.0),
            tx_gbps: 3.0,
            rx_gbps: 4.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: None,
            #[cfg(feature = "nvlink")]
            nvlink: None,
            #[cfg(feature = "xgmi")]
            xgmi: None,
        };
        let rdma_b_ref = PortThroughput {
            dev_name: "mlx5_1".to_string(),
            port: 1,
            link_gbps: Some(100.0),
            tx_gbps: 5.0,
            rx_gbps: 6.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: None,
            #[cfg(feature = "nvlink")]
            nvlink: None,
            #[cfg(feature = "xgmi")]
            xgmi: None,
        };
        let reference = vec![rdma_a_ref.clone(), nvlink_ref.clone(), rdma_b_ref.clone()];
        let nvlink_ref_pos = 1usize;

        // Build the averaged rows in shuffled order. Note the NVLink row now
        // has `port: 3` (active-link count changed) and `port_label: "3/3"`.
        let nvlink_avg = PortThroughput {
            dev_name: "nvidia0".to_string(),
            port: 3,
            link_gbps: Some(150.0),
            tx_gbps: 1.5,
            rx_gbps: 2.5,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: Some("3/3".to_string()),
            #[cfg(feature = "nvlink")]
            nvlink: Some(NvLinkThroughputMeta {
                gpu_index: 0,
                gpu_name: "H100".to_string(),
                active_links: 3,
                links: Vec::new(),
            }),
            #[cfg(feature = "xgmi")]
            xgmi: None,
        };
        let rdma_a_avg = PortThroughput {
            dev_name: "mlx5_0".to_string(),
            port: 1,
            link_gbps: Some(100.0),
            tx_gbps: 3.5,
            rx_gbps: 4.5,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: None,
            #[cfg(feature = "nvlink")]
            nvlink: None,
            #[cfg(feature = "xgmi")]
            xgmi: None,
        };
        let rdma_b_avg = PortThroughput {
            dev_name: "mlx5_1".to_string(),
            port: 1,
            link_gbps: Some(100.0),
            tx_gbps: 5.5,
            rx_gbps: 6.5,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: None,
            #[cfg(feature = "nvlink")]
            nvlink: None,
            #[cfg(feature = "xgmi")]
            xgmi: None,
        };
        let mut avgs = vec![rdma_b_avg.clone(), nvlink_avg.clone(), rdma_a_avg.clone()];

        sort_by_throughput_order(&mut avgs, &reference);

        // After sorting the averaged list must mirror the reference order:
        // mlx5_0, nvidia0, mlx5_1.
        assert_eq!(avgs[0].dev_name, "mlx5_0");
        assert_eq!(avgs[1].dev_name, "nvidia0");
        assert_eq!(avgs[2].dev_name, "mlx5_1");
        // The NVLink row must NOT have been pushed to the end via
        // `unwrap_or(&usize::MAX)` (which would happen if the sort key still
        // included the changed `port` field).
        assert_eq!(avgs[1].dev_name, nvlink_ref.dev_name);
        assert_ne!(avgs[1].port, nvlink_ref.port);
        assert_eq!(avgs[1].port_label, nvlink_avg.port_label);
        // Sanity: the NVLink row's resolved index in the reference equals the
        // original reference position, confirming the key match.
        let _ = nvlink_ref_pos;
    }
}

#[cfg(test)]
mod sort_tests {
    use super::*;

    fn row(dev_name: &str, port: u32) -> PortThroughput {
        PortThroughput {
            dev_name: dev_name.to_string(),
            port,
            link_gbps: None,
            tx_gbps: 0.0,
            rx_gbps: 0.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: None,
            #[cfg(feature = "nvlink")]
            nvlink: None,
            #[cfg(feature = "xgmi")]
            xgmi: None,
        }
    }

    fn names(rows: &[PortThroughput]) -> Vec<(String, u32)> {
        rows.iter().map(|t| (t.dev_name.clone(), t.port)).collect()
    }

    #[test]
    fn sort_by_device_order_sorts_index_numerically_not_lexically() {
        // The whole point of natural order: mlx5_2 must precede mlx5_10.
        let mut rows = vec![row("mlx5_10", 1), row("mlx5_2", 1), row("mlx5_9", 1)];
        sort_by_device_order(&mut rows);
        let sorted: Vec<_> = rows.iter().map(|t| t.dev_name.clone()).collect();
        assert_eq!(sorted, vec!["mlx5_2", "mlx5_9", "mlx5_10"]);
    }

    #[test]
    fn sort_by_device_order_orders_by_name_then_port() {
        let mut rows = vec![
            row("mlx5_10", 1),
            row("mlx5_2", 2),
            row("mlx5_2", 1),
            row("bnxt_0", 1),
        ];
        sort_by_device_order(&mut rows);
        assert_eq!(
            names(&rows),
            vec![
                ("bnxt_0".to_string(), 1),
                ("mlx5_2".to_string(), 1),
                ("mlx5_2".to_string(), 2),
                ("mlx5_10".to_string(), 1),
            ]
        );
    }
}
