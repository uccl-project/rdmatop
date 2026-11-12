//! Agent mode: serve sampling snapshots over the wire protocol so a
//! remote `rdmatop --nodes` viewer can display this host. One process
//! serves any number of viewers; the local sampler is shared and its
//! snapshots are serialized once per pass and broadcast to all connections.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::sampler::Sampler;
use crate::sysinfo;
use crate::wire::{self, Control, Hello, Welcome, PROTOCOL_VERSION};

/// Default listen address: loopback only. Cluster use must opt in with
/// `--listen 0.0.0.0:PORT` and should pair it with `--token-file`.
const DEFAULT_LISTEN: &str = "127.0.0.1:7809";

/// Reject lines larger than this; a runaway frame (huge process list)
/// must not balloon agent memory. Disconnect instead.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

pub struct Options {
    pub listen: Option<String>,
    pub name: Option<String>,
    pub token_file: Option<String>,
    /// Serve one viewer over stdin/stdout instead of TCP (SSH transport).
    pub stdio: bool,
}

/// Constant-time equality so a token guess cannot be timed.
fn token_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

struct Shared {
    /// Serialized latest snapshot; connection writers clone the Arc.
    latest: Mutex<Option<Arc<str>>>,
    /// Monotonic anchor for mono_ns timestamps.
    anchor: Instant,
    interval_ms: AtomicU64,
}

impl Shared {
    fn new(interval_ms: u64) -> Self {
        Self {
            latest: Mutex::new(None),
            anchor: Instant::now(),
            interval_ms: AtomicU64::new(interval_ms.max(1)),
        }
    }
}

/// Broadcast loop: take each fresh snapshot from the sampler, serialize
/// once, store it for connection threads to pick up.
fn broadcast(shared: Arc<Shared>, sampler: Sampler) {
    // Surface a dead sampler thread loudly (once): the viewer would
    // otherwise see a frozen node flapping STALE/DISCONNECTED silently.
    let mut reported_death = false;
    loop {
        if !reported_death {
            if let Some(reason) = sampler.death_reason() {
                eprintln!("rdmatop agent: sampler thread died: {reason}");
                reported_death = true;
            }
        }
        if let Some(snap) = sampler.try_latest() {
            let anchor = shared.anchor;
            let mono = move |i: Instant| i.duration_since(anchor).as_nanos() as u64;
            match serde_json::to_string(&wire::to_wire(&snap, mono)) {
                Ok(json) => {
                    let arc: Arc<str> = Arc::from(json.as_str());
                    *shared.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(arc);
                }
                Err(_) => continue, // skip a pass we cannot serialize
            }
        }
        // Wake often enough to notice interval changes without busy-waiting.
        let interval = Duration::from_millis(shared.interval_ms.load(Ordering::Relaxed));
        thread::sleep((interval / 4).max(Duration::from_millis(25)).min(interval));
    }
}

fn caps_from_first_snapshot(sampler: &Sampler) -> Vec<String> {
    // The sampler samples immediately on spawn; wait briefly for it.
    let mut caps = Vec::new();
    for _ in 0..20 {
        if let Some(snap) = sampler.try_latest() {
            if snap.stats.is_some() {
                caps.push("rdma".into());
            }
            if snap.ifstats.is_some() {
                caps.push("net".into());
            }
            if snap.nvlink.is_some() {
                caps.push("nvlink".into());
            }
            if snap.xgmi.is_some() {
                caps.push("xgmi".into());
            }
            if snap.processes.is_some() {
                caps.push("procs".into());
            }
            if snap.sys.is_some() {
                caps.push("sys".into());
            }
            return caps;
        }
        thread::sleep(Duration::from_millis(50));
    }
    caps
}

fn welcome(name: &str, caps: Vec<String>) -> Welcome {
    Welcome {
        v: PROTOCOL_VERSION,
        ok: true,
        error: None,
        host: sysinfo::read_sysinfo(Default::default()).hostname,
        name: name.to_string(),
        pid: std::process::id(),
        caps,
    }
}

fn denied(error: &str) -> String {
    format!(
        "{{\"v\":{PROTOCOL_VERSION},\"ok\":false,\"error\":\"{error}\",\"host\":\"\",\"name\":\"\",\"pid\":0,\"caps\":[]}}"
    )
}

