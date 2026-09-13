//! RDMA recording: cumulative CSV counters from the command line and
//! per-interval Perfetto metrics from the TUI.

use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::time::{Duration, Instant};

use crate::stat::{CounterReader, PortStat};

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
    /// `taken_at` is the sampling pass start (sampler-side), so trace spacing
    /// is immune to UI queue and event-poll latency; a sample taken before
    /// recording started saturates to ts 0.
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

const COUNTERS: &[&str] = &[
    "tx_bytes",
    "rx_bytes",
    "tx_pkts",
    "rx_pkts",
    "rx_drops",
    "rdma_write_bytes",
    "rdma_write_recv_bytes",
    "rdma_write_wrs",
    "retrans_bytes",
    "retrans_pkts",
    "retrans_timeout_events",
];

pub const HELP: &str = "\
Usage: rdmatop
       rdmatop --format csv --output PATH --device NAME [--port N]
               [--interval MS] [--duration SECONDS]

Record cumulative RDMA counters without the TUI or GPU/process scans.
Command-line recording supports CSV. Press r in the TUI for a Perfetto trace.
Defaults: port 1, interval 1 ms, duration 10 seconds.
Fractional intervals are accepted: --interval 0.1 requests 0.1 ms.
Driver latency limits the achieved rate.
CSV poll_start_ns/poll_end_ns use CLOCK_MONOTONIC on this host.
The output file must not already exist. Run one recorder per port to
sample ports independently. Duration must be positive.
";

pub struct Options {
    path: String,
    device: String,
    port: u32,
    interval: Duration,
    duration: Duration,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn seconds(value: &str, scale: f64) -> io::Result<Duration> {
    let number = value
        .parse::<f64>()
        .map_err(|_| invalid(format!("invalid duration: {value}")))?;
    if !number.is_finite() || number <= 0.0 {
        return Err(invalid(format!("invalid duration: {value}")));
    }
    let duration = Duration::try_from_secs_f64(number * scale)
        .map_err(|_| invalid(format!("duration out of range: {value}")))?;
    if duration.is_zero() {
        return Err(invalid("duration must be at least 1 ns"));
    }
    Ok(duration)
}

pub fn parse(args: &[String]) -> io::Result<Options> {
    let mut format = None;
    let mut path = None;
    let mut device = None;
    let mut port = 1;
    let mut interval = Duration::from_millis(1);
    let mut duration = Duration::from_secs(10);
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| invalid(format!("missing value for {flag}")))?;
        match flag.as_str() {
            "--format" => format = Some(value.as_str()),
            "--output" => path = Some(value.clone()),
            "--device" => device = Some(value.clone()),
            "--port" => port = value.parse().map_err(|_| invalid("invalid port"))?,
            "--interval" => interval = seconds(value, 0.001)?,
            "--duration" => duration = seconds(value, 1.0)?,
            _ => return Err(invalid(format!("unknown option: {flag}"))),
        }
    }
    match format {
        Some("csv") => {}
        Some(value) => {
            return Err(invalid(format!(
                "unsupported format: {value} (expected csv)"
            )))
        }
        None => return Err(invalid("--format is required")),
    }
    if port == 0 {
        return Err(invalid("port must be positive"));
    }
    Ok(Options {
        path: path
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("--output is required"))?,
        device: device
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("--device is required"))?,
        port,
        interval,
        duration,
    })
}

fn monotonic_ns() -> io::Result<u64> {
    let mut value: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(value.tv_sec as u64 * 1_000_000_000 + value.tv_nsec as u64)
}

fn write_row(out: &mut impl Write, start: u64, end: u64, stat: &PortStat) -> io::Result<()> {
    write!(out, "{start},{end}")?;
    for name in COUNTERS {
        write!(out, ",")?;
        if let Some(value) = stat.counter_value(name) {
            write!(out, "{value}")?;
        }
    }
    writeln!(out)
}

// Skip missed deadlines: slow reads must not trigger a burst of catch-up queries.
fn next_delay(interval: Duration, elapsed: Duration) -> Duration {
    interval.saturating_sub(elapsed)
}

