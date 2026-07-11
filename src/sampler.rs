//! Background sampling thread. Collects every subsystem into a `Snapshot`
//! and ships it to the UI over mpsc, so slow driver calls (NVML/amdsmi
//! init, netlink) never block rendering or input.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{net, nvlink, stat, xgmi};

/// Everything one sampling pass produces. Raw counters only; delta/rate
/// math stays on the UI thread where the previous snapshot lives.
pub struct Snapshot {
    /// None when the netlink read failed; the UI skips the snapshot to
    /// avoid diffing cumulative counters against a blanked baseline.
    pub stats: Option<Vec<stat::PortStat>>,
    pub ifstats: Vec<net::IfStats>,
    pub nvlink: Vec<nvlink::NvLinkSnapshot>,
    pub xgmi: Vec<xgmi::XgmiSnapshot>,
    pub processes: Vec<stat::ProcessRdmaInfo>,
    pub taken_at: Instant,
}

fn collect() -> Snapshot {
    let processes = stat::read_all_qps()
        .map(|qps| stat::aggregate_by_process(&qps))
        .unwrap_or_default();
    Snapshot {
        stats: stat::read_all_stats().ok(),
        ifstats: net::read_all_ifstats().unwrap_or_default(),
        nvlink: nvlink::read_all_nvlink_stats().unwrap_or_default(),
        xgmi: xgmi::read_all_xgmi_stats().unwrap_or_default(),
        processes,
        taken_at: Instant::now(),
    }
}

pub struct Sampler {
    rx: Receiver<Snapshot>,
    interval_ms: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    dead: bool,
}

impl Sampler {
    /// Spawn the sampling thread. It samples immediately (the baseline),
    /// then keeps sampling at the current interval until stopped.
    pub fn spawn(interval: Duration) -> Self {
        let interval_ms = Arc::new(AtomicU64::new(interval.as_millis() as u64));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let (t_interval, t_stop) = (interval_ms.clone(), stop.clone());
        std::thread::spawn(move || run(tx, t_interval, t_stop));
        Self {
            rx,
            interval_ms,
            stop,
            dead: false,
        }
    }

    /// Newest snapshot, draining any backlog. `None` when nothing arrived.
    pub fn try_latest(&mut self) -> Option<Snapshot> {
        let mut latest = None;
        loop {
            match self.rx.try_recv() {
                Ok(s) => latest = Some(s),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.dead = true;
                    break;
                }
            }
        }
        latest
    }

    /// True once the sampling thread has died (channel disconnected).
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    pub fn set_interval(&self, interval: Duration) {
        self.interval_ms
            .store(interval.as_millis() as u64, Ordering::Relaxed);
    }

    /// Ask the thread to exit. Detach, never join: a thread stuck inside a
    /// driver call must not block process exit.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run(tx: Sender<Snapshot>, interval_ms: Arc<AtomicU64>, stop: Arc<AtomicBool>) {
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let snap = collect();
        if tx.send(snap).is_err() {
            return; // UI gone
        }
        // Sleep in short slices so interval changes and stop apply quickly.
        let started = Instant::now();
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let interval = Duration::from_millis(interval_ms.load(Ordering::Relaxed));
            if started.elapsed() >= interval {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