/// Read one line with a hard byte cap; false when the peer exceeds it or
/// the stream ends.
fn read_capped_line(reader: &mut impl BufRead, buf: &mut String) -> bool {
    buf.clear();
    let mut limited = reader.take(MAX_LINE_BYTES as u64);
    match limited.read_line(buf) {
        Ok(0) => false,
        Ok(_) => buf.len() < MAX_LINE_BYTES || buf.ends_with('\n'),
        Err(_) => false,
    }
}

/// Apply a viewer handshake: version + token checks, interval adoption.
/// Returns the reply line to send (either a Welcome or a denial).
fn handshake(
    hello_line: &str,
    expected_token: Option<&str>,
    shared: &Shared,
    sampler: &Sampler,
    name: &str,
    caps: &[String],
) -> Result<String, String> {
    let hello: Hello =
        serde_json::from_str(hello_line.trim_end()).map_err(|_| denied("bad handshake"))?;
    if hello.v != PROTOCOL_VERSION {
        return Err(denied("version mismatch"));
    }
    if let Some(expected) = expected_token {
        let got = hello.token.unwrap_or_default();
        if !token_eq(expected, &got) {
            return Err(denied("bad token"));
        }
    }
    let ms = hello.interval_ms.max(1);
    shared.interval_ms.store(ms, Ordering::Relaxed);
    sampler.set_interval(Duration::from_millis(ms));
    serde_json::to_string(&welcome(name, caps.to_vec())).map_err(|_| denied("serialize"))
}