pub fn run(options: Options) -> io::Result<()> {
    let mut reader = CounterReader::open(&options.device, options.port)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&options.path)?;
    let mut out = BufWriter::with_capacity(64 * 1024, file);
    writeln!(out, "poll_start_ns,poll_end_ns,{}", COUNTERS.join(","))?;
    out.flush()?;
    let started = Instant::now();
    let mut samples = 0u64;
    let mut read_time = Duration::ZERO;
    let mut overruns = 0u64;
    loop {
        if samples > 0 && started.elapsed() >= options.duration {
            break;
        }
        let tick = Instant::now();
        let start = monotonic_ns()?;
        let stat = reader.read()?;
        let end = monotonic_ns()?;
        read_time += Duration::from_nanos(end - start);
        samples += 1;
        write_row(&mut out, start, end, &stat)?;
        let elapsed = tick.elapsed();
        overruns += u64::from(elapsed > options.interval);
        if started.elapsed() >= options.duration {
            break;
        }
        let delay = next_delay(options.interval, elapsed)
            .min(options.duration.saturating_sub(started.elapsed()));
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
    }
    out.flush()?;
    eprintln!(
        "{}/{}: {} samples in {:.3}s, mean query {:.3}ms, {} interval overruns",
        options.device,
        options.port,
        samples,
        started.elapsed().as_secs_f64(),
        read_time.as_secs_f64() * 1000.0 / samples as f64,
        overruns
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stat::HwCounter;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn recording_defaults() {
        let options = parse(&args("--format csv --output out.csv --device efa0")).unwrap();
        assert_eq!(options.path, "out.csv");
        assert_eq!(options.device, "efa0");
        assert_eq!(options.interval, Duration::from_millis(1));
        assert_eq!(options.duration, Duration::from_secs(10));
        assert_eq!(options.port, 1);
    }

    #[test]
    fn fractional_interval_uses_milliseconds() {
        let options = parse(&args(
            "--format csv --output out.csv --device efa0 --interval 0.1 --duration 0.5 --port 2",
        ))
        .unwrap();
        assert_eq!(options.interval, Duration::from_micros(100));
        assert_eq!(options.duration, Duration::from_millis(500));
        assert_eq!(options.port, 2);
    }

    #[test]
    fn reject_invalid_recording_options() {
        for suffix in [
            "--interval 0",
            "--interval NaN",
            "--interval inf",
            "--interval -1",
            "--interval 1e-50",
            "--interval 1e100",
            "--duration -1",
            "--duration 0",
            "--duration NaN",
            "--duration inf",
            "--duration 1e-50",
            "--port 0",
            "--port invalid",
            "--format json",
            "--unknown 1",
            "--interval",
            "--format",
            "--output",
        ] {
            assert!(
                parse(&args(&format!(
                    "--format csv --output out.csv --device efa0 {suffix}"
                )))
                .is_err(),
                "accepted {suffix}"
            );
        }
        for command in [
            "--output out.csv --device efa0",
            "--format csv --device efa0",
            "--format csv --output out.csv",
        ] {
            assert!(parse(&args(command)).is_err(), "accepted {command}");
        }
        for option in ["--output", "--device", "--format"] {
            let mut arguments = args("--format csv --output out.csv --device efa0");
            arguments.extend([option.to_string(), String::new()]);
            assert!(parse(&arguments).is_err(), "accepted empty {option}");
        }
    }

    #[test]
    fn slow_query_does_not_add_another_full_interval() {
        let interval = Duration::from_millis(1);
        assert_eq!(
            next_delay(interval, Duration::from_micros(300)),
            Duration::from_micros(700)
        );
        assert_eq!(
            next_delay(interval, Duration::from_millis(2)),
            Duration::ZERO
        );
    }

    #[test]
    fn absent_counter_is_empty_instead_of_zero() {
        let stat = PortStat {
            dev_name: "efa0".into(),
            port: 1,
            link_gbps: None,
            state: None,
            counters: vec![HwCounter {
                name: "tx_bytes".into(),
                value: 42,
            }],
        };
        let mut row = Vec::new();
        write_row(&mut row, 100, 200, &stat).unwrap();
        let row = String::from_utf8(row).unwrap();
        let fields: Vec<_> = row.trim_end().split(',').collect();
        assert_eq!(fields.len(), 2 + COUNTERS.len());
        assert_eq!(&fields[..4], &["100", "200", "42", ""]);
    }

    #[test]
    #[ignore = "requires RDMATOP_TEST_DEVICE to name an RDMA port"]
    fn recording_counter_reader_smoke() {
        let device = std::env::var("RDMATOP_TEST_DEVICE").expect("set RDMATOP_TEST_DEVICE");
        let mut reader = CounterReader::open(&device, 1).unwrap();
        let stat = reader.read().unwrap();
        assert_eq!(stat.dev_name, device);
        assert!(stat.counter_value("tx_bytes").is_some());
    }
}
