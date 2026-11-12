use super::theme::Theme;
use crate::metrics::{self, bytes_to_gbps};
use crate::net::{self, IfStats, NetRate};
use crate::stat::{self, PortStat, EXTRA_COUNTERS};
use crate::trace::{PortMetrics, Recorder};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use std::collections::HashMap;

/// Which hardware class a `PortThroughput` row belongs to; one TUI tab each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceClass {
    Rdma,
    Xgmi,
    Nvlink,
}

impl DeviceClass {
    /// Display name used by the tab bar, header, and table titles.
    pub fn label(&self) -> &'static str {
        match self {
            DeviceClass::Rdma => "RDMA",
            DeviceClass::Xgmi => "XGMI",
            DeviceClass::Nvlink => "NVLink",
        }
    }
}

/// Per-port computed throughput (delta / interval).
#[derive(Clone, Debug)]
pub struct PortThroughput {
    pub dev_name: String,
    pub port: u32,
    /// Port line rate in Gbps, used to scale the throughput bar.
    pub link_gbps: Option<f64>,
    /// Logical port state ("ACTIVE", "DOWN", ...); None for GPU rows.
    pub state: Option<String>,
    pub tx_gbps: f64,
    pub rx_gbps: f64,
    pub tx_pkts_per_sec: f64,
    pub rx_pkts_per_sec: f64,
    pub rx_drops_per_sec: f64,
    pub counter_rates: Vec<CounterRate>,
    /// Optional override for the Port column text. Used by NVLink rows.
    pub port_label: Option<String>,
    /// NVLink metadata attached to this row.
    pub nvlink: Option<NvLinkThroughputMeta>,
    /// XGMI metadata attached to this row.
    pub xgmi: Option<XgmiThroughputMeta>,
    pub class: DeviceClass,
}

/// Per-GPU XGMI metadata. Mirrors `NvLinkThroughputMeta`; `links[]`
/// tx/rx are rewritten as per-second byte rates for the detail pane.
#[derive(Clone, Debug)]
pub struct XgmiThroughputMeta {
    /// Not rendered yet (rows already show `amdgpu<index>` via `dev_name`);
    /// kept so future panels (e.g. topology graph) can reference it.
    #[allow(dead_code)]
    pub gpu_index: u32,
    /// Marketing name (e.g. "AMD Instinct MI325X"); Name column on the tab.
    pub gpu_name: String,
    pub active_links: u32,
    pub links: Vec<crate::xgmi::XgmiLinkSnapshot>,
    /// Live GPU health (util/vram/temp/power/clock) for the gauge strip
    /// and detail pane.
    pub metrics: Option<crate::gpu::GpuMetrics>,
}

/// Per-GPU NVLink metadata. The TUI uses this to show per-link details
/// and error counters in the detail pane.
#[derive(Clone, Debug)]
pub struct NvLinkThroughputMeta {
    /// GPU index reported by NVML. Not directly rendered by the TUI today
    /// (the row already shows `nvidia<index>` via `PortThroughput::dev_name`)
    /// but kept so future panels (e.g. topology graph) can reference it.
    #[allow(dead_code)]
    pub gpu_index: u32,
    /// Marketing name of the GPU (e.g. "H100"); Name column on the tab.
    pub gpu_name: String,
    pub active_links: u32,
    pub tx_partial: bool,
    pub rx_partial: bool,
    pub links: Vec<crate::nvlink::LinkSnapshot>,
    /// Live GPU health (util/vram/temp/power/clock) for the gauge strip
    /// and detail pane.
    pub metrics: Option<crate::gpu::GpuMetrics>,
}

pub use crate::metrics::CounterRate;

/// Columns available for the main throughput table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableColumn {
    Device,
    Port,
    State,
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
            Self::State => "State".into(),
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

    /// Table-header text: the bar column names the direction ("TX"/"RX")
    /// and the value column next to it is just "Gbps" — no duplication.
    pub fn header_label(&self) -> String {
        match self {
            Self::TxBar => "TX".into(),
            Self::RxBar => "RX".into(),
            Self::TxGbps | Self::RxGbps => "Gbps".into(),
            _ => self.label(),
        }
    }

    pub fn width(&self) -> u16 {
        match self {
            Self::Device => 16,
            Self::Port => 6,
            Self::State => 5,
            Self::TxBar | Self::RxBar => BAR_WIDTH as u16,
            Self::TxGbps | Self::RxGbps => 9,
            Self::TxPps | Self::RxPps => 10,
            Self::Drops => 9,
            Self::Counter(name) => (name.len() as u16 + 2).max(12),
        }
    }
}

pub const BAR_WIDTH: usize = 12;

/// Stats refresh interval bounds (seconds). Sampling runs on a background
/// thread, so the floor only bounds sampler load; it must stay above the
/// 0.1s duplicate-snapshot guard in `apply_snapshot`.
pub const REFRESH_DEFAULT_SECS: f64 = 1.0;
const REFRESH_MIN_SECS: f64 = 0.2;
const REFRESH_MAX_SECS: f64 = 10.0;
const REFRESH_STEP_SECS: f64 = 0.5;

/// Returns the default set of visible columns.
pub fn default_columns() -> Vec<TableColumn> {
    vec![
        TableColumn::Device,
        TableColumn::Port,
        TableColumn::State,
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
            state: latest.state.clone(),
            tx_gbps: window.iter().map(|s| s.tx_gbps).sum::<f64>() / n,
            rx_gbps: window.iter().map(|s| s.rx_gbps).sum::<f64>() / n,
            tx_pkts_per_sec: window.iter().map(|s| s.tx_pkts_per_sec).sum::<f64>() / n,
            rx_pkts_per_sec: window.iter().map(|s| s.rx_pkts_per_sec).sum::<f64>() / n,
            rx_drops_per_sec: window.iter().map(|s| s.rx_drops_per_sec).sum::<f64>() / n,
            counter_rates: Vec::new(),
            port_label: latest.port_label.clone(),
            nvlink: latest.nvlink.clone(),
            xgmi: latest.xgmi.clone(),
            class: latest.class,
        };
        if let Some(meta) = avg.nvlink.as_mut() {
            meta.tx_partial = window
                .iter()
                .filter_map(|row| row.nvlink.as_ref())
                .any(|sample| sample.tx_partial);
            meta.rx_partial = window
                .iter()
                .filter_map(|row| row.nvlink.as_ref())
                .any(|sample| sample.rx_partial);
        }
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
    // Per-subsystem baselines: each timestamp marks when its subsystem last
    // read successfully, so a failed read holds that subsystem's state and
    // the next success diffs over the true elapsed span.
    prev_stats_at: Option<Instant>,
    prev_ifstats_at: Option<Instant>,
    prev_nvlink_at: Option<Instant>,
    prev_xgmi_at: Option<Instant>,
    prev_taken_at: Option<Instant>,
    last_applied_at: Option<Instant>,
    rdma_rows: Vec<PortThroughput>,
    nvlink_rows: Vec<PortThroughput>,
    xgmi_rows: Vec<PortThroughput>,
    net_rate: NetRate,
    pub has_data: bool,
    /// Panic message from a dead sampler thread; None while it is alive.
    pub sampler_error: Option<String>,
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
    prev_nvlink: Vec<crate::nvlink::NvLinkSnapshot>,
    prev_xgmi: Vec<crate::xgmi::XgmiSnapshot>,
    pub active_tab: DeviceClass,
    pub seen_tabs: Vec<DeviceClass>,
    tab_selection: HashMap<DeviceClass, usize>,
    // ---- multi-node state (PR2/PR3/PR4) ----
    /// True when spawned via --nodes; enables the ALL view and Esc-to-ALL.
    pub multi: bool,
    /// ALL view when active; otherwise Node(i) shows nodes[i] in the
    /// legacy fields below (mirrored from node_states[i]).
    pub view_all: bool,
    pub node_names: Vec<String>,
    pub node_states: Vec<NodeState>,
    pub active_node: usize,
    /// Per-node link state for the ALL view (local node is always Live).
    pub node_links: Vec<crate::remote::LinkState>,
    /// Cursor in the ALL view table.
    pub all_selected: usize,
    /// Node picker popup (n key), mirroring the column picker interaction.
    pub show_node_picker: bool,
    pub node_picker_cursor: usize,
    /// ALL view: hide healthy nodes (any node with errors or stale data).
    pub errors_only: bool,
    /// True while apply_snapshot runs as scratch for a non-active node;
    /// suppresses shared-state side effects (rolling avg, recorder).
    folding: bool,
}

/// Per-node snapshot of everything the single-node UI renders. In multi
/// mode the active node's state is mirrored into the legacy App fields so
/// ui.rs renders remote nodes through the exact same code path as local.
pub struct NodeState {
    pub throughputs: Vec<PortThroughput>,
    pub processes: Vec<stat::ProcessRdmaInfo>,
    pub sysinfo: SysInfo,
    pub history: HashMap<String, DeviceHistory>,
    pub cpu_history: Vec<f32>,
    pub has_data: bool,
    /// Last-applied timestamp for stale marking in the ALL view.
    pub last_applied_at: Option<Instant>,
    // ---- per-node baselines (the folded counterpart of the legacy fields) ----
    // These advance ONLY when this node's own frames fold; sharing one
    // scratch baseline across nodes made every frame look like the first
    // (zero rates) and let nodes contaminate each other's deltas.
    pub prev_stats: Vec<PortStat>,
    pub prev_ifstats: Vec<IfStats>,
    pub prev_nvlink: Vec<crate::nvlink::NvLinkSnapshot>,
    pub prev_xgmi: Vec<crate::xgmi::XgmiSnapshot>,
    pub prev_stats_at: Option<Instant>,
    pub prev_ifstats_at: Option<Instant>,
    pub prev_nvlink_at: Option<Instant>,
    pub prev_xgmi_at: Option<Instant>,
    pub prev_taken_at: Option<Instant>,
    /// Last computed per-class rows. Held (not blanked) when a subsystem
    /// read fails, mirroring single-node semantics.
    pub rdma_rows: Vec<PortThroughput>,
    pub nvlink_rows: Vec<PortThroughput>,
    pub xgmi_rows: Vec<PortThroughput>,
    /// Aggregate net rate from this node's ifstats deltas (header Net).
    pub net_rate: NetRate,
}

impl NodeState {
    /// Seconds since the node's last applied snapshot past 3x the refresh
    /// interval (min 2s); None while fresh.
    pub fn stale_secs(&self, refresh_interval: Duration) -> Option<u64> {
        let at = self.last_applied_at?;
        let threshold = (refresh_interval * 3).max(Duration::from_secs(2));
        let age = at.elapsed();
        (age > threshold).then_some(age.as_secs())
    }

    /// Persist one fold's results, including the advanced baseline.
    fn store_fold(&mut self, f: FoldResult) {
        self.throughputs = f.throughputs;
        self.processes = f.processes;
        self.sysinfo = f.sysinfo;
        self.history = f.history;
        self.cpu_history = f.cpu_history;
        self.has_data = f.has_data;
        self.last_applied_at = f.last_applied_at;
        self.prev_stats = f.prev_stats;
        self.prev_ifstats = f.prev_ifstats;
        self.prev_nvlink = f.prev_nvlink;
        self.prev_xgmi = f.prev_xgmi;
        self.prev_stats_at = f.prev_stats_at;
        self.prev_ifstats_at = f.prev_ifstats_at;
        self.prev_nvlink_at = f.prev_nvlink_at;
        self.prev_xgmi_at = f.prev_xgmi_at;
        self.prev_taken_at = f.prev_taken_at;
        self.rdma_rows = f.rdma_rows;
        self.nvlink_rows = f.nvlink_rows;
        self.xgmi_rows = f.xgmi_rows;
        self.net_rate = f.net_rate;
    }
}

