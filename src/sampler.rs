//! Background sampling thread. Collects every subsystem into a `Snapshot`
//! and hands it to the UI through a single-slot mailbox, so slow driver
//! calls (NVML/amdsmi init, netlink) never block rendering or input.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{net, nvlink, stat, xgmi};

/// Everything one sampling pass produces. Raw counters only; delta/rate
/// math stays on the UI thread where the previous snapshot lives.
/// Each field is None when its read failed, so one failing subsystem
/// (e.g. a kernel without rdma netlink) never blocks the others.
pub struct Snapshot {
    pub stats: Option<Vec<stat::PortStat>>,
    pub ifstats: Option<Vec<net::IfStats>>,
    pub nvlink: Option<Vec<nvlink::NvLinkSnapshot>>,
    pub xgmi: Option<Vec<xgmi::XgmiSnapshot>>,
    pub processes: Option<Vec<stat::ProcessRdmaInfo>>,
    pub taken_at: Instant,
}

fn collect() -> Snapshot {
    // Stamp before the reads: the UI derives rates from taken_at deltas, so
    // a slow pass (e.g. first-time driver init) must not inflate elapsed.
    let taken_at = Instant::now();
    let processes = stat::read_all_qps()
        .ok()
        .map(|qps| stat::aggregate_by_process(&qps));
    Snapshot {
        stats: stat::read_all_stats().ok(),
        ifstats: net::read_all_ifstats().ok(),
        nvlink: nvlink::read_all_nvlink_stats().ok(),
        xgmi: xgmi::read_all_xgmi_stats().ok(),
        processes,
        taken_at,
    }
}

/// State shared between the sampling thread and the UI-side `Sampler`.
struct Shared {
    /// Latest snapshot; the thread overwrites, the UI takes. A single slot
    /// caps memory at one snapshot even if the UI stalls for hours.
    slot: Mutex<Option<Snapshot>>,
    interval_ms: AtomicU64,
    stop: AtomicBool,
    /// Panic message when the thread died; the UI surfaces it, since the
    /// default panic output is lost inside the alternate screen.
    died: Mutex<Option<String>>,
}

pub struct Sampler {
    shared: Arc<Shared>,
}

impl Sampler {
    /// Spawn the sampling thread. It samples immediately (the baseline),
    /// then keeps sampling at the current interval until stopped.
    pub fn spawn(interval: Duration) -> Self {
        let shared = Arc::new(Shared {
            slot: Mutex::new(None),
            interval_ms: AtomicU64::new(interval.as_millis() as u64),
            stop: AtomicBool::new(false),
            died: Mutex::new(None),
        });
        let thread_shared = shared.clone();
        std::thread::spawn(move || run(&thread_shared));
        Self { shared }
    }

    /// Latest snapshot, if a new one arrived since the last call.
    pub fn try_latest(&self) -> Option<Snapshot> {
        self.shared
            .slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// The captured panic message when the thread died; None while alive.
    pub fn death_reason(&self) -> Option<String> {
        self.shared
            .died
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_interval(&self, interval: Duration) {
        self.shared
            .interval_ms
            .store(interval.as_millis() as u64, Ordering::Relaxed);
    }

    /// Ask the thread to exit. Detach, never join: a thread stuck inside a
    /// driver call must not block process exit.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Sampler {
    // The mailbox has no disconnect signal (unlike a channel), so stopping
    // on drop is what keeps the thread from sampling forever on error paths.
    fn drop(&mut self) {
        self.stop();
    }
}

/// Render a `catch_unwind` payload (typically &str or String) for the UI.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn run(shared: &Shared) {
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        let snap = match std::panic::catch_unwind(collect) {
            Ok(s) => s,
            Err(payload) => {
                let mut died = shared.died.lock().unwrap_or_else(|e| e.into_inner());
                *died = Some(panic_message(payload));
                return;
            }
        };
        *shared.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(snap);
        // Sleep in short slices so interval changes and stop apply quickly.
        let started = Instant::now();
        loop {
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            let interval = Duration::from_millis(shared.interval_ms.load(Ordering::Relaxed));
            if started.elapsed() >= interval {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
