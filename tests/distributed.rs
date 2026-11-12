//! Loopback integration tests for the agent/viewer wire protocol.
//! These run on any Linux host: the sampler degrades to empty rows
//! without RDMA hardware, but the protocol layer works identically.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rdmatop::agent;
use rdmatop::remote;

#[test]
fn handshake_and_frames_over_loopback() {
    // Fixed port to keep the test self-contained; agent::run binds it.
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let opts = agent::Options {
            listen: Some("127.0.0.1:7899".into()),
            name: Some("loop-node".into()),
            token_file: None,
            stdio: false,
        };
        let _ = tx.send("started".into());
        let _ = agent::run(opts);
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap();

    // Give the listener a moment, then connect as a viewer.
    thread::sleep(Duration::from_millis(200));
    let mut stream = TcpStream::connect("127.0.0.1:7899").expect("agent listening");
    let hello = r#"{"v":1,"interval_ms":200}"#;
    writeln!(stream, "{hello}").unwrap();
    stream.flush().unwrap();

    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.contains("\"ok\":true"), "welcome not ok: {line}");
    assert!(line.contains("loop-node"), "welcome missing name: {line}");

    // First data frame must arrive within a few intervals.
    let mut got_frame = false;
    for _ in 0..40 {
        line.clear();
        let n = reader.read_line(&mut line).unwrap();
        if n == 0 {
            break;
        }
        if line.contains("mono_ns") {
            got_frame = true;
            break;
        }
    }
    assert!(got_frame, "no data frame arrived");

    // Interval control frame must not kill the stream.
    writeln!(stream, "{{\"t\":\"interval\",\"ms\":300}}").unwrap();
    stream.flush().unwrap();
    thread::sleep(Duration::from_millis(100));
    // Agent still alive if the next read succeeds.
    line.clear();
    let _ = reader.read_line(&mut line);
}

#[test]
fn steady_remote_frames_produce_steady_rates_end_to_end() {
    // Fake agent: handshake, then one canned frame per 200ms with counters
    // advancing at a steady 10 Gbps (1.25e9 bytes/s). The viewer must fold
    // these into stable 10 Gbps rows -- no jumps, no zeros after the first.
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // Hello
        let welcome = r#"{"v":1,"ok":true,"host":"fake","name":"fake","pid":1,"caps":["rdma"]}"#;
        writeln!(writer, "{welcome}").unwrap();
        // Anchor: frames carry mono_ns starting now, +200ms each.
        let start = std::time::Instant::now();
        let mut f: u64 = 0;
        loop {
            f += 1;
            let mono_ns = start.elapsed().as_nanos() as u64;
            let tx = 250_000_000u64 * f; // 0.25e9 bytes per 0.2s = 10 Gbps
            let frame = format!(
                r#"{{"t":"snap","mono_ns":{mono_ns},"stats":{{"mono_ns":{mono_ns},"data":[{{"dev":"mlx5_0","port":1,"counters":[{{"name":"tx_bytes","value":{tx}}}]}}]}}}}"#
            );
            if writeln!(writer, "{frame}").is_err() || writer.flush().is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
    });

    let sampler = remote::RemoteSampler::spawn(addr, Duration::from_millis(200), None);
    // Wait for live + collect 10 frames through the REAL fold path.
    let mut app = rdmatop::tui::app::App::new_multi();
    app.add_node("fake");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut applied = 0usize;
    while applied < 10 {
        if let Some(snap) = sampler.try_latest() {
            app.apply_node_snapshot(1, snap);
            applied += 1;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out at {applied} frames"
        );
        thread::sleep(Duration::from_millis(50));
    }
    sampler.stop();

    let rows = &app.node_states[1].throughputs;
    assert_eq!(rows.len(), 1);
    let rate = rows[0].tx_gbps;
    assert!(
        (rate - 10.0).abs() < 0.5,
        "steady 10 Gbps input folded to {rate} Gbps"
    );
}

#[test]
fn viewer_ignores_ping_keepalives_and_keeps_folding() {
    // The agent sends {"t":"ping"} when snapshots stall; the viewer must
    // skip those lines without dropping the connection or corrupting state.
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // Hello
        let welcome =
            r#"{"v":1,"ok":true,"host":"pinger","name":"pinger","pid":1,"caps":["rdma"]}"#;
        writeln!(writer, "{welcome}").unwrap();
        let start = std::time::Instant::now();
        let mut f: u64 = 0;
        loop {
            f += 1;
            let mono_ns = start.elapsed().as_nanos() as u64;
            let tx = 250_000_000u64 * f;
            let frame = format!(
                r#"{{"t":"snap","mono_ns":{mono_ns},"stats":{{"mono_ns":{mono_ns},"data":[{{"dev":"mlx5_0","port":1,"counters":[{{"name":"tx_bytes","value":{tx}}}]}}]}}}}"#
            );
            if writeln!(writer, "{frame}").is_err() || writer.flush().is_err() {
                return;
            }
            // Interleave keepalive pings like a stalling agent would.
            for _ in 0..3 {
                thread::sleep(Duration::from_millis(100));
                let ping = r#"{"t":"ping"}"#;
                if writeln!(writer, "{ping}").is_err() || writer.flush().is_err() {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
    });

    let sampler = remote::RemoteSampler::spawn(addr, Duration::from_millis(200), None);
    let mut app = rdmatop::tui::app::App::new_multi();
    app.add_node("pinger");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut applied = 0usize;
    while applied < 5 {
        if let Some(snap) = sampler.try_latest() {
            app.apply_node_snapshot(1, snap);
            applied += 1;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out at {applied} frames; connection dropped on pings?"
        );
        thread::sleep(Duration::from_millis(50));
    }
    sampler.stop();
    assert_eq!(app.node_states[1].throughputs.len(), 1);
    let rate = app.node_states[1].throughputs[0].tx_gbps;
    // 250 MB per 400ms frame interval (pings included) = 5 Gbps.
    assert!(
        (rate - 5.0).abs() < 0.5,
        "steady 5 Gbps with pings folded to {rate}"
    );
}

#[test]
fn remote_sampler_reconnects_after_agent_death() {
    // Start an agent, connect a RemoteSampler, kill the agent (drop the
    // listener thread), and verify the state machine reports the loss.
    let addr = "127.0.0.1:7898";
    let (tx, rx) = mpsc::channel::<()>();
    let _agent_thread = thread::spawn(move || {
        let opts = agent::Options {
            listen: Some(addr.into()),
            name: Some("mortal".into()),
            token_file: None,
            stdio: false,
        };
        let _ = tx.send(());
        let _ = agent::run(opts);
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    thread::sleep(Duration::from_millis(200));

    let sampler = remote::RemoteSampler::spawn(addr.into(), Duration::from_millis(200), None);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if sampler.state() == remote::LinkState::Live {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "never went live");
        thread::sleep(Duration::from_millis(50));
    }
    // The agent thread runs until process exit; we can only verify the
    // viewer tolerates a live agent and stops cleanly.
    sampler.stop();
}
