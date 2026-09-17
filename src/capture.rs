use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::metrics::rates_between;
use crate::stat::{self, PortStat};
use crate::trace::PortMetrics;

#[derive(Clone, Debug)]
pub struct CaptureSample {
    pub ts_unix_ns: u64,
    pub ports: Vec<PortMetrics>,
}

#[derive(Debug, Default)]
pub struct CaptureResult {
    pub samples: Vec<CaptureSample>,
    pub error: Option<String>,
}

pub(crate) type StatReader = Box<dyn FnMut() -> io::Result<Vec<PortStat>> + Send>;

pub const MAX_SAMPLES: usize = 100_000;
pub const MAX_CONSECUTIVE_READ_FAILURES: u32 = 10;
const STOP_POLL: Duration = Duration::from_millis(20);
const THREAD_NAME: &str = "rdmatop-capture";

pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<CaptureResult>>,
}

impl Capture {
    pub fn start(interval: Duration, devices: Option<Vec<String>>) -> io::Result<Self> {
        Self::start_with_reader(Box::new(stat::read_all_stats), interval, devices)
    }

    pub(crate) fn start_with_reader(
        read: StatReader,
        interval: Duration,
        devices: Option<Vec<String>>,
    ) -> io::Result<Self> {
        let mut sampling = SamplingLoop::from_baseline(read, interval, devices)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name(THREAD_NAME.to_string())
            .spawn(move || sampling.run(&thread_stop))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub fn stop(mut self) -> CaptureResult {
        self.stop.store(true, Ordering::Relaxed);
        let Some(thread) = self.thread.take() else {
            return panicked();
        };
        thread.join().unwrap_or_else(|_| panicked())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn panicked() -> CaptureResult {
    CaptureResult {
        samples: Vec::new(),
        error: Some("capture thread panicked".to_string()),
    }
}

struct SamplingLoop {
    read: StatReader,
    prev: Vec<PortStat>,
    prev_at: Instant,
    prev_unix_ns: u64,
    interval: Duration,
    devices: Option<Vec<String>>,
    samples: Vec<CaptureSample>,
    consecutive_failures: u32,
}

impl SamplingLoop {
    fn from_baseline(
        mut read: StatReader,
        interval: Duration,
        devices: Option<Vec<String>>,
    ) -> io::Result<Self> {
        let prev = keep_devices(read()?, devices.as_deref());
        let prev_at = Instant::now();
        let prev_unix_ns = unix_ns();
        if prev.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no RDMA port matches the device filter",
            ));
        }
        Ok(Self {
            read,
            prev,
            prev_at,
            prev_unix_ns,
            interval,
            devices,
            samples: Vec::new(),
            consecutive_failures: 0,
        })
    }

    fn run(&mut self, stop: &AtomicBool) -> CaptureResult {
        let error = self.sample_until_stopped(stop);
        CaptureResult {
            samples: std::mem::take(&mut self.samples),
            error,
        }
    }

    fn sample_until_stopped(&mut self, stop: &AtomicBool) -> Option<String> {
        let mut due = self.prev_at + self.interval;
        loop {
            sleep_until(due, stop);
            if stop.load(Ordering::Relaxed) {
                return None;
            }
            if let Err(message) = self.read_once() {
                return Some(message);
            }
            if self.samples.len() >= MAX_SAMPLES {
                return Some("sample cap reached".to_string());
            }
            due = next_due(due, self.interval);
        }
    }

    fn read_once(&mut self) -> Result<(), String> {
        let stats = match (self.read)() {
            Ok(stats) => stats,
            Err(error) => return self.record_failure(error),
        };
        self.consecutive_failures = 0;
        self.record_sample(keep_devices(stats, self.devices.as_deref()));
        Ok(())
    }

    fn record_failure(&mut self, error: io::Error) -> Result<(), String> {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MAX_CONSECUTIVE_READ_FAILURES {
            return Err(error.to_string());
        }
        Ok(())
    }

    fn record_sample(&mut self, curr: Vec<PortStat>) {
        let now = Instant::now();
        let now_unix_ns = unix_ns();
        let elapsed = now.duration_since(self.prev_at).as_secs_f64();
        self.samples.push(CaptureSample {
            ts_unix_ns: self.prev_unix_ns,
            ports: rates_between(&self.prev, &curr, elapsed),
        });
        self.prev = curr;
        self.prev_at = now;
        self.prev_unix_ns = now_unix_ns;
    }
}

fn sleep_until(due: Instant, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        let remaining = due.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        std::thread::sleep(remaining.min(STOP_POLL));
    }
}

fn next_due(due: Instant, interval: Duration) -> Instant {
    (due + interval).max(Instant::now())
}

