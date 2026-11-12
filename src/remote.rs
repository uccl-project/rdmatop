//! Viewer side of the wire protocol: `RemoteSampler` connects to an
//! agent, rebuilds `Snapshot`s from frames, and exposes the same
//! single-slot mailbox interface as the local `Sampler`, so the TUI
//! loop is identical for local and remote nodes.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::sampler::Snapshot;
use crate::wire::{self, Hello, Welcome, PROTOCOL_VERSION};

/// Cap inbound lines; a corrupt or hostile stream must not balloon memory.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Connection state surfaced to the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    Connecting,
    Live,
    Disconnected,
}

struct Shared {
    slot: Mutex<Option<Snapshot>>,
    state: Mutex<LinkState>,
    /// Agent-reported identity from the Welcome.
    welcome: Mutex<Option<Welcome>>,
    interval_ms: AtomicU64,
    stop: AtomicBool,
}

impl Shared {
    fn new(interval_ms: u64) -> Self {
        Self {
            slot: Mutex::new(None),
            state: Mutex::new(LinkState::Connecting),
            welcome: Mutex::new(None),
            interval_ms: AtomicU64::new(interval_ms.max(1)),
            stop: AtomicBool::new(false),
        }
    }
}

/// Maps agent mono_ns back to local Instants: a single anchor per
/// connection keeps adjacent-frame deltas exact.
#[derive(Clone, Copy)]
struct MonoMap {
    anchor: Instant,
}

impl MonoMap {
    fn new() -> Self {
        Self {
            anchor: Instant::now(),
        }
    }
    fn instant(&self, mono_ns: u64) -> Instant {
        self.anchor
            .checked_add(Duration::from_nanos(mono_ns))
            .unwrap_or(self.anchor)
    }
}

fn read_capped_line(reader: &mut impl BufRead, buf: &mut String) -> bool {
    buf.clear();
    let mut limited = reader.take(MAX_LINE_BYTES as u64);
    match limited.read_line(buf) {
        Ok(0) => false,
        Ok(_) => buf.len() < MAX_LINE_BYTES || buf.ends_with('\n'),
        Err(_) => false,
    }
}

pub struct RemoteSampler {
    shared: Arc<Shared>,
}

impl RemoteSampler {
    /// Connect to `addr` (host:port) and start the reader loop. Reconnects
    /// with capped backoff until `stop()`.
    pub fn spawn(addr: String, interval: Duration, token: Option<String>) -> Self {
        let shared = Arc::new(Shared::new(interval.as_millis().max(1) as u64));
        let thread_shared = Arc::clone(&shared);
        thread::spawn(move || run(&addr, token, thread_shared));
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

    pub fn state(&self) -> LinkState {
        *self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Agent identity once the handshake completed.
    pub fn welcome(&self) -> Option<Welcome> {
        self.shared
            .welcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_interval(&self, interval: Duration) {
        let ms = (interval.as_millis() as u64).max(1);
        self.shared.interval_ms.store(ms, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for RemoteSampler {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One connection attempt: handshake, then read frames until EOF/error.
fn serve_connection(stream: TcpStream, shared: &Shared, token: &Option<String>) -> io::Result<()> {
    let mono = MonoMap::new();
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    let interval_ms = shared.interval_ms.load(Ordering::Relaxed);
    let hello = Hello {
        v: PROTOCOL_VERSION,
        token: token.clone(),
        interval_ms,
    };
    let hello_line = serde_json::to_string(&hello)?;
    writeln!(writer, "{hello_line}")?;
    writer.flush()?;

    let mut line = String::new();
    if !read_capped_line(&mut reader, &mut line) {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "no welcome"));
    }
    let welcome: Welcome = serde_json::from_str(line.trim_end())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad welcome: {e}")))?;
    if !welcome.ok {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            welcome.error.unwrap_or_else(|| "denied".into()),
        ));
    }
    *shared.welcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(welcome);
    *shared.state.lock().unwrap_or_else(|e| e.into_inner()) = LinkState::Live;

    let mut buf = String::new();
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            let _ = writeln!(writer, "{{\"t\":\"bye\"}}");
            let _ = writer.flush();
            return Ok(());
        }
        if !read_capped_line(&mut reader, &mut buf) {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "stream end"));
        }
        // Skip malformed lines; a single bad frame must not kill the feed.
        let Ok(wire_snap) = serde_json::from_str::<wire::WireSnapshot>(buf.trim_end()) else {
            continue;
        };
        let snap = wire::from_wire(&wire_snap, |ns| mono.instant(ns));
        *shared.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(snap);
    }
}

fn run(addr: &str, token: Option<String>, shared: Arc<Shared>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        match TcpStream::connect(addr) {
            Ok(stream) => {
                // Generous timeout: a stalled agent (dead sampler thread)
                // must surface as STALE data, not flap the connection; the
                // agent also sends keepalive pings when idle.
                let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
                match serve_connection(stream, &shared, &token) {
                    Ok(()) => return, // clean bye
                    Err(_) => {
                        *shared.state.lock().unwrap_or_else(|e| e.into_inner()) =
                            LinkState::Disconnected;
                    }
                }
            }
            Err(_) => {
                *shared.state.lock().unwrap_or_else(|e| e.into_inner()) = LinkState::Disconnected;
            }
        }
        // Capped backoff: 1s, 2s, 4s, ... up to 30s.
        let mut waited = Duration::ZERO;
        while waited < backoff {
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            let slice = backoff - waited;
            let slice = slice.min(Duration::from_millis(200));
            thread::sleep(slice);
            waited += slice;
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

/// Smoke-test viewer: connect, print the welcome and each frame summary.
/// Used by `rdmatop --connect ADDR` and by the loopback tests.
pub fn smoke(addr: &str, token: Option<String>, frames: u64) -> io::Result<()> {
    let sampler = RemoteSampler::spawn(addr.to_string(), Duration::from_millis(500), token);
    // Wait for the handshake.
    let deadline = Instant::now() + Duration::from_secs(5);
    let welcome = loop {
        if let Some(w) = sampler.welcome() {
            break w;
        }
        if Instant::now() > deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no welcome"));
        }
        thread::sleep(Duration::from_millis(50));
    };
    println!(
        "welcome: {} (host: {}, caps: {:?})",
        welcome.name, welcome.host, welcome.caps
    );
    let mut got = 0u64;
    let deadline = Instant::now() + Duration::from_secs(10);
    while got < frames {
        if let Some(snap) = sampler.try_latest() {
            let ports = snap.stats.map(|s| s.data.len()).unwrap_or(0);
            let procs = snap.processes.map(|p| p.len()).unwrap_or(0);
            let host = snap.sys.map(|s| s.hostname).unwrap_or_default();
            println!("frame {got}: ports={ports} procs={procs} host={host}");
            got += 1;
        } else if Instant::now() > deadline {
            break;
        } else {
            thread::sleep(Duration::from_millis(50));
        }
    }
    sampler.stop();
    if got == 0 {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "no frames"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_map_preserves_deltas() {
        let m = MonoMap::new();
        let a = m.instant(1_000_000_000);
        let b = m.instant(2_000_000_000);
        assert_eq!(b.duration_since(a).as_secs_f64(), 1.0);
    }

    #[test]
    fn read_capped_line_rejects_runaway() {
        let mut huge = io::Cursor::new(vec![b'x'; MAX_LINE_BYTES + 10]);
        let mut buf = String::new();
        assert!(!read_capped_line(&mut huge, &mut buf));
    }
}