/// Serve one accepted TCP connection: handshake, then stream frames.
fn serve_conn(
    stream: TcpStream,
    shared: Arc<Shared>,
    sampler: Sampler,
    expected_token: Option<String>,
    name: String,
    caps: Vec<String>,
) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;

    let mut line = String::new();
    if !read_capped_line(&mut reader, &mut line) {
        return;
    }
    match handshake(
        &line,
        expected_token.as_deref(),
        &shared,
        &sampler,
        &name,
        &caps,
    ) {
        Ok(reply) => {
            if writeln!(writer, "{reply}").is_err() {
                return;
            }
            let _ = writer.flush();
        }
        Err(reply) => {
            let _ = writeln!(writer, "{reply}");
            let _ = writer.flush();
            return;
        }
    }

    // Control frames arrive on the read half; own the reader in this thread
    // and stop when the peer goes away.
    {
        let sampler = sampler;
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            let mut ctrl = String::new();
            loop {
                if !read_capped_line(&mut reader, &mut ctrl) {
                    return;
                }
                match serde_json::from_str::<Control>(ctrl.trim_end()) {
                    Ok(Control::Interval { ms }) => {
                        let ms = ms.max(1);
                        shared.interval_ms.store(ms, Ordering::Relaxed);
                        sampler.set_interval(Duration::from_millis(ms));
                    }
                    Ok(Control::Bye) => return,
                    Err(_) => {} // ignore unrecognized lines; never kill the stream
                }
            }
        });
    }

    // Writer loop: push the latest serialized snapshot when it changes,
    // and a keepalive ping when snapshots stall (dead sampler, slow driver)
    // so the viewer marks the node STALE instead of flapping the connection.
    let mut sent: Option<Arc<str>> = None;
    let mut last_push = Instant::now();
    loop {
        let current = shared
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(json) = current {
            if sent.as_ref().map(|s| **s != *json).unwrap_or(true) {
                if writeln!(writer, "{json}").is_err() {
                    return;
                }
                if writer.flush().is_err() {
                    return;
                }
                sent = Some(json);
                last_push = Instant::now();
            }
        }
        if last_push.elapsed() > Duration::from_secs(2) {
            if writeln!(writer, "{{\"t\":\"ping\"}}").is_err() {
                return;
            }
            if writer.flush().is_err() {
                return;
            }
            last_push = Instant::now();
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// stdio variant: one viewer, stdin for control frames, stdout for frames.
fn serve_stdio(
    shared: Arc<Shared>,
    sampler: Sampler,
    expected_token: Option<String>,
    name: String,
    caps: Vec<String>,
) {
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let stdin = io::stdin();
    let mut reader = stdin.lock();

    let mut line = String::new();
    if !read_capped_line(&mut reader, &mut line) {
        return;
    }
    match handshake(
        &line,
        expected_token.as_deref(),
        &shared,
        &sampler,
        &name,
        &caps,
    ) {
        Ok(reply) => {
            if writeln!(writer, "{reply}").is_err() {
                return;
            }
            let _ = writer.flush();
        }
        Err(_) => return,
    }

    // Control frames from stdin in a thread; exit stops the whole agent.
    {
        let sampler = sampler;
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            let stdin = io::stdin();
            let mut reader = stdin.lock();
            let mut ctrl = String::new();
            loop {
                if !read_capped_line(&mut reader, &mut ctrl) {
                    return;
                }
                match serde_json::from_str::<Control>(ctrl.trim_end()) {
                    Ok(Control::Interval { ms }) => {
                        let ms = ms.max(1);
                        shared.interval_ms.store(ms, Ordering::Relaxed);
                        sampler.set_interval(Duration::from_millis(ms));
                    }
                    Ok(Control::Bye) => {
                        let _ = io::stdout().flush();
                        std::process::exit(0);
                    }
                    Err(_) => {}
                }
            }
        });
    }

    let mut sent: Option<Arc<str>> = None;
    let mut last_push = Instant::now();
    loop {
        let current = shared
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(json) = current {
            if sent.as_ref().map(|s| **s != *json).unwrap_or(true) {
                if writeln!(writer, "{json}").is_err() {
                    return;
                }
                if writer.flush().is_err() {
                    return;
                }
                sent = Some(json);
                last_push = Instant::now();
            }
        }
        if last_push.elapsed() > Duration::from_secs(2) {
            if writeln!(writer, "{{\"t\":\"ping\"}}").is_err() {
                return;
            }
            if writer.flush().is_err() {
                return;
            }
            last_push = Instant::now();
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Run the agent. Blocks until the listener errors out.
pub fn run(opts: Options) -> io::Result<()> {
    let interval_ms = 1000u64;
    let sampler = Sampler::spawn(Duration::from_millis(interval_ms));
    let caps = caps_from_first_snapshot(&sampler);
    let name = opts
        .name
        .clone()
        .unwrap_or_else(|| sysinfo::read_sysinfo(Default::default()).hostname);
    let expected_token = opts
        .token_file
        .as_ref()
        .map(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .filter(|t| !t.is_empty());

    if opts.stdio {
        let shared = Arc::new(Shared::new(interval_ms));
        {
            let s = sampler.clone_handle();
            let sh = Arc::clone(&shared);
            thread::spawn(move || broadcast(sh, s));
        }
        serve_stdio(shared, sampler, expected_token, name, caps);
        return Ok(());
    }

    let listen = opts.listen.unwrap_or_else(|| DEFAULT_LISTEN.into());
    let listener = TcpListener::bind(&listen)?;
    eprintln!(
        "rdmatop agent {} listening on {listen} (name: {name})",
        crate::version()
    );
    let shared = Arc::new(Shared::new(interval_ms));
    {
        let s = sampler.clone_handle();
        let sh = Arc::clone(&shared);
        thread::spawn(move || broadcast(sh, s));
    }
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let shared = Arc::clone(&shared);
                let sampler = sampler.clone_handle();
                let token = expected_token.clone();
                let name = name.clone();
                let caps = caps.clone();
                thread::spawn(move || {
                    serve_conn(stream, shared, sampler, token, name, caps);
                });
            }
            Err(_) => continue, // transient accept error; keep serving
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_eq_is_constant_time_shape() {
        assert!(token_eq("abc", "abc"));
        assert!(!token_eq("abc", "abd"));
        assert!(!token_eq("abc", "abcd"));
        assert!(!token_eq("", "x"));
    }

    #[test]
    fn denied_line_is_valid_json() {
        let line = denied("bad token");
        let w: Welcome = serde_json::from_str(&line).unwrap();
        assert!(!w.ok);
        assert_eq!(w.error.as_deref(), Some("bad token"));
    }
}