fn keep_devices(stats: Vec<PortStat>, devices: Option<&[String]>) -> Vec<PortStat> {
    let Some(names) = devices else {
        return stats;
    };
    stats
        .into_iter()
        .filter(|stat| names.contains(&stat.dev_name))
        .collect()
}

pub(crate) fn unix_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) fn growing_reader(devices: &'static [&'static str]) -> StatReader {
    use crate::stat::port_stat;
    use std::sync::atomic::AtomicU64;

    let calls = AtomicU64::new(0);
    Box::new(move || {
        let n = calls.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(devices
            .iter()
            .map(|dev| port_stat(dev, 1, &[("tx_bytes", n * 1_000_000), ("rx_pkts", n * 10)]))
            .collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stat::port_stat;
    use std::sync::atomic::AtomicU64;

    fn failing_reader(
        fail_on: impl Fn(u64) -> bool + Send + 'static,
    ) -> (StatReader, Arc<AtomicU64>) {
        let calls = Arc::new(AtomicU64::new(0));
        let counter = calls.clone();
        let reader: StatReader = Box::new(move || {
            let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
            if fail_on(n) {
                return Err(io::Error::other("netlink gone"));
            }
            Ok(vec![port_stat("efa0", 1, &[("tx_bytes", n * 1000)])])
        });
        (reader, calls)
    }

    fn wait_until(done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn capture_for(millis: u64, devices: Option<Vec<String>>) -> CaptureResult {
        let capture = Capture::start_with_reader(
            growing_reader(&["mlx5_0", "mlx5_1"]),
            Duration::from_millis(5),
            devices,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(millis));
        capture.stop()
    }

    #[test]
    fn sample_timestamps_are_monotonic_and_within_the_run() {
        let before = unix_ns();
        let result = capture_for(60, None);
        let after = unix_ns();
        assert!(result.error.is_none());
        assert!(result.samples.len() >= 3, "got {}", result.samples.len());
        let stamps: Vec<_> = result.samples.iter().map(|s| s.ts_unix_ns).collect();
        assert!(stamps.is_sorted());
        assert!(stamps[0] >= before);
        assert!(*stamps.last().unwrap() <= after);
    }

    #[test]
    fn each_sample_carries_rates_for_every_port() {
        let result = capture_for(60, None);
        for sample in &result.samples {
            assert_eq!(sample.ports.len(), 2);
            assert_eq!(sample.ports[0].dev_name, "mlx5_0");
            assert!(sample.ports[0].tx_gbps > 0.0 && sample.ports[0].tx_gbps.is_finite());
            assert!(sample.ports[1].rx_pps > 0.0);
        }
    }

    #[test]
    fn device_filter_keeps_only_named_devices() {
        let result = capture_for(30, Some(vec!["mlx5_1".into()]));
        assert!(!result.samples.is_empty());
        for sample in &result.samples {
            assert_eq!(sample.ports.len(), 1);
            assert_eq!(sample.ports[0].dev_name, "mlx5_1");
        }
    }

    #[test]
    fn filter_matching_nothing_fails_at_start() {
        let err = Capture::start_with_reader(
            growing_reader(&["mlx5_0"]),
            Duration::from_millis(5),
            Some(vec!["nope".into()]),
        )
        .err()
        .expect("start must fail");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn transient_read_failure_is_skipped_and_the_gap_is_bridged() {
        let (reader, calls) = failing_reader(|n| n == 3);
        let capture = Capture::start_with_reader(reader, Duration::from_millis(2), None).unwrap();
        wait_until(|| calls.load(Ordering::Relaxed) >= 5);
        let result = capture.stop();
        let reads = calls.load(Ordering::Relaxed) as usize;
        assert!(result.error.is_none());
        assert_eq!(result.samples.len(), reads - 2);
        for sample in &result.samples {
            assert!(sample.ports[0].tx_gbps > 0.0);
        }
    }

    #[test]
    fn persistent_read_failure_stops_capture_and_keeps_earlier_samples() {
        let (reader, calls) = failing_reader(|n| n > 3);
        let capture = Capture::start_with_reader(reader, Duration::from_millis(2), None).unwrap();
        let expected_calls = 3 + u64::from(MAX_CONSECUTIVE_READ_FAILURES);
        wait_until(|| calls.load(Ordering::Relaxed) >= expected_calls);
        let result = capture.stop();
        assert_eq!(result.samples.len(), 2);
        assert_eq!(result.error.as_deref(), Some("netlink gone"));
    }

    #[test]
    fn next_due_skips_missed_deadlines_instead_of_bursting() {
        let interval = Duration::from_millis(10);
        let stale = Instant::now() - Duration::from_secs(1);
        let before = Instant::now();
        let due = next_due(stale, interval);
        assert!(due >= before && due <= Instant::now());
        let fresh = Instant::now() + Duration::from_secs(1);
        assert_eq!(next_due(fresh, interval), fresh + interval);
    }
}