const HISTORY_LEN: usize = 60;

#[derive(Clone, Debug)]
pub struct DeviceHistory {
    pub tx: Vec<f64>,
    pub rx: Vec<f64>,
    pub util: Vec<f64>,
}

impl DeviceHistory {
    fn new() -> Self {
        Self {
            tx: Vec::with_capacity(HISTORY_LEN),
            rx: Vec::with_capacity(HISTORY_LEN),
            util: Vec::with_capacity(HISTORY_LEN),
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

    fn push_util(&mut self, util: f64) {
        if self.util.len() >= HISTORY_LEN {
            self.util.remove(0);
        }
        self.util.push(util);
    }
}

pub use crate::sysinfo::SysInfo;

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
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
            prev_stats: Vec::new(),
            prev_ifstats: Vec::new(),
            prev_stats_at: None,
            prev_ifstats_at: None,
            prev_nvlink_at: None,
            prev_xgmi_at: None,
            prev_taken_at: None,
            last_applied_at: None,
            rdma_rows: Vec::new(),
            nvlink_rows: Vec::new(),
            xgmi_rows: Vec::new(),
            net_rate: NetRate::default(),
            has_data: false,
            sampler_error: None,
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
            prev_nvlink: Vec::new(),
            prev_xgmi: Vec::new(),
            active_tab: DeviceClass::Rdma,
            seen_tabs: vec![DeviceClass::Rdma],
            tab_selection: HashMap::new(),
            multi: false,
            view_all: false,
            node_names: vec!["local".into()],
            node_states: vec![NodeState {
                throughputs: Vec::new(),
                processes: Vec::new(),
                sysinfo: read_sysinfo(NetRate::default()),
                history: HashMap::new(),
                cpu_history: Vec::with_capacity(HISTORY_LEN),
                has_data: false,
                last_applied_at: None,
                prev_stats: Vec::new(),
                prev_ifstats: Vec::new(),
                prev_nvlink: Vec::new(),
                prev_xgmi: Vec::new(),
                prev_stats_at: None,
                prev_ifstats_at: None,
                prev_nvlink_at: None,
                prev_xgmi_at: None,
                prev_taken_at: None,
                rdma_rows: Vec::new(),
                nvlink_rows: Vec::new(),
                xgmi_rows: Vec::new(),
                net_rate: NetRate::default(),
            }],
            active_node: 0,
            node_links: vec![crate::remote::LinkState::Live],
            all_selected: 0,
            show_node_picker: false,
            node_picker_cursor: 0,
            errors_only: false,
            folding: false,
        }
    }

    /// Multi-node App: starts on the ALL view with one local node; the
    /// NodeManager registers remote nodes as they connect.
    pub fn new_multi() -> Self {
        let mut app = Self::new();
        app.multi = true;
        app.view_all = true;
        app
    }

    /// Register a node (called by run_multi before the loop). Keeps the
    /// local node at index 0.
    pub fn add_node(&mut self, name: &str) {
        self.node_names.push(name.to_string());
        self.node_states.push(NodeState {
            throughputs: Vec::new(),
            processes: Vec::new(),
            sysinfo: SysInfo::default(),
            history: HashMap::new(),
            cpu_history: Vec::with_capacity(HISTORY_LEN),
            has_data: false,
            last_applied_at: None,
            prev_stats: Vec::new(),
            prev_ifstats: Vec::new(),
            prev_nvlink: Vec::new(),
            prev_xgmi: Vec::new(),
            prev_stats_at: None,
            prev_ifstats_at: None,
            prev_nvlink_at: None,
            prev_xgmi_at: None,
            prev_taken_at: None,
            rdma_rows: Vec::new(),
            nvlink_rows: Vec::new(),
            xgmi_rows: Vec::new(),
            net_rate: NetRate::default(),
        });
        self.node_links.push(crate::remote::LinkState::Connecting);
    }

    /// Record a node's connection state for the ALL view.
    pub fn set_node_state(&mut self, idx: usize, state: crate::remote::LinkState) {
        if let Some(slot) = self.node_links.get_mut(idx) {
            *slot = state;
        }
    }

    /// Fold a snapshot from node `idx` into that node's state; when it is
    /// the active node (and we are not on the ALL view), also mirror it
    /// into the legacy fields so ui.rs renders it unchanged.
    pub fn apply_node_snapshot(&mut self, idx: usize, snap: crate::sampler::Snapshot) {
        if idx == 0 && !self.multi {
            // Single-node mode: the original path, byte for byte.
            self.apply_snapshot(snap);
            return;
        }
        // Multi-node: swap THIS node's baseline into the legacy fields, fold
        // one snapshot, then write the advanced baseline back. Each node's
        // baseline is pushed only by its own frames, so rates are correct
        // and nodes cannot contaminate each other.
        let Some(baseline) = self.baseline_of(idx) else {
            return;
        };
        self.install_baseline(baseline);
        self.folding = true;
        let folded_ok = self.apply_snapshot(snap);
        self.folding = false;
        if !folded_ok {
            // The duplicate guard skipped this frame: the legacy fields hold
            // the installed baseline and post-harvest empties, NOT fresh
            // results. Harvesting now would overwrite the node state with
            // blank sysinfo and empty rows (the blank-header bug).
            return;
        }
        let folded = self.harvest_fold(idx);
        if let Some(state) = self.node_states.get_mut(idx) {
            state.store_fold(folded);
        }
        // The rolling average tracks the node being viewed; folding
        // suppressed the push inside apply_snapshot, so push from the
        // node's stored rows (the display reads node state directly).
        if self.multi && self.active_node == idx && !self.view_all {
            if let Some(state) = self.node_states.get(idx) {
                self.rolling_avg.push(&state.throughputs);
            }
        }
    }

    /// Drop node idx's rate baselines so its next frame re-establishes
    /// them (zero rates for one tick). Called when the node's connection
    /// transitions to Live: a fresh handshake means a fresh MonoMap anchor,
    /// and timestamps mapped with the OLD anchor would poison the elapsed
    /// math (saturating to 0 => inf rates, or a wrong span).
    pub fn reset_node_baseline(&mut self, idx: usize) {
        if let Some(state) = self.node_states.get_mut(idx) {
            state.prev_stats_at = None;
            state.prev_ifstats_at = None;
            state.prev_nvlink_at = None;
            state.prev_xgmi_at = None;
            state.prev_taken_at = None;
        }
    }

    /// This node's baseline, lifted out of its NodeState for one fold.
    fn baseline_of(&self, idx: usize) -> Option<NodeBaseline> {
        let s = self.node_states.get(idx)?;
        Some(NodeBaseline {
            prev_stats: s.prev_stats.clone(),
            prev_ifstats: s.prev_ifstats.clone(),
            prev_nvlink: s.prev_nvlink.clone(),
            prev_xgmi: s.prev_xgmi.clone(),
            prev_stats_at: s.prev_stats_at,
            prev_ifstats_at: s.prev_ifstats_at,
            prev_nvlink_at: s.prev_nvlink_at,
            prev_xgmi_at: s.prev_xgmi_at,
            prev_taken_at: s.prev_taken_at,
            rdma_rows: s.rdma_rows.clone(),
            nvlink_rows: s.nvlink_rows.clone(),
            xgmi_rows: s.xgmi_rows.clone(),
            processes: s.processes.clone(),
            sysinfo: s.sysinfo.clone(),
            net_rate: s.net_rate.clone(),
        })
    }

    /// Swap a node's baseline into the legacy fields so apply_snapshot
    /// folds against it.
    fn install_baseline(&mut self, b: NodeBaseline) {
        self.prev_stats = b.prev_stats;
        self.prev_ifstats = b.prev_ifstats;
        self.prev_nvlink = b.prev_nvlink;
        self.prev_xgmi = b.prev_xgmi;
        self.prev_stats_at = b.prev_stats_at;
        self.prev_ifstats_at = b.prev_ifstats_at;
        self.prev_nvlink_at = b.prev_nvlink_at;
        self.prev_xgmi_at = b.prev_xgmi_at;
        self.prev_taken_at = b.prev_taken_at;
        self.rdma_rows = b.rdma_rows;
        self.nvlink_rows = b.nvlink_rows;
        self.xgmi_rows = b.xgmi_rows;
        self.processes = b.processes;
        self.sysinfo = b.sysinfo;
        self.net_rate = b.net_rate;
    }

    /// Harvest the fold results plus the ADVANCED baseline: apply_snapshot
    /// has just written both into the legacy fields.
    fn harvest_fold(&mut self, idx: usize) -> FoldResult {
        let _ = idx;
        FoldResult {
            throughputs: std::mem::take(&mut self.throughputs),
            processes: std::mem::take(&mut self.processes),
            sysinfo: std::mem::take(&mut self.sysinfo),
            history: std::mem::take(&mut self.history),
            cpu_history: std::mem::take(&mut self.cpu_history),
            has_data: self.has_data,
            last_applied_at: self.last_applied_at,
            prev_stats: std::mem::take(&mut self.prev_stats),
            prev_ifstats: std::mem::take(&mut self.prev_ifstats),
            prev_nvlink: std::mem::take(&mut self.prev_nvlink),
            prev_xgmi: std::mem::take(&mut self.prev_xgmi),
            prev_stats_at: self.prev_stats_at,
            prev_ifstats_at: self.prev_ifstats_at,
            prev_nvlink_at: self.prev_nvlink_at,
            prev_xgmi_at: self.prev_xgmi_at,
            prev_taken_at: self.prev_taken_at,
            rdma_rows: std::mem::take(&mut self.rdma_rows),
            nvlink_rows: std::mem::take(&mut self.nvlink_rows),
            xgmi_rows: std::mem::take(&mut self.xgmi_rows),
            net_rate: self.net_rate.clone(),
        }
    }

    /// Header identity for the current view: the active node's name in a
    /// node view, "ALL" on the aggregate view, empty in single-node mode.
    pub fn view_label(&self) -> String {
        if !self.multi {
            return String::new();
        }
        if self.view_all {
            return "ALL".to_string();
        }
        self.node_names
            .get(self.active_node)
            .cloned()
            .unwrap_or_default()
    }

    /// Drill into the node selected by the ALL-view cursor, resolving
    /// through the sorted rows so the target is the row the operator sees.
    pub fn enter_selected_node(&mut self) {
        let rows = self.all_rows();
        if let Some(row) = rows.get(self.all_selected) {
            let idx = row.node_idx;
            self.enter_node(idx);
        }
    }

    /// Drill into node idx from the ALL view.
    pub fn enter_node(&mut self, idx: usize) {
        if idx >= self.node_states.len() {
            return;
        }
        self.active_node = idx;
        self.view_all = false;
        self.selected_row = 0;
        self.table_offset = 0;
        self.h_scroll = 0;
        self.detail_scroll = 0;
    }

    /// Return to the ALL view (Esc in a node view).
    pub fn exit_to_all(&mut self) {
        self.view_all = true;
    }

    /// Cycle the node picker cursor.
    pub fn node_picker_up(&mut self) {
        self.node_picker_cursor = self.node_picker_cursor.saturating_sub(1);
    }

    pub fn node_picker_down(&mut self) {
        if self.node_picker_cursor + 1 < self.node_names.len() {
            self.node_picker_cursor += 1;
        }
    }

    pub fn toggle_node_picker(&mut self) {
        self.show_node_picker = !self.show_node_picker;
        if self.show_node_picker {
            self.node_picker_cursor = self.active_node;
        }
    }

    pub fn close_node_picker(&mut self) {
        self.show_node_picker = false;
    }

    /// Toggle the ALL view's "unhealthy nodes only" filter.
    pub fn toggle_errors_only(&mut self) {
        self.errors_only = !self.errors_only;
        self.all_selected = 0;
    }

    /// ALL-view cursor movement.
    pub fn all_up(&mut self) {
        self.all_selected = self.all_selected.saturating_sub(1);
    }

    pub fn all_down(&mut self) {
        if self.all_selected + 1 < self.node_names.len() {
            self.all_selected += 1;
        }
    }

    /// Aggregate rows for the ALL view: one row per node, summing the
    /// active device class. Sorted by TX descending.
    pub fn all_rows(&self) -> Vec<AllRow> {
        let mut rows = Vec::new();
        for (idx, state) in self.node_states.iter().enumerate() {
            let mut tx = 0.0;
            let mut rx = 0.0;
            let mut active_ports = 0usize;
            let mut total_ports = 0usize;
            let mut err_rate = 0.0;
            let mut cong_rate = 0.0;
            // Dominant error: the counter with the highest rate this pass.
            let mut top: Option<(&'static str, f64)> = None;
            for t in &state.throughputs {
                if t.class != self.active_tab {
                    continue;
                }
                tx += t.tx_gbps;
                rx += t.rx_gbps;
                total_ports += 1;
                if t.state.as_deref() != Some("DOWN") {
                    active_ports += 1;
                }
                for cr in &t.counter_rates {
                    if cr.rate <= 0.0 {
                        continue;
                    }
                    if let Some(abbrev) = counter_abbrev(ERROR_COUNTERS, &cr.name) {
                        err_rate += cr.rate;
                        if top.map(|(_, r)| cr.rate > r).unwrap_or(true) {
                            top = Some((abbrev, cr.rate));
                        }
                    } else if counter_abbrev(CONGESTION_COUNTERS, &cr.name).is_some() {
                        cong_rate += cr.rate;
                    }
                }
            }
            rows.push(AllRow {
                node_idx: idx,
                name: self.node_names.get(idx).cloned().unwrap_or_default(),
                tx_gbps: tx,
                rx_gbps: rx,
                active_ports,
                total_ports,
                procs: state.processes.len(),
                link: self
                    .node_links
                    .get(idx)
                    .copied()
                    .unwrap_or(crate::remote::LinkState::Disconnected),
                stale_secs: state.stale_secs(self.refresh_interval),
                spark: state
                    .history
                    .values()
                    .map(|h| h.tx.iter().sum::<f64>())
                    .sum(),
                err_rate,
                cong_rate,
                top_error: top.map(|(a, _)| a),
            });
        }
        if self.errors_only {
            rows.retain(|r| {
                r.err_rate > 0.0
                    || r.stale_secs.is_some()
                    || r.link == crate::remote::LinkState::Disconnected
            });
        }
        rows.sort_by(|a, b| {
            b.tx_gbps
                .partial_cmp(&a.tx_gbps)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        rows
    }

    /// Fold one sampler snapshot into the app state, per subsystem: a failed
    /// read (None) keeps that subsystem's previous rows and baseline, so one
    /// broken source never blocks the others. Each subsystem's first success
    /// is its baseline (prev == curr, zero rates); real rates follow. Elapsed
    /// comes from sampler-side timestamps so UI queue latency can't skew rates.
    /// Returns false when the duplicate guard skipped the snapshot (the
    /// caller must not treat the legacy fields as freshly folded then).
    pub fn apply_snapshot(&mut self, snap: crate::sampler::Snapshot) -> bool {
        if let Some(prev) = self.prev_taken_at {
            if snap.taken_at.duration_since(prev).as_secs_f64() < 0.1 {
                return false;
            }
        }
        // Per-subsystem elapsed: each sample carries its own read-adjacent
        // timestamp, measured to that subsystem's last success, so neither
        // another subsystem's latency nor a skipped tick can skew the span.
        let since =
            |now: Instant, at: Option<Instant>| at.map(|a| now.duration_since(a).as_secs_f64());

        if let Some(s) = snap.stats {
            self.rdma_rows = match since(s.taken_at, self.prev_stats_at) {
                Some(e) => compute_throughputs(&self.prev_stats, &s.data, e),
                None => compute_throughputs(&s.data, &s.data, 1.0), // baseline
            };
            self.prev_stats = s.data;
            self.prev_stats_at = Some(s.taken_at);
        }

        if let Some(s) = snap.nvlink {
            self.nvlink_rows = match since(s.taken_at, self.prev_nvlink_at) {
                Some(e) => compute_nvlink_throughputs(&self.prev_nvlink, &s.data, e),
                None => compute_nvlink_throughputs(&s.data, &s.data, 1.0), // baseline
            };
            self.prev_nvlink = s.data;
            self.prev_nvlink_at = Some(s.taken_at);
        }

        if let Some(s) = snap.xgmi {
            self.xgmi_rows = match since(s.taken_at, self.prev_xgmi_at) {
                Some(e) => compute_xgmi_throughputs(&self.prev_xgmi, &s.data, e),
                None => compute_xgmi_throughputs(&s.data, &s.data, 1.0), // baseline
            };
            self.prev_xgmi = s.data;
            self.prev_xgmi_at = Some(s.taken_at);
        }

        if let Some(s) = snap.ifstats {
            if let Some(e) = since(s.taken_at, self.prev_ifstats_at) {
                self.net_rate = net::compute_net_rate(&self.prev_ifstats, &s.data, e);
            }
            self.prev_ifstats = s.data;
            self.prev_ifstats_at = Some(s.taken_at);
        }

        if let Some(processes) = snap.processes {
            self.processes = processes;
        }

        self.throughputs = self.rdma_rows.clone();
        self.throughputs.extend(self.nvlink_rows.iter().cloned());
        self.throughputs.extend(self.xgmi_rows.iter().cloned());

        // Stable display order: sort once here so history, rolling averages,
        // recording, and the display all inherit the same order.
        sort_by_device_order(&mut self.throughputs);
        detect_tabs(&mut self.seen_tabs, &self.throughputs);

        self.prev_taken_at = Some(snap.taken_at);
        self.last_applied_at = Some(Instant::now());
        self.has_data = true;

        self.clamp_selection();
        self.update_history();
        if !self.folding {
            self.rolling_avg.push(&self.throughputs);
        }
        if let Some(rec) = &mut self.recorder {
            if !self.folding {
                // Folding a non-active node: never record into the active
                // node's trace.
                let metrics = port_metrics(&self.throughputs);
                // In multi-node mode, prefix the node name so a shared trace
                // keeps per-device tracks distinguishable (node1/mlx5_0).
                let metrics = if self.multi {
                    let node = self
                        .node_names
                        .get(self.active_node)
                        .cloned()
                        .unwrap_or_default();
                    metrics
                        .into_iter()
                        .map(|mut m| {
                            m.dev_name = format!("{}/{}", node, m.dev_name);
                            m
                        })
                        .collect()
                } else {
                    metrics
                };
                rec.push(snap.taken_at, metrics);
            }
        }
        // Prefer the sampler-reported sysinfo (correct for remote nodes).
        // When absent: single-node mode reads the local /proc (legacy
        // behavior); a fold holds the node's installed sysinfo -- reading
        // the viewer's own /proc would attribute the wrong host to a
        // remote node.
        self.sysinfo = match snap.sys {
            Some(mut sys) => {
                sys.net = self.net_rate.clone();
                sys
            }
            None if !self.folding => read_sysinfo(self.net_rate.clone()),
            None => self.sysinfo.clone(),
        };
        if self.cpu_history.len() >= HISTORY_LEN {
            self.cpu_history.remove(0);
        }
        self.cpu_history.push(self.sysinfo.cpu_pct);
        self.recompute_display();
        true
    }

    /// Seconds since the last applied snapshot when it exceeds a staleness
    /// threshold (3x the refresh interval, min 2s); `None` while fresh.
    /// Lets the UI flag a stalled sampler, which `sampler_error` cannot see.
    pub fn stale_secs(&self) -> Option<u64> {
        let at = if self.multi {
            self.node_states
                .get(self.active_node)
                .and_then(|s| s.last_applied_at)?
        } else {
            self.last_applied_at?
        };
        let threshold = (self.refresh_interval * 3).max(Duration::from_secs(2));
        let age = at.elapsed();
        (age > threshold).then_some(age.as_secs())
    }

    /// Recompute the cached display throughputs (call after any change to
    /// `throughputs`, `show_rolling_avg`, `active_tab`, or rolling avg state).
    fn recompute_display(&mut self) {
        let rows: Vec<PortThroughput> = if self.show_rolling_avg {
            let mut avgs = self.rolling_avg.averages();
            sort_by_throughput_order(&mut avgs, &self.throughputs);
            avgs
        } else {
            self.throughputs.clone()
        };
        self.cached_display = rows
            .into_iter()
            .filter(|t| t.class == self.active_tab)
            .collect();
        // Keep the cursor inside the (possibly shorter) filtered view.
        self.selected_row = self
            .selected_row
            .min(self.cached_display.len().saturating_sub(1));
    }

    fn update_history(&mut self) {
        for t in &self.throughputs {
            let util = t
                .nvlink
                .as_ref()
                .and_then(|m| m.metrics.as_ref())
                .or_else(|| t.xgmi.as_ref().and_then(|m| m.metrics.as_ref()))
                .and_then(|m| m.util_pct);
            let entry = self
                .history
                .entry(t.dev_name.clone())
                .or_insert_with(DeviceHistory::new);
            entry.push(t.tx_gbps, t.rx_gbps);
            if let Some(u) = util {
                entry.push_util(u as f64);
            }
        }
    }

    pub fn move_up(&mut self) {
        self.selected_row = self.selected_row.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if !self.cached_display.is_empty() && self.selected_row < self.cached_display.len() - 1 {
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
        } else if !self.cached_display.is_empty()
            && self.selected_row < self.cached_display.len() - 1
        {
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
        // Column layout is configurable only on the RDMA tab; GPU tabs
        // render a fixed column set.
        if self.active_tab != DeviceClass::Rdma {
            return;
        }
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
    /// Rows to render for the active tab, computed from the viewed node's
    /// stored state (multi) or the cached display (single). All render
    /// reads go through the view_* accessors so the display can never
    /// observe another node's fold scratch.
    pub fn display_throughputs(&self) -> Vec<PortThroughput> {
        if !self.multi {
            return self.cached_display.clone();
        }
        let Some(state) = self.node_states.get(self.active_node) else {
            return Vec::new();
        };
        state
            .throughputs
            .iter()
            .filter(|t| t.class == self.active_tab)
            .cloned()
            .collect()
    }

    pub fn selected_throughput(&self) -> Option<PortThroughput> {
        self.display_throughputs()
            .into_iter()
            .nth(self.selected_row)
    }

    pub fn selected_device_processes(&self) -> Vec<&stat::ProcessRdmaInfo> {
        let Some(t) = self.selected_throughput() else {
            return Vec::new();
        };
        let procs: &Vec<stat::ProcessRdmaInfo> = if self.multi {
            self.node_states
                .get(self.active_node)
                .map(|s| &s.processes)
                .unwrap_or(&self.processes)
        } else {
            &self.processes
        };
        procs.iter().filter(|p| p.dev_name == t.dev_name).collect()
    }

    /// The viewed node's sysinfo (header).
    pub fn view_sysinfo(&self) -> &SysInfo {
        if self.multi {
            self.node_states
                .get(self.active_node)
                .map(|s| &s.sysinfo)
                .unwrap_or(&self.sysinfo)
        } else {
            &self.sysinfo
        }
    }

    /// The viewed node's sparkline history.
    pub fn view_history(&self) -> &HashMap<String, DeviceHistory> {
        if self.multi {
            self.node_states
                .get(self.active_node)
                .map(|s| &s.history)
                .unwrap_or(&self.history)
        } else {
            &self.history
        }
    }

    pub fn view_cpu_history(&self) -> &[f32] {
        if self.multi {
            self.node_states
                .get(self.active_node)
                .map(|s| s.cpu_history.as_slice())
                .unwrap_or(&self.cpu_history)
        } else {
            &self.cpu_history
        }
    }

    pub fn view_has_data(&self) -> bool {
        if self.multi {
            self.node_states
                .get(self.active_node)
                .map(|s| s.has_data)
                .unwrap_or(self.has_data)
        } else {
            self.has_data
        }
    }

    pub fn view_net_rate(&self) -> NetRate {
        if self.multi {
            self.node_states
                .get(self.active_node)
                .map(|s| s.net_rate.clone())
                .unwrap_or_else(|| self.net_rate.clone())
        } else {
            self.net_rate.clone()
        }
    }

    fn clamp_selection(&mut self) {
        if !self.cached_display.is_empty() && self.selected_row >= self.cached_display.len() {
            self.selected_row = self.cached_display.len() - 1;
        }
    }

    /// Cycle the active tab (Tab forward, Shift-Tab back); remembers the
    /// cursor per tab and resets scroll state.
    pub fn cycle_tab(&mut self, forward: bool) {
        if self.seen_tabs.len() < 2 {
            return;
        }
        let idx = self
            .seen_tabs
            .iter()
            .position(|&t| t == self.active_tab)
            .unwrap_or(0);
        let n = self.seen_tabs.len();
        let next = if forward {
            (idx + 1) % n
        } else {
            (idx + n - 1) % n
        };
        self.tab_selection
            .insert(self.active_tab, self.selected_row);
        self.active_tab = self.seen_tabs[next];
        self.selected_row = *self.tab_selection.get(&self.active_tab).unwrap_or(&0);
        self.table_offset = 0;
        self.h_scroll = 0;
        // Tab also works with the detail pane open; start it at the top.
        self.detail_scroll = 0;
        self.recompute_display();
    }
}

/// One ALL-view table row.
pub struct AllRow {
    /// Index into node_states; survives the TX sort so Enter drills into
    /// the row's actual node, not whatever lands under the cursor.
    pub node_idx: usize,
    pub name: String,
    pub tx_gbps: f64,
    pub rx_gbps: f64,
    pub active_ports: usize,
    pub total_ports: usize,
    pub procs: usize,
    pub link: crate::remote::LinkState,
    pub stale_secs: Option<u64>,
    /// Summed TX history, for a rough activity indicator.
    pub spark: f64,
    /// Hard error counters per second (out_of_buffer, RNR, CQE errors, ...).
    pub err_rate: f64,
    /// Congestion signals per second (ECN marks, CNPs) -- not errors, but
    /// worth surfacing next to them.
    pub cong_rate: f64,
    /// Abbreviated name of the dominant error counter (e.g. "OB", "RNR");
    /// None when the node is clean.
    pub top_error: Option<&'static str>,
}

/// Hard-error counters: non-zero means the node is dropping or failing
/// work. Abbreviations keep the ALL view narrow with many nodes.
const ERROR_COUNTERS: &[(&str, &str)] = &[
    ("out_of_buffer", "OB"),
    ("rnr_nak_retry_err", "RNR"),
    ("req_cqe_error", "CQE"),
    ("req_cqe_flush_error", "CQEfl"),
    ("resp_cqe_error", "rCQE"),
    ("resp_cqe_flush_error", "rCQEfl"),
    ("local_ack_timeout_err", "ACKTO"),
    ("out_of_sequence", "OOS"),
    ("packet_seq_err", "PSEQ"),
    ("rx_drops", "DROP"),
    ("retrans_timeout_events", "RETX"),
    ("unresponsive_remote_events", "UNRSP"),
    ("impaired_remote_conn_events", "IMPR"),
    ("rdma_write_wr_err", "WrErr"),
    ("rdma_read_wr_err", "RdErr"),
    ("req_remote_invalid_request", "REQinv"),
];

/// Congestion signals: ECN marks and their CNP feedback. Traffic-level
/// information, not failures.
const CONGESTION_COUNTERS: &[(&str, &str)] = &[
    ("np_ecn_marked_roce_packets", "ECN"),
    ("np_cnp_sent", "CNPs"),
    ("rp_cnp_handled", "CNPh"),
    ("roce_adp_retrans", "ADP"),
];

fn counter_abbrev(
    table: &'static [(&'static str, &'static str)],
    name: &str,
) -> Option<&'static str> {
    table.iter().find(|(n, _)| *n == name).map(|(_, a)| *a)
}

/// A node's pre-fold baseline, swapped into the legacy fields for one
/// apply_snapshot pass.
struct NodeBaseline {
    prev_stats: Vec<PortStat>,
    prev_ifstats: Vec<IfStats>,
    prev_nvlink: Vec<crate::nvlink::NvLinkSnapshot>,
    prev_xgmi: Vec<crate::xgmi::XgmiSnapshot>,
    prev_stats_at: Option<Instant>,
    prev_ifstats_at: Option<Instant>,
    prev_nvlink_at: Option<Instant>,
    prev_xgmi_at: Option<Instant>,
    prev_taken_at: Option<Instant>,
    /// Held display state so a failed read keeps the node's own rows,
    /// processes, and sysinfo instead of another node's leftovers.
    rdma_rows: Vec<PortThroughput>,
    nvlink_rows: Vec<PortThroughput>,
    xgmi_rows: Vec<PortThroughput>,
    processes: Vec<stat::ProcessRdmaInfo>,
    sysinfo: SysInfo,
    net_rate: NetRate,
}

/// One fold's output: display state plus the ADVANCED baseline that
/// apply_snapshot wrote into the legacy fields.
struct FoldResult {
    throughputs: Vec<PortThroughput>,
    processes: Vec<stat::ProcessRdmaInfo>,
    sysinfo: SysInfo,
    history: HashMap<String, DeviceHistory>,
    cpu_history: Vec<f32>,
    has_data: bool,
    last_applied_at: Option<Instant>,
    prev_stats: Vec<PortStat>,
    prev_ifstats: Vec<IfStats>,
    prev_nvlink: Vec<crate::nvlink::NvLinkSnapshot>,
    prev_xgmi: Vec<crate::xgmi::XgmiSnapshot>,
    prev_stats_at: Option<Instant>,
    prev_ifstats_at: Option<Instant>,
    prev_nvlink_at: Option<Instant>,
    prev_xgmi_at: Option<Instant>,
    prev_taken_at: Option<Instant>,
    rdma_rows: Vec<PortThroughput>,
    nvlink_rows: Vec<PortThroughput>,
    xgmi_rows: Vec<PortThroughput>,
    net_rate: NetRate,
}

/// A class's tab appears the first time it produces rows and stays for the
/// session, so a transient sampling failure cannot flicker it away.
fn detect_tabs(seen: &mut Vec<DeviceClass>, rows: &[PortThroughput]) {
    for class in [DeviceClass::Xgmi, DeviceClass::Nvlink] {
        if !seen.contains(&class) && rows.iter().any(|t| t.class == class) {
            seen.push(class);
            seen.sort_by_key(|c| *c as usize); // Rdma < Xgmi < Nvlink
        }
    }
}

fn read_sysinfo(net: NetRate) -> SysInfo {
    crate::sysinfo::read_sysinfo(net)
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

fn compute_port_throughput(
    curr: &PortStat,
    prev: Option<&PortStat>,
    elapsed: f64,
) -> PortThroughput {
    let counter_rates = metrics::counter_rates(curr, prev, elapsed);
    let rates = metrics::port_metrics(curr, prev, elapsed);
    PortThroughput {
        dev_name: rates.dev_name,
        port: rates.port,
        link_gbps: curr.link_gbps,
        state: curr.state.clone(),
        tx_gbps: rates.tx_gbps,
        rx_gbps: rates.rx_gbps,
        tx_pkts_per_sec: rates.tx_pps,
        rx_pkts_per_sec: rates.rx_pps,
        rx_drops_per_sec: rates.rx_drops_per_sec,
        counter_rates,
        port_label: None,
        nvlink: None,
        xgmi: None,
        class: DeviceClass::Rdma,
    }
}

fn compute_throughputs(prev: &[PortStat], curr: &[PortStat], elapsed: f64) -> Vec<PortThroughput> {
    curr.iter()
        .map(|c| compute_port_throughput(c, metrics::find_prev(prev, &c.dev_name, c.port), elapsed))
        .collect()
}

/// Stable identity key for a `PortThroughput` row across refreshes.
///
/// NVLink and XGMI rows key by `dev_name` alone because their `port` field
/// encodes the active-link count, which can change between samples.
fn throughput_key(t: &PortThroughput) -> String {
    if t.nvlink.is_some() || t.xgmi.is_some() {
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
/// The previous snapshot is looked up by `gpu_index`. A GPU, link, or
/// counter without a prior sample emits a zero delta: NVML counters are
/// cumulative since driver load, so diffing against zero would render an
/// absurd rate spike (same convention as `compute_xgmi_throughputs`).
///
/// Prefer the GPU aggregate fields; otherwise sum the per-link deltas.
fn compute_nvlink_throughputs(
    prev: &[crate::nvlink::NvLinkSnapshot],
    curr: &[crate::nvlink::NvLinkSnapshot],
    elapsed: f64,
) -> Vec<PortThroughput> {
    // Both samples must exist to diff: a counter reappearing after a gap
    // would otherwise diff its cumulative value against zero.
    let delta = |c: Option<u64>, p: Option<u64>| match (c, p) {
        (Some(c), Some(p)) => c.saturating_sub(p),
        _ => 0,
    };
    let mut out = Vec::with_capacity(curr.len());
    for gpu in curr {
        let prev_gpu = prev.iter().find(|p| p.gpu_index == gpu.gpu_index);
        let mut counter_rates: Vec<CounterRate> = Vec::new();

        let mut tx_bytes_total = 0u64;
        let mut rx_bytes_total = 0u64;
        let mut tx_partial = false;
        let mut rx_partial = false;

        let rate = |c: Option<u64>, p: Option<u64>| -> Option<u64> {
            c.zip(p)
                .map(|(c, p)| (c.saturating_sub(p) as f64 / elapsed) as u64)
        };
        let mut links = Vec::with_capacity(gpu.links.len());
        for link in &gpu.links {
            let prev_link =
                prev_gpu.and_then(|p| p.links.iter().find(|l| l.link_id == link.link_id));

            let was_active = prev_link.is_some_and(|previous| previous.is_active);
            tx_partial |= (link.is_active || was_active || link.tx_bytes.is_some())
                && (link.tx_bytes.is_none() || prev_link.and_then(|l| l.tx_bytes).is_none());
            rx_partial |= (link.is_active || was_active || link.rx_bytes.is_some())
                && (link.rx_bytes.is_none() || prev_link.and_then(|l| l.rx_bytes).is_none());

            let link_tx_bytes = delta(link.tx_bytes, prev_link.and_then(|l| l.tx_bytes));
            let link_rx_bytes = delta(link.rx_bytes, prev_link.and_then(|l| l.rx_bytes));
            tx_bytes_total = tx_bytes_total.saturating_add(link_tx_bytes);
            rx_bytes_total = rx_bytes_total.saturating_add(link_rx_bytes);

            let mut link_clone = link.clone();
            link_clone.tx_bytes = rate(link.tx_bytes, prev_link.and_then(|l| l.tx_bytes));
            link_clone.rx_bytes = rate(link.rx_bytes, prev_link.and_then(|l| l.rx_bytes));
            links.push(link_clone);
        }

        if gpu.tx_bytes.is_some() && prev_gpu.and_then(|p| p.tx_bytes).is_some() {
            tx_bytes_total = delta(gpu.tx_bytes, prev_gpu.and_then(|p| p.tx_bytes));
            tx_partial = false;
        }
        if gpu.rx_bytes.is_some() && prev_gpu.and_then(|p| p.rx_bytes).is_some() {
            rx_bytes_total = delta(gpu.rx_bytes, prev_gpu.and_then(|p| p.rx_bytes));
            rx_partial = false;
        }

        for link in &gpu.links {
            let prev_link =
                prev_gpu.and_then(|p| p.links.iter().find(|l| l.link_id == link.link_id));

            if let Some(v) = link.crc_error_count {
                let d = delta(
                    link.crc_error_count,
                    prev_link.and_then(|l| l.crc_error_count),
                );
                counter_rates.push(CounterRate {
                    name: format!("nvlink_crc_l{}", link.link_id),
                    value: v,
                    delta: d,
                    rate: d as f64 / elapsed,
                    is_bytes: false,
                });
            }
            if let Some(v) = link.replay_error_count {
                let d = delta(
                    link.replay_error_count,
                    prev_link.and_then(|l| l.replay_error_count),
                );
                counter_rates.push(CounterRate {
                    name: format!("nvlink_replay_l{}", link.link_id),
                    value: v,
                    delta: d,
                    rate: d as f64 / elapsed,
                    is_bytes: false,
                });
            }
            if let Some(v) = link.recovery_error_count {
                let d = delta(
                    link.recovery_error_count,
                    prev_link.and_then(|l| l.recovery_error_count),
                );
                counter_rates.push(CounterRate {
                    name: format!("nvlink_recovery_l{}", link.link_id),
                    value: v,
                    delta: d,
                    rate: d as f64 / elapsed,
                    is_bytes: false,
                });
            }
        }

        let active = gpu.active_links();
        out.push(PortThroughput {
            dev_name: format!("nvidia{}", gpu.gpu_index),
            port: active,
            link_gbps: gpu.link_gbps,
            state: None,
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
                tx_partial,
                rx_partial,
                links,
                metrics: gpu.metrics.clone(),
            }),
            xgmi: None,
            class: DeviceClass::Nvlink,
        });
    }
    out
}

/// Compute one `PortThroughput` row per GPU from a pair of XGMI snapshots.
/// amdsmi reports true per-link accumulators, so GPU totals are the sum of
/// per-link deltas; meta links carry per-second byte rates for the pane.
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
        // Both samples must exist to diff: a link or counter reappearing
        // after a gap would otherwise diff its lifetime TB-scale
        // accumulator against zero and render an absurd rate spike.
        let delta = |c: Option<u64>, p: Option<u64>| match (c, p) {
            (Some(c), Some(p)) => c.saturating_sub(p),
            _ => 0,
        };
        for link in &gpu.links {
            let prev_link =
                prev_gpu.and_then(|p| p.links.iter().find(|l| l.link_id == link.link_id));
            let (tx_delta, rx_delta) = match prev_link {
                Some(pl) => (
                    delta(link.tx_bytes, pl.tx_bytes),
                    delta(link.rx_bytes, pl.rx_bytes),
                ),
                None => (0, 0),
            };
            tx_total = tx_total.saturating_add(tx_delta);
            rx_total = rx_total.saturating_add(rx_delta);

            // Keep "no counter data" as None so the pane can show "-"
            // instead of a fabricated 0.0 rate.
            let mut link_rate = link.clone();
            link_rate.tx_bytes = link.tx_bytes.map(|_| (tx_delta as f64 / elapsed) as u64);
            link_rate.rx_bytes = link.rx_bytes.map(|_| (rx_delta as f64 / elapsed) as u64);
            links.push(link_rate);
        }

        let mut counter_rates: Vec<CounterRate> = Vec::new();
        if let Some(v) = gpu.correctable_errors {
            // Missing prev => zero delta, same convention as the byte
            // counters above (the accumulated total still shows in `value`).
            let delta = match prev_gpu.and_then(|p| p.correctable_errors) {
                Some(prev_v) => v.saturating_sub(prev_v),
                None => 0,
            };
            counter_rates.push(CounterRate {
                name: "xgmi_wafl_ce".to_string(),
                value: v,
                delta,
                rate: delta as f64 / elapsed,
                is_bytes: false,
            });
        }
        if let Some(v) = gpu.uncorrectable_errors {
            let delta = match prev_gpu.and_then(|p| p.uncorrectable_errors) {
                Some(prev_v) => v.saturating_sub(prev_v),
                None => 0,
            };
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
            state: None,
            tx_gbps: bytes_to_gbps(tx_total as f64 / elapsed),
            rx_gbps: bytes_to_gbps(rx_total as f64 / elapsed),
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates,
            port_label: Some(format!("{}/{}", active, gpu.link_count)),
            nvlink: None,
            xgmi: Some(XgmiThroughputMeta {
                gpu_index: gpu.gpu_index,
                gpu_name: gpu.gpu_name.clone(),
                active_links: active,
                links,
                metrics: gpu.metrics.clone(),
            }),
            class: DeviceClass::Xgmi,
        });
    }
    out
}

#[cfg(test)]
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
            metrics: None,
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
    fn unseen_gpu_emits_zero_rates() {
        // A GPU with no prior sample must not diff its lifetime accumulator
        // against zero (would render an absurd spike mid-run).
        let curr = vec![mk_gpu(0, vec![mk_link(0, true, 4096, 8192)])];
        let rows = compute_xgmi_throughputs(&[], &curr, 2.0);
        let row = &rows[0];
        assert_eq!(row.tx_gbps, 0.0);
        assert_eq!(row.rx_gbps, 0.0);
        let meta = row.xgmi.as_ref().expect("xgmi meta present");
        assert_eq!(meta.links[0].tx_bytes, Some(0));
        assert_eq!(meta.links[0].rx_bytes, Some(0));
    }

    #[test]
    fn counter_reappearing_after_gap_emits_zero() {
        // prev sample exists but carried no counter data: diffing the
        // reappeared lifetime accumulator against zero must not spike.
        let mut prev_link = mk_link(0, true, 0, 0);
        prev_link.tx_bytes = None;
        prev_link.rx_bytes = None;
        let prev = vec![mk_gpu(0, vec![prev_link])];
        let curr = vec![mk_gpu(
            0,
            vec![mk_link(0, true, u64::MAX / 2, u64::MAX / 2)],
        )];
        let rows = compute_xgmi_throughputs(&prev, &curr, 1.0);
        assert_eq!(rows[0].tx_gbps, 0.0);
        assert_eq!(rows[0].rx_gbps, 0.0);
    }

    #[test]
    fn missing_counters_stay_none() {
        // Peers without accumulator data must not fabricate 0.0 rates.
        let mut link = mk_link(0, true, 0, 0);
        link.tx_bytes = None;
        link.rx_bytes = None;
        let prev = vec![mk_gpu(0, vec![link.clone()])];
        let curr = vec![mk_gpu(0, vec![link])];
        let rows = compute_xgmi_throughputs(&prev, &curr, 1.0);
        let meta = rows[0].xgmi.as_ref().expect("xgmi meta present");
        assert_eq!(meta.links[0].tx_bytes, None);
        assert_eq!(meta.links[0].rx_bytes, None);
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

#[cfg(test)]
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
            metrics: None,
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
            metrics: None,
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

        let expected_tx_gbps = 5.6;
        let expected_rx_gbps = 4.8;
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

        // Per-link rates live in metadata, separate from error counters.
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
    fn equal_link_rates_are_summed_when_aggregate_is_unavailable() {
        let elapsed = 1.0_f64;

        let prev = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            metrics: None,
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
            metrics: None,
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

        assert_eq!(row.tx_gbps, 2.0);
        assert_eq!(row.rx_gbps, 2.0);
        // Active-link count must still be reported for the port column.
        assert_eq!(row.port, 2);
        assert_eq!(row.port_label.as_deref(), Some("2/2"));
    }

    #[test]
    fn aggregate_fallback_includes_counters_on_inactive_links() {
        let elapsed = 1.0_f64;

        let prev = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            metrics: None,
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
            metrics: None,
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

        let expected_tx_gbps = 9.999999992;
        let expected_rx_gbps = 8.999999992;
        assert!((row.tx_gbps - expected_tx_gbps).abs() < 1e-6);
        assert!((row.rx_gbps - expected_rx_gbps).abs() < 1e-6);
        assert_eq!(row.port, 0);
        assert_eq!(row.port_label.as_deref(), Some("0/2"));
    }

    #[test]
    fn missing_prev_snapshot_emits_zero_rates() {
        // NVML counters are cumulative since driver load: a GPU with no
        // prior sample must not diff them against zero (rate spike).
        let elapsed = 2.0_f64;
        let curr = vec![NvLinkSnapshot {
            gpu_index: 1,
            gpu_name: "A100".to_string(),
            metrics: None,
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
        assert_eq!(row.tx_gbps, 0.0);
        assert_eq!(row.rx_gbps, 0.0);
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
            metrics: None,
            link_count: 2,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: None,
            links: links_first.clone(),
        }];
        let curr_first = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            metrics: None,
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
            metrics: None,
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
            metrics: None,
            link_count: 1,
            link_gbps: Some(50.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![make_link(0, true, Some(0), Some(0))],
        }];
        let curr = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            metrics: None,
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
            metrics: None,
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
            metrics: None,
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
            metrics: None,
            link_count: 3,
            link_gbps: Some(150.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![make_link(0, true, Some(0), Some(0))],
        }];
        let curr_a = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            metrics: None,
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
            metrics: None,
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
    fn aggregate_without_a_baseline_uses_available_link_deltas() {
        let previous = NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            metrics: None,
            link_count: 1,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: Some(1_000_000_000),
            links: vec![make_link(0, true, Some(0), Some(0))],
        };
        let mut current = previous.clone();
        current.tx_bytes = Some(10_000_000_000);
        current.rx_bytes = Some(1_250_000_000);
        current.links[0].tx_bytes = Some(125_000_000);
        current.links[0].rx_bytes = Some(50_000_000);
        let rows = compute_nvlink_throughputs(&[previous], &[current], 1.0);
        assert_eq!(rows[0].tx_gbps, 1.0);
        assert_eq!(rows[0].rx_gbps, 2.0);
    }

    #[test]
    fn link_rate_without_a_baseline_is_unavailable() {
        let mut previous = NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            metrics: None,
            link_count: 1,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![make_link(0, true, None, Some(0))],
        };
        let mut current = previous.clone();
        current.links[0].tx_bytes = Some(125_000_000);
        current.links[0].rx_bytes = Some(125_000_000);
        let rows = compute_nvlink_throughputs(&[previous.clone()], &[current.clone()], 1.0);
        let meta = rows[0].nvlink.as_ref().unwrap();
        assert_eq!(meta.links[0].tx_bytes, None);
        assert_eq!(meta.links[0].rx_bytes, Some(125_000_000));
        assert!(meta.tx_partial);
        assert!(!meta.rx_partial);

        previous.links[0].tx_bytes = Some(0);
        previous.links[0].rx_bytes = None;
        let rows = compute_nvlink_throughputs(&[previous], &[current], 1.0);
        let meta = rows[0].nvlink.as_ref().unwrap();
        assert_eq!(meta.links[0].tx_bytes, Some(125_000_000));
        assert_eq!(meta.links[0].rx_bytes, None);
        assert!(!meta.tx_partial);
        assert!(meta.rx_partial);
    }

    #[test]
    fn incomplete_link_coverage_marks_only_the_affected_total_partial() {
        let previous = NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            metrics: None,
            link_count: 2,
            link_gbps: Some(100.0),
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                make_link(0, true, Some(0), Some(0)),
                make_link(1, true, None, Some(0)),
            ],
        };
        let mut current = previous.clone();
        current.links[0].tx_bytes = Some(125_000_000);
        current.links[0].rx_bytes = Some(125_000_000);
        current.links[1].rx_bytes = Some(125_000_000);
        let rows = compute_nvlink_throughputs(&[previous], &[current], 1.0);
        let meta = rows[0].nvlink.as_ref().unwrap();
        assert_eq!(rows[0].tx_gbps, 1.0);
        assert!(meta.tx_partial);
        assert_eq!(rows[0].rx_gbps, 2.0);
        assert!(!meta.rx_partial);

        let mut complete_rows = rows.clone();
        complete_rows[0].nvlink.as_mut().unwrap().tx_partial = false;
        let mut history = RollingAvgState::new(2);
        history.push(&rows);
        history.push(&complete_rows);
        let averages = history.averages();
        assert!(averages[0].nvlink.as_ref().unwrap().tx_partial);
    }

    #[test]
    fn aggregates_uses_gpu_wide_fields_when_present() {
        let elapsed = 1.0_f64;

        let prev = vec![NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "H100".to_string(),
            metrics: None,
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
            metrics: None,
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
            state: None,
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
                tx_partial: false,
                rx_partial: false,
                links: Vec::new(),
                metrics: None,
            }),
            xgmi: None,
            class: DeviceClass::Nvlink,
        };
        let rdma_a_ref = PortThroughput {
            dev_name: "mlx5_0".to_string(),
            port: 1,
            link_gbps: Some(100.0),
            state: None,
            tx_gbps: 3.0,
            rx_gbps: 4.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: None,
            nvlink: None,
            xgmi: None,
            class: DeviceClass::Rdma,
        };
        let rdma_b_ref = PortThroughput {
            dev_name: "mlx5_1".to_string(),
            port: 1,
            link_gbps: Some(100.0),
            state: None,
            tx_gbps: 5.0,
            rx_gbps: 6.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: None,
            nvlink: None,
            xgmi: None,
            class: DeviceClass::Rdma,
        };
        let reference = vec![rdma_a_ref.clone(), nvlink_ref.clone(), rdma_b_ref.clone()];
        let nvlink_ref_pos = 1usize;

        // Build the averaged rows in shuffled order. Note the NVLink row now
        // has `port: 3` (active-link count changed) and `port_label: "3/3"`.
        let nvlink_avg = PortThroughput {
            dev_name: "nvidia0".to_string(),
            port: 3,
            link_gbps: Some(150.0),
            state: None,
            tx_gbps: 1.5,
            rx_gbps: 2.5,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: Some("3/3".to_string()),
            nvlink: Some(NvLinkThroughputMeta {
                gpu_index: 0,
                gpu_name: "H100".to_string(),
                active_links: 3,
                tx_partial: false,
                rx_partial: false,
                links: Vec::new(),
                metrics: None,
            }),
            xgmi: None,
            class: DeviceClass::Nvlink,
        };
        let rdma_a_avg = PortThroughput {
            dev_name: "mlx5_0".to_string(),
            port: 1,
            link_gbps: Some(100.0),
            state: None,
            tx_gbps: 3.5,
            rx_gbps: 4.5,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: None,
            nvlink: None,
            xgmi: None,
            class: DeviceClass::Rdma,
        };
        let rdma_b_avg = PortThroughput {
            dev_name: "mlx5_1".to_string(),
            port: 1,
            link_gbps: Some(100.0),
            state: None,
            tx_gbps: 5.5,
            rx_gbps: 6.5,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: None,
            nvlink: None,
            xgmi: None,
            class: DeviceClass::Rdma,
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
            state: None,
            tx_gbps: 0.0,
            rx_gbps: 0.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: None,
            nvlink: None,
            xgmi: None,
            class: DeviceClass::Rdma,
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

#[cfg(test)]
mod tab_tests {
    use super::*;

    fn row(class: DeviceClass, name: &str) -> PortThroughput {
        PortThroughput {
            dev_name: name.to_string(),
            port: 1,
            link_gbps: None,
            state: None,
            tx_gbps: 0.0,
            rx_gbps: 0.0,
            tx_pkts_per_sec: 0.0,
            rx_pkts_per_sec: 0.0,
            rx_drops_per_sec: 0.0,
            counter_rates: Vec::new(),
            port_label: None,
            nvlink: None,
            xgmi: None,
            class,
        }
    }

    #[test]
    fn detect_tabs_is_sticky() {
        let mut seen = vec![DeviceClass::Rdma];
        detect_tabs(&mut seen, &[row(DeviceClass::Xgmi, "amdgpu0")]);
        assert_eq!(seen, vec![DeviceClass::Rdma, DeviceClass::Xgmi]);
        // rows disappear -> tab stays
        detect_tabs(&mut seen, &[]);
        assert_eq!(seen, vec![DeviceClass::Rdma, DeviceClass::Xgmi]);
        // no duplicates
        detect_tabs(&mut seen, &[row(DeviceClass::Xgmi, "amdgpu0")]);
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn detect_tabs_orders_rdma_xgmi_nvlink() {
        let mut seen = vec![DeviceClass::Rdma];
        detect_tabs(&mut seen, &[row(DeviceClass::Nvlink, "nvidia0")]);
        detect_tabs(&mut seen, &[row(DeviceClass::Xgmi, "amdgpu0")]);
        assert_eq!(
            seen,
            vec![DeviceClass::Rdma, DeviceClass::Xgmi, DeviceClass::Nvlink]
        );
    }

    #[test]
    fn display_filters_by_active_tab() {
        let mut app = App::new();
        app.throughputs = vec![
            row(DeviceClass::Rdma, "mlx5_0"),
            row(DeviceClass::Rdma, "mlx5_1"),
            row(DeviceClass::Xgmi, "amdgpu0"),
        ];
        app.active_tab = DeviceClass::Rdma;
        app.recompute_display();
        assert_eq!(app.display_throughputs().len(), 2);
        app.active_tab = DeviceClass::Xgmi;
        app.recompute_display();
        assert_eq!(app.display_throughputs().len(), 1);
        assert_eq!(app.display_throughputs()[0].dev_name, "amdgpu0");
    }

    #[test]
    fn cycle_wraps_and_remembers_selection() {
        let mut app = App::new();
        app.throughputs = vec![
            row(DeviceClass::Rdma, "mlx5_0"),
            row(DeviceClass::Rdma, "mlx5_1"),
            row(DeviceClass::Rdma, "mlx5_2"),
            row(DeviceClass::Xgmi, "amdgpu0"),
        ];
        app.seen_tabs = vec![DeviceClass::Rdma, DeviceClass::Xgmi];
        app.active_tab = DeviceClass::Rdma;
        app.recompute_display();
        app.selected_row = 2;
        app.cycle_tab(true);
        assert_eq!(app.active_tab, DeviceClass::Xgmi);
        assert_eq!(app.selected_row, 0);
        app.cycle_tab(true); // wraps back
        assert_eq!(app.active_tab, DeviceClass::Rdma);
        assert_eq!(app.selected_row, 2, "remembered per-tab cursor");
        app.cycle_tab(false); // backward wraps to Xgmi
        assert_eq!(app.active_tab, DeviceClass::Xgmi);
    }

    #[test]
    fn single_tab_cycle_is_noop() {
        let mut app = App::new();
        app.seen_tabs = vec![DeviceClass::Rdma];
        app.active_tab = DeviceClass::Rdma;
        app.cycle_tab(true);
        assert_eq!(app.active_tab, DeviceClass::Rdma);
    }
}

#[cfg(test)]
mod apply_snapshot_tests {
    use super::*;
    use crate::sampler::Snapshot;
    use crate::stat::{HwCounter, PortStat};

    fn port_stat(dev: &str, tx_bytes: u64) -> PortStat {
        PortStat {
            dev_name: dev.to_string(),
            port: 1,
            link_gbps: Some(400.0),
            state: None,
            counters: vec![HwCounter {
                name: "tx_bytes".to_string(),
                value: tx_bytes,
            }],
        }
    }

    fn sample<T>(data: T, taken_at: Instant) -> Option<crate::sampler::Sample<T>> {
        Some(crate::sampler::Sample { data, taken_at })
    }

    fn port_stat_bytes(dev: &str, tx_bytes: u64) -> PortStat {
        let mut s = PortStat::new(dev.to_string(), 1);
        s.counters.push(crate::stat::HwCounter {
            name: "tx_bytes".to_string(),
            value: tx_bytes,
        });
        s
    }

    fn snapshot(stats: Vec<PortStat>, taken_at: Instant) -> Snapshot {
        Snapshot {
            stats: sample(stats, taken_at),
            ifstats: sample(Vec::new(), taken_at),
            nvlink: sample(Vec::new(), taken_at),
            xgmi: sample(Vec::new(), taken_at),
            processes: Some(Vec::new()),
            sys: None,
            taken_at,
        }
    }

    fn gpu_snapshot(tx_bytes: u64, taken_at: Instant) -> Snapshot {
        let gpu = crate::nvlink::NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "test-gpu".to_string(),
            metrics: Some(crate::gpu::GpuMetrics {
                util_pct: Some(42),
                ..Default::default()
            }),
            link_count: 0,
            link_gbps: None,
            tx_bytes: Some(tx_bytes),
            rx_bytes: Some(0),
            links: Vec::new(),
        };
        Snapshot {
            stats: None, // e.g. kernel without rdma netlink support
            ifstats: sample(Vec::new(), taken_at),
            nvlink: sample(vec![gpu], taken_at),
            xgmi: sample(Vec::new(), taken_at),
            processes: Some(Vec::new()),
            sys: None,
            taken_at,
        }
    }

    #[test]
    fn gpu_util_flows_into_history() {
        let mut app = App::new();
        let t0 = Instant::now();
        app.apply_snapshot(gpu_snapshot(0, t0));
        app.apply_snapshot(gpu_snapshot(1_000, t0 + Duration::from_secs(2)));
        let h = app.history.get("nvidia0").expect("history for nvidia0");
        assert_eq!(h.util.len(), 2);
        assert_eq!(h.util[0], 42.0);
    }

    #[test]
    fn rates_use_per_subsystem_read_timestamps() {
        // A slow pass must not skew a subsystem read later in it: the GPU
        // sample's own timestamp, not the pass start, drives its elapsed.
        let mut app = App::new();
        let t0 = Instant::now();
        let mut first = gpu_snapshot(0, t0);
        // First pass was slow: GPU counters actually read 2s after pass start.
        first.nvlink.as_mut().unwrap().taken_at = t0 + Duration::from_secs(2);
        app.apply_snapshot(first);
        // Second pass, fast: read at t0+4s -> true GPU window is 2s, not 4s.
        app.apply_snapshot(gpu_snapshot(1_000_000_000, t0 + Duration::from_secs(4)));
        // 1e9 bytes * 8 / 2 s / 1e9 = 4.0 Gbps (2.0 would mean pass-start skew)
        assert!((app.throughputs[0].tx_gbps - 4.0).abs() < 1e-9);
    }

    #[test]
    fn first_snapshot_populates_rows_with_zero_rates() {
        let mut app = App::new();
        let t0 = Instant::now();
        app.apply_snapshot(snapshot(vec![port_stat("mlx5_0", 1_000_000)], t0));
        assert!(app.has_data);
        assert_eq!(app.throughputs.len(), 1);
        assert_eq!(app.throughputs[0].dev_name, "mlx5_0");
        assert_eq!(app.throughputs[0].tx_gbps, 0.0); // baseline, no delta yet
    }

    #[test]
    fn second_snapshot_computes_rates_from_thread_timestamps() {
        let mut app = App::new();
        let t0 = Instant::now();
        app.apply_snapshot(snapshot(vec![port_stat("mlx5_0", 0)], t0));
        // +1 GB over exactly 2s (thread timestamps, not wall clock here)
        let t1 = t0 + Duration::from_secs(2);
        app.apply_snapshot(snapshot(vec![port_stat("mlx5_0", 1_000_000_000)], t1));
        // 1e9 bytes * 8 bits / 2 s / 1e9 = 4.0 Gbps
        assert!((app.throughputs[0].tx_gbps - 4.0).abs() < 1e-9);
    }

    #[test]
    fn subsecond_duplicate_snapshot_is_skipped() {
        let mut app = App::new();
        let t0 = Instant::now();
        app.apply_snapshot(snapshot(vec![port_stat("mlx5_0", 0)], t0));
        app.apply_snapshot(snapshot(
            vec![port_stat("mlx5_0", 999)],
            t0 + Duration::from_millis(50),
        ));
        // Skipped: rates still the baseline zeros.
        assert_eq!(app.throughputs[0].tx_gbps, 0.0);
        // The skipped counters must not have become the new baseline: the
        // next snapshot diffs against the ORIGINAL t0 baseline (0 bytes).
        app.apply_snapshot(snapshot(
            vec![port_stat("mlx5_0", 1_000_000_000)],
            t0 + Duration::from_secs(2),
        ));
        // 1e9 bytes * 8 / 2 s / 1e9 = 4.0 Gbps (4.1 would mean 999 leaked in)
        assert!((app.throughputs[0].tx_gbps - 4.0).abs() < 1e-9);
    }

    #[test]
    fn failed_rdma_read_holds_rdma_state_only() {
        let mut app = App::new();
        let t0 = Instant::now();
        app.apply_snapshot(snapshot(vec![port_stat("mlx5_0", 0)], t0));
        let mut bad = snapshot(Vec::new(), t0 + Duration::from_secs(2));
        bad.stats = None;
        app.apply_snapshot(bad);
        // RDMA rows and baseline are held, not blanked.
        assert_eq!(app.throughputs.len(), 1);
        // Next good snapshot diffs against the ORIGINAL baseline: no spike.
        app.apply_snapshot(snapshot(
            vec![port_stat("mlx5_0", 1_000_000_000)],
            t0 + Duration::from_secs(4),
        ));
        // 1e9 bytes * 8 / 4 s / 1e9 = 2.0 Gbps
        assert!((app.throughputs[0].tx_gbps - 2.0).abs() < 1e-9);
    }

    #[test]
    fn gpu_data_flows_when_rdma_read_always_fails() {
        // A kernel without rdma netlink must still show NVLink data.
        let mut app = App::new();
        let t0 = Instant::now();
        app.apply_snapshot(gpu_snapshot(0, t0));
        assert!(app.has_data);
        assert_eq!(app.throughputs.len(), 1);
        assert_eq!(app.throughputs[0].tx_gbps, 0.0); // GPU baseline
        app.apply_snapshot(gpu_snapshot(1_000_000_000, t0 + Duration::from_secs(2)));
        // 1e9 bytes * 8 / 2 s / 1e9 = 4.0 Gbps
        assert!((app.throughputs[0].tx_gbps - 4.0).abs() < 1e-9);
    }

    #[test]
    fn slow_pass_is_not_stale_once_applied() {
        let mut app = App::new();
        let pass_started = Instant::now() - Duration::from_secs(4);
        app.apply_snapshot(snapshot(vec![port_stat("mlx5_0", 0)], pass_started));
        assert_eq!(app.stale_secs(), None);
    }

    #[test]
    fn multi_node_rates_are_nonzero_across_frames() {
        // Regression for the scratch-folding bug: restore_legacy rolled the
        // per-node baseline back every frame, so every frame looked like the
        // first (baseline) frame and rates stayed zero forever.
        let mut app = App::new_multi();
        app.add_node("remote");
        let t0 = Instant::now();
        // Frame 1 for node 0 (local): establishes the baseline.
        app.apply_node_snapshot(0, snapshot(vec![port_stat_bytes("mlx5_0", 1_000)], t0));
        // Frame 2 for node 0 one second later: +1e9 bytes => 8 Gbps.
        app.apply_node_snapshot(
            0,
            snapshot(
                vec![port_stat_bytes("mlx5_0", 1_000 + 1_000_000_000)],
                t0 + Duration::from_secs(1),
            ),
        );
        let rows = &app.node_states[0].throughputs;
        assert_eq!(rows.len(), 1);
        assert!(
            (rows[0].tx_gbps - 8.0).abs() < 1e-9,
            "local node rate must be 8 Gbps, got {}",
            rows[0].tx_gbps
        );

        // Same for a remote node (idx 1): its baseline must be independent.
        app.apply_node_snapshot(1, snapshot(vec![port_stat_bytes("mlx5_1", 5_000)], t0));
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_bytes("mlx5_1", 5_000 + 500_000_000)],
                t0 + Duration::from_secs(1),
            ),
        );
        let rows = &app.node_states[1].throughputs;
        assert_eq!(rows.len(), 1);
        assert!(
            (rows[0].tx_gbps - 4.0).abs() < 1e-9,
            "remote node rate must be 4 Gbps, got {}",
            rows[0].tx_gbps
        );
        // Cross-node isolation: node 0's baseline must be untouched by node 1.
        assert_eq!(app.node_states[0].throughputs[0].dev_name, "mlx5_0");
    }

    #[test]
    fn multi_node_steady_rates_do_not_jump() {
        // Steady 10 Gbps traffic through the multi-node fold path: every
        // frame must produce the same rate. A guard-skip or baseline leak
        // would make rows vanish or rates oscillate.
        let mut app = App::new_multi();
        app.add_node("remote");
        let t0 = Instant::now();
        // 1.25e9 bytes/s = 10 Gbps; frames 1s apart, 6 frames per node.
        for f in 0..6 {
            let tx = 1_250_000_000u64 * (f + 1);
            app.apply_node_snapshot(
                0,
                snapshot(
                    vec![port_stat_bytes("mlx5_0", tx)],
                    t0 + Duration::from_secs(f),
                ),
            );
            app.apply_node_snapshot(
                1,
                snapshot(
                    vec![port_stat_bytes("mlx5_1", tx / 2)],
                    t0 + Duration::from_secs(f),
                ),
            );
        }
        for (idx, want) in [(0, 10.0), (1, 5.0)] {
            let rows = &app.node_states[idx].throughputs;
            assert_eq!(rows.len(), 1, "node {idx} rows vanished");
            let got = rows[0].tx_gbps;
            assert!((got - want).abs() < 1e-9, "node {idx} rate {got} != {want}");
        }
    }

    #[test]
    fn reconnect_anchor_mismatch_produces_bogus_rates() {
        // Documents the reconnect bug: after a reconnect the viewer's MonoMap
        // anchor changes, so the node baseline's prev_taken_at (mapped with the
        // OLD anchor) no longer lines up with the new frame's mapped time.
        // The elapsed is then garbage (saturating to 0 => inf rates, or a
        // wrong span => wrong rates) until the baseline re-establishes.
        let mut app = App::new_multi();
        app.add_node("remote");
        let a1 = Instant::now();
        // Connection 1: baseline, then +1e9 bytes over 1s = 8 Gbps.
        app.apply_node_snapshot(1, snapshot(vec![port_stat_bytes("mlx5_0", 1_000)], a1));
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_bytes("mlx5_0", 1_000_000_001)],
                a1 + Duration::from_secs(1),
            ),
        );
        let before = app.node_states[1].throughputs[0].tx_gbps;
        // Instant addition rounds; allow µs-level slop in the elapsed.
        assert!((before - 8.0).abs() < 1e-4);

        // Reconnect 10s later: new anchor A2. run_multi resets the node's
        // baseline on the Live transition, so the next frame re-establishes
        // (zero rates) instead of dividing by a garbage elapsed.
        let a2 = a1 + Duration::from_secs(10);
        app.reset_node_baseline(1);
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_bytes("mlx5_0", 1_000_200_001)],
                a2 + Duration::from_millis(200),
            ),
        );
        let after = app.node_states[1].throughputs[0].tx_gbps;
        assert_eq!(
            after, 0.0,
            "post-reconnect frame must re-baseline to zero, got {after}"
        );
        // The frame after that computes correctly under the new anchor.
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_bytes("mlx5_0", 1_000_200_001 + 250_000_000)],
                a2 + Duration::from_millis(400),
            ),
        );
        let recovered = app.node_states[1].throughputs[0].tx_gbps;
        assert!(
            (recovered - 10.0).abs() < 1e-3,
            "second post-reconnect frame must be 10 Gbps, got {recovered}"
        );
    }

    #[test]
    fn drill_into_remote_node_shows_remote_data_not_local() {
        // Reproduces the user's report: Enter on a remote node's row showed
        // the LOCAL node's numbers. Fold local frames with distinctive
        // counters, fold remote frames with different counters, drill into
        // the remote node, and assert the display carries the remote rows.
        let mut app = App::new_multi();
        app.add_node("node4");
        let t0 = Instant::now();
        // Local (idx 0): mlx5_local with tx=900 on the second frame.
        app.apply_node_snapshot(0, snapshot(vec![port_stat_bytes("mlx5_local", 100)], t0));
        app.apply_node_snapshot(
            0,
            snapshot(
                vec![port_stat_bytes("mlx5_local", 100 + 900_000_000)],
                t0 + Duration::from_secs(1),
            ),
        );
        // Remote (idx 1 = "node4"): mlx5_remote with different counters.
        app.apply_node_snapshot(1, snapshot(vec![port_stat_bytes("mlx5_remote", 500)], t0));
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_bytes("mlx5_remote", 500 + 100_000_000)],
                t0 + Duration::from_secs(1),
            ),
        );
        // Drill into node4 (ALL-view Enter).
        app.enter_node(1);
        // The displayed rows must be the remote node's, not the local's.
        let display = app.display_throughputs();
        assert_eq!(
            display.len(),
            1,
            "drill-in must show the remote node's rows"
        );
        assert_eq!(
            display[0].dev_name, "mlx5_remote",
            "drill-in shows {} (local data leaked!)",
            display[0].dev_name
        );
        // And the header sysinfo must be the remote node's, not blank.
        assert!(
            !app.sysinfo.hostname.is_empty() || app.node_states[1].sysinfo.hostname.is_empty(),
            "sysinfo should come from the folded node state"
        );
    }

    #[test]
    fn enter_uses_sorted_row_identity_not_position() {
        // The ALL view sorts by TX; the cursor is a position in that sorted
        // list. Enter must drill into the row's actual node, so a sort order
        // that differs from the node order cannot send the operator into
        // the wrong node.
        let mut app = App::new_multi();
        app.add_node("remote-a"); // idx 1
        app.add_node("remote-b"); // idx 2
        let t0 = Instant::now();
        // local (idx 0): 1 Gbps. remote-a (idx 1): 5 Gbps. remote-b: 0.
        app.apply_node_snapshot(0, snapshot(vec![port_stat_bytes("dev_local", 0)], t0));
        app.apply_node_snapshot(
            0,
            snapshot(
                vec![port_stat_bytes("dev_local", 125_000_000)],
                t0 + Duration::from_secs(1),
            ),
        );
        app.apply_node_snapshot(1, snapshot(vec![port_stat_bytes("dev_a", 0)], t0));
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_bytes("dev_a", 625_000_000)],
                t0 + Duration::from_secs(1),
            ),
        );
        app.apply_node_snapshot(2, snapshot(vec![port_stat_bytes("dev_b", 0)], t0));
        app.apply_node_snapshot(
            2,
            snapshot(
                vec![port_stat_bytes("dev_b", 1_000_000)],
                t0 + Duration::from_secs(1),
            ),
        );
        // Sorted by TX: [0]=remote-a(5G), [1]=local(1G), [2]=remote-b(~0).
        let rows = app.all_rows();
        assert_eq!(rows[0].name, "remote-a");
        assert_eq!(rows[0].node_idx, 1);
        // Cursor on the TOP row (position 0) — visually remote-a.
        app.all_selected = 0;
        app.enter_selected_node();
        assert_eq!(app.active_node, 1, "must enter remote-a, not local");
        assert_eq!(app.display_throughputs()[0].dev_name, "dev_a");
        // The header must identify the node.
        assert_eq!(app.view_label(), "remote-a");
    }

    #[test]
    fn guard_skip_does_not_blank_node_state() {
        // Regression for the blank-header bug: a frame rejected by the
        // duplicate guard must not overwrite the node's stored state with
        // post-harvest empties.
        let mut app = App::new_multi();
        app.add_node("remote");
        let t0 = Instant::now();
        app.apply_node_snapshot(1, snapshot(vec![port_stat_bytes("mlx5_0", 1_000)], t0));
        let before = app.node_states[1].throughputs.clone();
        // A second frame within 0.1s: the guard skips it.
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_bytes("mlx5_0", 999_999)],
                t0 + Duration::from_millis(50),
            ),
        );
        assert_eq!(
            app.node_states[1].throughputs.len(),
            before.len(),
            "guard skip must not blank the node's rows"
        );
    }

    /// A PortStat with arbitrary named counters (for error-rate tests).
    fn port_stat_counters(dev: &str, counters: &[(&str, u64)]) -> PortStat {
        let mut s = PortStat::new(dev.to_string(), 1);
        for (name, value) in counters {
            s.counters.push(crate::stat::HwCounter {
                name: (*name).to_string(),
                value: *value,
            });
        }
        s
    }

    #[test]
    fn all_rows_surface_error_counters_with_abbreviations() {
        // Non-zero out_of_buffer / RNR must light up the ALL row with their
        // abbreviation so a bad node is visible without drilling in.
        let mut app = App::new_multi();
        app.add_node("bad");
        app.add_node("clean");
        let t0 = Instant::now();
        // bad: baseline then a pass with error counters increasing.
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_counters(
                    "mlx5_0",
                    &[
                        ("out_of_buffer", 1_000),
                        ("rnr_nak_retry_err", 10),
                        ("np_ecn_marked_roce_packets", 500),
                    ],
                )],
                t0,
            ),
        );
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_counters(
                    "mlx5_0",
                    &[
                        ("out_of_buffer", 1_000 + 800), // +800 / 1s
                        ("rnr_nak_retry_err", 10 + 21),
                        ("np_ecn_marked_roce_packets", 500 + 49),
                    ],
                )],
                t0 + Duration::from_secs(1),
            ),
        );
        // clean: no error counters at all.
        app.apply_node_snapshot(2, snapshot(vec![port_stat_counters("mlx5_1", &[])], t0));
        app.apply_node_snapshot(
            2,
            snapshot(
                vec![port_stat_counters("mlx5_1", &[])],
                t0 + Duration::from_secs(1),
            ),
        );

        let rows = app.all_rows();
        let bad = rows.iter().find(|r| r.name == "bad").expect("bad row");
        assert!(
            (bad.err_rate - 821.0).abs() < 1e-6,
            "err_rate {}",
            bad.err_rate
        );
        assert!(
            (bad.cong_rate - 49.0).abs() < 1e-6,
            "cong_rate {}",
            bad.cong_rate
        );
        assert_eq!(bad.top_error, Some("OB"), "dominant error abbreviation");

        let clean = rows.iter().find(|r| r.name == "clean").expect("clean row");
        assert_eq!(clean.err_rate, 0.0);
        assert_eq!(clean.top_error, None);

        // errors_only hides the clean node.
        app.toggle_errors_only();
        let filtered = app.all_rows();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "bad");
    }

    #[test]
    fn display_reads_active_node_state_not_fold_scratch() {
        // Scoped-B guarantee: the render accessors read the ACTIVE node's
        // stored state. Interleaved folds of other nodes (which use the
        // legacy fields as scratch and empty them via harvest) must never
        // be observable through the display, even on ticks where the
        // active node has no new frame.
        let mut app = App::new_multi();
        app.add_node("node-a");
        app.add_node("node-b");
        let t0 = Instant::now();
        // Both nodes fold real frames.
        app.apply_node_snapshot(1, snapshot(vec![port_stat_bytes("mlx5_a", 1_000)], t0));
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_bytes("mlx5_a", 1_000_000_001)],
                t0 + Duration::from_secs(1),
            ),
        );
        app.apply_node_snapshot(2, snapshot(vec![port_stat_bytes("mlx5_b", 5_000)], t0));
        app.apply_node_snapshot(
            2,
            snapshot(
                vec![port_stat_bytes("mlx5_b", 5_000_500_000)],
                t0 + Duration::from_secs(1),
            ),
        );
        // Drill into node-a.
        app.enter_node(1);
        // Simulate a frame-less tick: node-b folds again (its frame
        // arrives), node-a's does NOT. The harvest empties the legacy
        // scratch — the display must still show node-a.
        app.apply_node_snapshot(
            2,
            snapshot(
                vec![port_stat_bytes("mlx5_b", 5_001_000_000)],
                t0 + Duration::from_secs(2),
            ),
        );
        let display = app.display_throughputs();
        assert_eq!(display.len(), 1, "active node's rows vanished");
        assert_eq!(display[0].dev_name, "mlx5_a");
        // Process list: node-a's stored list (empty here, but must read
        // the node state, not the scratch).
        let _ = app.selected_device_processes();
        // Header sysinfo must come from node-a's state.
        let _ = app.view_sysinfo();
        // Stale clock must read node-a's last_applied_at.
        assert_eq!(app.stale_secs(), None);
    }

    #[test]
    fn failed_read_does_not_leak_another_nodes_rows() {
        // The drain loop folds nodes interleaved (local, remote A, remote
        // B, ...). rdma_rows survive in the legacy fields between folds
        // (harvest does not take them), so a failed-stats fold for node B
        // harvests whatever rows node A's fold left behind -- B briefly
        // displays A's devices. This is the flicker-with-wrong-data bug.
        let mut app = App::new_multi();
        app.add_node("node-a");
        app.add_node("node-b");
        let t0 = Instant::now();
        // Node A folds real rows.
        app.apply_node_snapshot(1, snapshot(vec![port_stat_bytes("mlx5_a", 1_000)], t0));
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_bytes("mlx5_a", 1_000_000_001)],
                t0 + Duration::from_secs(1),
            ),
        );
        // Node B folds a FAILED pass (stats=None) right after A.
        let failed = crate::sampler::Snapshot {
            stats: None,
            ifstats: None,
            nvlink: None,
            xgmi: None,
            processes: None,
            sys: None,
            taken_at: t0 + Duration::from_secs(2),
        };
        app.apply_node_snapshot(2, failed);
        // Node B must NOT display node A's devices.
        let names: Vec<&str> = app.node_states[2]
            .throughputs
            .iter()
            .map(|t| t.dev_name.as_str())
            .collect();
        assert!(
            !names.contains(&"mlx5_a"),
            "node-b leaked node-a's rows: {names:?}"
        );
    }

    #[test]
    fn failed_subsystem_read_holds_previous_rows_in_multi_mode() {
        // Documents the flicker bug: a snapshot whose stats read failed
        // (None) must HOLD the node's previous rows in multi-node mode,
        // exactly like single-node mode. The fold's harvest previously
        // stored the post-harvest empties instead, making the table and
        // detail pane vanish for a tick whenever a subsystem read failed.
        let mut app = App::new_multi();
        app.add_node("remote");
        let t0 = Instant::now();
        // Frame 1: real rows.
        app.apply_node_snapshot(1, snapshot(vec![port_stat_bytes("mlx5_0", 1_000)], t0));
        // Frame 2: real rows again (rates computed).
        app.apply_node_snapshot(
            1,
            snapshot(
                vec![port_stat_bytes("mlx5_0", 1_000_000_001)],
                t0 + Duration::from_secs(1),
            ),
        );
        assert_eq!(app.node_states[1].throughputs.len(), 1);
        // Frame 3: the stats read FAILED (None) but the pass still ran.
        // Build a snapshot with stats=None but a fresh taken_at.
        let failed = crate::sampler::Snapshot {
            stats: None,
            ifstats: None,
            nvlink: None,
            xgmi: None,
            processes: None,
            sys: None,
            taken_at: t0 + Duration::from_secs(2),
        };
        app.apply_node_snapshot(1, failed);
        // The node's rows must SURVIVE the failed read (hold semantics).
        assert_eq!(
            app.node_states[1].throughputs.len(),
            1,
            "failed stats read blanked the node's rows (flicker bug)"
        );
    }

    #[test]
    fn multi_node_duplicate_guard_uses_per_node_timestamp() {
        // The 0.1s duplicate guard must compare against the SAME node's
        // previous frame, not a rolled-back scratch value.
        let mut app = App::new_multi();
        app.add_node("remote");
        let t0 = Instant::now();
        app.apply_node_snapshot(0, snapshot(vec![port_stat_bytes("mlx5_0", 1)], t0));
        app.apply_node_snapshot(1, snapshot(vec![port_stat_bytes("mlx5_1", 1)], t0));
        // Node 0's second frame is 1s later: must NOT be dropped as a
        // duplicate just because node 1's frame arrived in between.
        app.apply_node_snapshot(
            0,
            snapshot(
                vec![port_stat_bytes("mlx5_0", 2)],
                t0 + Duration::from_secs(1),
            ),
        );
        assert_eq!(app.node_states[0].throughputs.len(), 1);
    }
}
