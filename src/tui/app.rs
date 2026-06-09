use super::theme::Theme;
use crate::net::{self, IfStats, NetRate};
use crate::stat::{self, PortStat};
use std::collections::VecDeque;
use std::time::Instant;

use std::collections::HashMap;

/// Per-port computed throughput (delta / interval).
#[derive(Clone, Debug)]
pub struct PortThroughput {
    pub dev_name: String,
    pub port: u32,
    pub tx_gbps: f64,
    pub rx_gbps: f64,
    pub tx_pkts_per_sec: f64,
    pub rx_pkts_per_sec: f64,
    pub rx_drops_per_sec: f64,
    pub counter_rates: Vec<CounterRate>,
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
            let key = format!("{}/{}", t.dev_name, t.port);
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
        let first = window[0];
        let mut avg = PortThroughput {
            dev_name: first.dev_name.clone(),
            port: first.port,
            tx_gbps: window.iter().map(|s| s.tx_gbps).sum::<f64>() / n,
            rx_gbps: window.iter().map(|s| s.rx_gbps).sum::<f64>() / n,
            tx_pkts_per_sec: window.iter().map(|s| s.tx_pkts_per_sec).sum::<f64>() / n,
            rx_pkts_per_sec: window.iter().map(|s| s.rx_pkts_per_sec).sum::<f64>() / n,
            rx_drops_per_sec: window.iter().map(|s| s.rx_drops_per_sec).sum::<f64>() / n,
            counter_rates: Vec::new(),
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
    pub elapsed: f64,
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
    cached_display: Vec<PortThroughput>,
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
            elapsed: 1.0,
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
            cached_display: Vec::new(),
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
        self.elapsed = elapsed;
        self.throughputs = compute_throughputs(&self.prev_stats, &curr, elapsed);
        self.prev_stats = curr;

        let curr_if = net::read_all_ifstats().unwrap_or_default();
        let net_rate = net::compute_net_rate(&self.prev_ifstats, &curr_if, elapsed);
        self.prev_ifstats = curr_if;

        self.prev_time = Instant::now();
        self.clamp_selection();
        self.update_history();
        self.refresh_processes();
        self.rolling_avg.push(&self.throughputs);
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
        tx_gbps: bytes_to_gbps(tx_bps),
        rx_gbps: bytes_to_gbps(rx_bps),
        tx_pkts_per_sec: rate_by_name(&counter_rates, "tx_pkts"),
        rx_pkts_per_sec: rate_by_name(&counter_rates, "rx_pkts"),
        rx_drops_per_sec: rate_by_name(&counter_rates, "rx_drops"),
        counter_rates,
    }
}

fn compute_throughputs(prev: &[PortStat], curr: &[PortStat], elapsed: f64) -> Vec<PortThroughput> {
    curr.iter()
        .map(|c| compute_port_throughput(c, find_prev(prev, &c.dev_name, c.port), elapsed))
        .collect()
}

/// Sort averaged throughputs to match the order of the reference (instant) throughputs,
/// using an index map for O(n log n) performance.
fn sort_by_throughput_order(avgs: &mut [PortThroughput], reference: &[PortThroughput]) {
    let index_map: HashMap<(String, u32), usize> = reference
        .iter()
        .enumerate()
        .map(|(i, t)| ((t.dev_name.clone(), t.port), i))
        .collect();
    avgs.sort_by_key(|t| {
        *index_map
            .get(&(t.dev_name.clone(), t.port))
            .unwrap_or(&usize::MAX)
    });
}
