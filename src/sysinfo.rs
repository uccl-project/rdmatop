//! Host-level info (hostname, uptime, load, memory, CPU) read from /proc.
//! Lives in the sampler so the same data flows to the local TUI and to
//! remote viewers over the wire protocol; the UI thread never reads /proc.

use crate::net::NetRate;

#[derive(Clone, Debug, Default)]
pub struct SysInfo {
    pub hostname: String,
    pub uptime: String,
    pub load_avg: String,
    pub mem_total_mb: u64,
    pub mem_used_mb: u64,
    pub mem_pct: f32,
    pub cpu_pct: f32,
    /// Aggregate net rate; filled by the consumer from ifstats deltas so the
    /// sampler stays a dumb forwarder of raw counters.
    pub net: NetRate,
}

fn read_file_trimmed(path: &str) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn read_hostname() -> String {
    let h = read_file_trimmed("/etc/hostname");
    if h.is_empty() {
        "unknown".into()
    } else {
        h
    }
}

fn read_uptime() -> String {
    let secs: u64 = read_file_trimmed("/proc/uptime")
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    format!(
        "up {} days, {}:{:02}",
        secs / 86400,
        (secs % 86400) / 3600,
        (secs % 3600) / 60
    )
}

fn read_load_avg() -> String {
    read_file_trimmed("/proc/loadavg")
        .split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join(", ")
}

fn read_meminfo() -> (u64, u64) {
    let content = read_file_trimmed("/proc/meminfo");
    let mut total = 0u64;
    let mut avail = 0u64;
    for line in content.lines() {
        let val = || {
            line.split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        if line.starts_with("MemTotal:") {
            total = val();
        }
        if line.starts_with("MemAvailable:") {
            avail = val();
        }
    }
    (total / 1024, (total.saturating_sub(avail)) / 1024)
}

fn read_cpu_usage() -> f32 {
    let content = read_file_trimmed("/proc/stat");
    let first = content.lines().next().unwrap_or("");
    let vals: Vec<u64> = first
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    if vals.len() < 4 {
        return 0.0;
    }
    let idle = vals[3];
    let total: u64 = vals.iter().sum();
    if total == 0 {
        return 0.0;
    }
    ((total - idle) as f32 / total as f32) * 100.0
}

/// Read host info; `net` is filled by the caller from interface deltas.
pub fn read_sysinfo(net: NetRate) -> SysInfo {
    let (mem_total_mb, mem_used_mb) = read_meminfo();
    let mem_pct = if mem_total_mb > 0 {
        (mem_used_mb as f32 / mem_total_mb as f32) * 100.0
    } else {
        0.0
    };
    SysInfo {
        hostname: read_hostname(),
        uptime: read_uptime(),
        load_avg: read_load_avg(),
        mem_total_mb,
        mem_used_mb,
        mem_pct,
        cpu_pct: read_cpu_usage(),
        net,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysinfo_default_is_all_zero() {
        let s = SysInfo::default();
        assert_eq!(s.hostname, "");
        assert_eq!(s.mem_total_mb, 0);
        assert_eq!(s.net.tx_bytes_per_sec, 0.0);
    }

    #[test]
    fn read_sysinfo_returns_hostname() {
        // /etc/hostname exists on any Linux host; the read must not fail.
        let s = read_sysinfo(NetRate::default());
        assert!(!s.hostname.is_empty());
    }
}
