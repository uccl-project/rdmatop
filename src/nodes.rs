//! Node registry for multi-node mode: parses `--nodes`, owns one
//! sampler per node (local first, then remotes), and exposes per-node
//! state queries the TUI needs.

use std::time::Duration;

use crate::remote::RemoteSampler;
use crate::sampler::Sampler;

/// One monitored node.
pub struct Node {
    /// Display name (agent-reported, or the address while connecting).
    pub name: String,
    /// Address as given in --nodes (host:port); empty for the local node.
    pub addr: String,
    pub local: bool,
    /// Local sampling thread; Some only for the local node. A remote node
    /// must never run a local sampler: its snapshots would be folded into
    /// the remote node's state and show the viewer's own hardware there.
    pub sampler: Option<Sampler>,
    pub remote: Option<RemoteSampler>,
}

pub struct NodeManager {
    pub nodes: Vec<Node>,
}

/// Parse a --nodes spec: comma-separated host:port entries, optionally
/// prefixed with name=host:port. `@path` entries load one spec per line.
pub fn parse_spec(spec: &str) -> std::io::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for entry in spec.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some(path) = entry.strip_prefix('@') {
            let text = std::fs::read_to_string(path)?;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                out.push(parse_entry(line)?);
            }
        } else {
            out.push(parse_entry(entry)?);
        }
    }
    Ok(out)
}

fn parse_entry(entry: &str) -> std::io::Result<(String, String)> {
    // name=host:port, or host:port (name defaults to host).
    if let Some((name, addr)) = entry.split_once('=') {
        let name = name.trim();
        let addr = addr.trim();
        if name.is_empty() || addr.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid node entry: {entry}"),
            ));
        }
        Ok((name.to_string(), addr.to_string()))
    } else {
        if entry.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty node entry",
            ));
        }
        Ok((entry.to_string(), entry.to_string()))
    }
}

impl NodeManager {
    /// Build the node list: the local node first, then one per spec entry.
    pub fn spawn(
        spec: Option<&str>,
        interval: Duration,
        token: Option<String>,
    ) -> std::io::Result<Self> {
        let mut nodes = Vec::new();
        // Local node always present; the ALL view shows it alongside remotes.
        nodes.push(Node {
            name: "local".into(),
            addr: String::new(),
            local: true,
            sampler: Some(Sampler::spawn(interval)),
            remote: None,
        });
        if let Some(spec) = spec {
            for (name, addr) in parse_spec(spec)? {
                nodes.push(Node {
                    name,
                    addr: addr.clone(),
                    local: false,
                    sampler: None,
                    remote: Some(RemoteSampler::spawn(addr, interval, token.clone())),
                });
            }
        }
        Ok(Self { nodes })
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Adopt the agent-reported display name once the handshake lands.
    pub fn refresh_names(&mut self) {
        for node in &mut self.nodes {
            if let Some(remote) = &node.remote {
                if let Some(w) = remote.welcome() {
                    if !w.name.is_empty() && node.name != w.name {
                        node.name = w.name;
                    }
                }
            }
        }
    }

    pub fn stop_all(&self) {
        for node in &self.nodes {
            if let Some(sampler) = &node.sampler {
                sampler.stop();
            }
            if let Some(remote) = &node.remote {
                remote.stop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spec_supports_names_and_files() {
        let spec = "n1=10.0.0.1:7809,10.0.0.2:7809";
        let nodes = parse_spec(spec).unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].0, "n1");
        assert_eq!(nodes[0].1, "10.0.0.1:7809");
        assert_eq!(nodes[1].0, "10.0.0.2:7809");
    }

    #[test]
    fn parse_spec_skips_empty_entries() {
        let nodes = parse_spec("a:1,,b:2").unwrap();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn parse_entry_rejects_bad() {
        assert!(parse_entry("=host:1").is_err());
        assert!(parse_entry("").is_err());
    }
}
