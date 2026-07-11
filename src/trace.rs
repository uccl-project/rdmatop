//! Perfetto trace export. Records per-interval RDMA metrics and serializes them
//! to the Chrome JSON "trace event" format, loadable in https://ui.perfetto.dev.

use std::io;
use std::time::Instant;

/// One device/port's metric values at a single sample instant.
#[derive(Clone, Debug)]
pub struct PortMetrics {
    pub dev_name: String,
    pub port: u32,
    pub tx_gbps: f64,
    pub rx_gbps: f64,
    pub tx_pps: f64,
    pub rx_pps: f64,
    pub rx_drops_per_sec: f64,
}

/// One interval snapshot: a timestamp plus every port's metrics.
#[derive(Clone, Debug)]
pub struct TraceSample {
    pub ts_us: u64,
    pub ports: Vec<PortMetrics>,
}

/// Accumulates samples in memory while recording; flush to a Chrome JSON file.
pub struct Recorder {
    start: Instant,
    samples: Vec<TraceSample>,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            samples: Vec::new(),
        }
    }

    /// Record one interval's metrics, timestamped relative to record start.
    /// `taken_at` is when the counters were read (sampler-side), so trace
    /// spacing is immune to UI queue and event-poll latency; a sample taken
    /// before recording started saturates to ts 0.
    pub fn push(&mut self, taken_at: Instant, ports: Vec<PortMetrics>) {
        let ts_us = taken_at.saturating_duration_since(self.start).as_micros() as u64;
        self.samples.push(TraceSample { ts_us, ports });
    }

    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    pub fn elapsed_secs(&self) -> u64 {
        self.start.elapsed().as_secs()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Serialize the buffer and write it to `path`.
    pub fn write_to(&self, path: &str) -> io::Result<()> {
        std::fs::write(path, to_chrome_json(&self.samples))
    }
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

/// Serialize samples to Chrome JSON. Each device/port becomes a Perfetto
/// "process" (via a process_name metadata event); each metric/direction becomes
/// its own counter track.
pub fn to_chrome_json(samples: &[TraceSample]) -> String {
    let order = device_order(samples);
    let pid_for = |p: &PortMetrics| pid_of(&order, &p.dev_name, p.port);

    let metadata = order
        .iter()
        .enumerate()
        .map(|(i, (dev, port))| process_name_event(i + 1, dev, *port));
    let counters = samples.iter().flat_map(|s| {
        s.ports
            .iter()
            .flat_map(|p| counter_events(pid_for(p), s.ts_us, p))
    });

    let events: Vec<String> = metadata.chain(counters).collect();
    format!(
        "{{\"displayTimeUnit\":\"ns\",\"traceEvents\":[{}]}}",
        events.join(",")
    )
}

/// Collect every (dev, port) in first-seen order; the index is its Perfetto pid.
fn device_order(samples: &[TraceSample]) -> Vec<(String, u32)> {
    let mut order: Vec<(String, u32)> = Vec::new();
    let keys = samples.iter().flat_map(|s| s.ports.iter());
    for p in keys {
        let key = (p.dev_name.clone(), p.port);
        if !order.contains(&key) {
            order.push(key);
        }
    }
    order
}

fn pid_of(order: &[(String, u32)], dev: &str, port: u32) -> usize {
    order
        .iter()
        .position(|(d, p)| d == dev && *p == port)
        .unwrap_or(0)
        + 1
}

/// The process_name metadata event labeling a pid as "<device>:<port>".
fn process_name_event(pid: usize, dev: &str, port: u32) -> String {
    format!(
        r#"{{"name":"process_name","ph":"M","pid":{},"args":{{"name":"{}:{}"}}}}"#,
        pid,
        escape(dev),
        port
    )
}

/// The counter tracks emitted per port per sample. Each direction is its own
/// track so Perfetto renders it as an independent line rather than a stack.
fn counter_events(pid: usize, ts_us: u64, p: &PortMetrics) -> [String; 5] {
    [
        counter(pid, ts_us, "tx_gbps", p.tx_gbps),
        counter(pid, ts_us, "rx_gbps", p.rx_gbps),
        counter(pid, ts_us, "tx_pps", p.tx_pps),
        counter(pid, ts_us, "rx_pps", p.rx_pps),
        counter(pid, ts_us, "rx_drops_per_sec", p.rx_drops_per_sec),
    ]
}

/// Build a single-value Chrome JSON counter ("C") event.
fn counter(pid: usize, ts_us: u64, name: &str, value: f64) -> String {
    format!(
        r#"{{"name":"{}","ph":"C","pid":{},"ts":{},"args":{{"value":{}}}}}"#,
        name,
        pid,
        ts_us,
        json_num(value)
    )
}

/// Format an f64 as valid JSON, coercing NaN/Inf (not valid JSON) to 0.
fn json_num(v: f64) -> String {
    if v.is_finite() {
        format!("{}", v)
    } else {
        "0".to_string()
    }
}

/// Escape a string for embedding in a JSON string literal.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
