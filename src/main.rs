use std::io;

use rdmatop::{agent, remote, trace, tui};

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        tui::run()
    } else if args == ["--version"] || args == ["-V"] {
        println!("rdmatop {}", rdmatop::version());
        Ok(())
    } else if args == ["--help"] || args == ["-h"] {
        println!("{}", trace::HELP);
        Ok(())
    } else if args.first().map(String::as_str) == Some("--agent") {
        agent::run(parse_agent_opts(&args[1..])?)
    } else if args.first().map(String::as_str) == Some("--connect") {
        let addr = args.get(1).cloned().unwrap_or_default();
        if addr.is_empty() {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--connect requires host:port",
            ))
        } else {
            remote::smoke(&addr, None, 5)
        }
    } else if args.first().map(String::as_str) == Some("--nodes") {
        let spec = args.get(1).cloned();
        tui::run_multi(spec.as_deref())
    } else {
        trace::run(trace::parse(&args)?)
    }
}

/// Parse --agent flags: [--listen ADDR] [--name NAME] [--token-file F] [--stdio]
fn parse_agent_opts(rest: &[String]) -> io::Result<agent::Options> {
    let mut opts = agent::Options {
        listen: None,
        name: None,
        token_file: None,
        stdio: false,
    };
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--stdio" => {
                opts.stdio = true;
                i += 1;
            }
            "--listen" | "--name" | "--token-file" => {
                let value = rest.get(i + 1).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("missing value for {}", rest[i]),
                    )
                })?;
                match rest[i].as_str() {
                    "--listen" => opts.listen = Some(value.clone()),
                    "--name" => opts.name = Some(value.clone()),
                    "--token-file" => opts.token_file = Some(value.clone()),
                    _ => unreachable!(),
                }
                i += 2;
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown option: {other}"),
                ))
            }
        }
    }
    Ok(opts)
}
