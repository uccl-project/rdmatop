//! Wire protocol types: the JSON-lines snapshot format exchanged between
//! `rdmatop --agent` and viewers. Mirrors `sampler::Snapshot` field for
//! field; `Instant` timestamps become `mono_ns` (nanoseconds since the
//! connection was established), so rates computed from adjacent frames are
//! immune to clock skew between hosts.

use serde::{Deserialize, Serialize};

use crate::gpu::GpuMetrics;
use crate::net::IfStats;
use crate::nvlink::{LinkSnapshot, NvLinkSnapshot, RemoteDeviceType};
use crate::sampler::{Sample, Snapshot};
use crate::stat::{HwCounter, PortStat, ProcessRdmaInfo};
use crate::sysinfo::SysInfo;
use crate::xgmi::{XgmiLinkSnapshot, XgmiSnapshot};

/// Protocol version; a viewer rejects a different version.
pub const PROTOCOL_VERSION: u32 = 1;

/// Handshake sent by the viewer as the first line after connecting.
#[derive(Serialize, Deserialize, Debug)]
pub struct Hello {
    pub v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default = "default_interval_ms")]
    pub interval_ms: u64,
}

fn default_interval_ms() -> u64 {
    1000
}

/// Agent's handshake reply.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Welcome {
    pub v: u32,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Hostname as the agent sees itself.
    pub host: String,
    /// Display name (`--name` override or the hostname).
    pub name: String,
    pub pid: u32,
    /// Subsystems that produced data in the first sampling pass.
    pub caps: Vec<String>,
}

/// Viewer-to-agent control frame.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "t")]
pub enum Control {
    #[serde(rename = "interval")]
    Interval { ms: u64 },
    #[serde(rename = "bye")]
    Bye,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WHwCounter {
    pub name: String,
    pub value: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WPortStat {
    pub dev: String,
    pub port: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_gbps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default)]
    pub counters: Vec<WHwCounter>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WIfStats {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WGpuMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub util_pct: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_used_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_total_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_c: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_mhz: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WLinkSnapshot {
    pub link_id: u32,
    pub is_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_gbps: Option<f64>,
    /// "gpu" | "npu" | "switch" | "unknown"
    pub remote: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_pci_bdf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rx_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crc_error_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_error_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_error_count: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WNvLinkSnapshot {
    pub gpu_index: u32,
    pub gpu_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<WGpuMetrics>,
    pub link_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_gbps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rx_bytes: Option<u64>,
    #[serde(default)]
    pub links: Vec<WLinkSnapshot>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WXgmiLinkSnapshot {
    pub link_id: u32,
    pub is_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_gbps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_rate_gbps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_pci_bdf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rx_bytes: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WXgmiSnapshot {
    pub gpu_index: u32,
    pub gpu_name: String,
    pub link_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_gbps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correctable_errors: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncorrectable_errors: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<WGpuMetrics>,
    #[serde(default)]
    pub links: Vec<WXgmiLinkSnapshot>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WProcessRdmaInfo {
    pub pid: u32,
    pub dev: String,
    pub qp_count: usize,
    pub user: String,
    pub nice: i32,
    pub state: String,
    pub virt_kb: u64,
    pub res_kb: u64,
    pub shr_kb: u64,
    pub mem_pct: f32,
    pub threads: u32,
    pub cmdline: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WSysInfo {
    pub hostname: String,
    pub uptime: String,
    pub load_avg: String,
    pub mem_total_mb: u64,
    pub mem_used_mb: u64,
    pub mem_pct: f32,
    pub cpu_pct: f32,
    // net is derived by the viewer from ifstats deltas; not sent.
}

/// One subsystem's reading plus its read-adjacent timestamp.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WSample<T> {
    pub mono_ns: u64,
    pub data: T,
}

/// One sampling pass, as sent per interval by the agent.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WireSnapshot {
    /// Pass start, nanoseconds since connection establishment.
    pub mono_ns: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sys: Option<WSysInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<WSample<Vec<WPortStat>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ifstats: Option<WSample<Vec<WIfStats>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvlink: Option<WSample<Vec<WNvLinkSnapshot>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xgmi: Option<WSample<Vec<WXgmiSnapshot>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub procs: Option<Vec<WProcessRdmaInfo>>,
}

// ---- conversions -----------------------------------------------------------

fn remote_to_str(r: &RemoteDeviceType) -> &'static str {
    match r {
        RemoteDeviceType::Gpu => "gpu",
        RemoteDeviceType::IbmNpu => "npu",
        RemoteDeviceType::Switch => "switch",
        RemoteDeviceType::Unknown => "unknown",
    }
}

fn remote_from_str(s: &str) -> RemoteDeviceType {
    match s {
        "gpu" => RemoteDeviceType::Gpu,
        "npu" => RemoteDeviceType::IbmNpu,
        "switch" => RemoteDeviceType::Switch,
        _ => RemoteDeviceType::Unknown,
    }
}

impl From<&HwCounter> for WHwCounter {
    fn from(c: &HwCounter) -> Self {
        Self {
            name: c.name.clone(),
            value: c.value,
        }
    }
}

impl From<&PortStat> for WPortStat {
    fn from(s: &PortStat) -> Self {
        Self {
            dev: s.dev_name.clone(),
            port: s.port,
            link_gbps: s.link_gbps,
            state: s.state.clone(),
            counters: s.counters.iter().map(Into::into).collect(),
        }
    }
}

impl From<&WPortStat> for PortStat {
    fn from(w: &WPortStat) -> Self {
        let mut s = PortStat::new(w.dev.clone(), w.port);
        s.link_gbps = w.link_gbps;
        s.state = w.state.clone();
        s.counters = w
            .counters
            .iter()
            .map(|c| HwCounter {
                name: c.name.clone(),
                value: c.value,
            })
            .collect();
        s
    }
}

impl From<&IfStats> for WIfStats {
    fn from(s: &IfStats) -> Self {
        Self {
            name: s.name.clone(),
            rx_bytes: s.rx_bytes,
            tx_bytes: s.tx_bytes,
        }
    }
}

impl From<&WIfStats> for IfStats {
    fn from(w: &WIfStats) -> Self {
        Self {
            name: w.name.clone(),
            rx_bytes: w.rx_bytes,
            tx_bytes: w.tx_bytes,
        }
    }
}

impl From<&GpuMetrics> for WGpuMetrics {
    fn from(m: &GpuMetrics) -> Self {
        Self {
            util_pct: m.util_pct,
            vram_used_mb: m.vram_used_mb,
            vram_total_mb: m.vram_total_mb,
            temp_c: m.temp_c,
            power_w: m.power_w,
            clock_mhz: m.clock_mhz,
        }
    }
}

impl From<&WGpuMetrics> for GpuMetrics {
    fn from(w: &WGpuMetrics) -> Self {
        Self {
            util_pct: w.util_pct,
            vram_used_mb: w.vram_used_mb,
            vram_total_mb: w.vram_total_mb,
            temp_c: w.temp_c,
            power_w: w.power_w,
            clock_mhz: w.clock_mhz,
        }
    }
}

impl From<&LinkSnapshot> for WLinkSnapshot {
    fn from(l: &LinkSnapshot) -> Self {
        Self {
            link_id: l.link_id,
            is_active: l.is_active,
            version: l.version,
            speed_gbps: l.speed_gbps,
            remote: remote_to_str(&l.remote_device_type).to_string(),
            remote_pci_bdf: l.remote_pci_bdf.clone(),
            tx_bytes: l.tx_bytes,
            rx_bytes: l.rx_bytes,
            crc_error_count: l.crc_error_count,
            replay_error_count: l.replay_error_count,
            recovery_error_count: l.recovery_error_count,
        }
    }
}

impl From<&WLinkSnapshot> for LinkSnapshot {
    fn from(w: &WLinkSnapshot) -> Self {
        Self {
            link_id: w.link_id,
            is_active: w.is_active,
            version: w.version,
            speed_gbps: w.speed_gbps,
            remote_device_type: remote_from_str(&w.remote),
            remote_pci_bdf: w.remote_pci_bdf.clone(),
            tx_bytes: w.tx_bytes,
            rx_bytes: w.rx_bytes,
            crc_error_count: w.crc_error_count,
            replay_error_count: w.replay_error_count,
            recovery_error_count: w.recovery_error_count,
        }
    }
}

impl From<&NvLinkSnapshot> for WNvLinkSnapshot {
    fn from(s: &NvLinkSnapshot) -> Self {
        Self {
            gpu_index: s.gpu_index,
            gpu_name: s.gpu_name.clone(),
            metrics: s.metrics.as_ref().map(Into::into),
            link_count: s.link_count,
            link_gbps: s.link_gbps,
            tx_bytes: s.tx_bytes,
            rx_bytes: s.rx_bytes,
            links: s.links.iter().map(Into::into).collect(),
        }
    }
}

impl From<&WNvLinkSnapshot> for NvLinkSnapshot {
    fn from(w: &WNvLinkSnapshot) -> Self {
        Self {
            gpu_index: w.gpu_index,
            gpu_name: w.gpu_name.clone(),
            metrics: w.metrics.as_ref().map(Into::into),
            link_count: w.link_count,
            link_gbps: w.link_gbps,
            tx_bytes: w.tx_bytes,
            rx_bytes: w.rx_bytes,
            links: w.links.iter().map(Into::into).collect(),
        }
    }
}

impl From<&XgmiLinkSnapshot> for WXgmiLinkSnapshot {
    fn from(l: &XgmiLinkSnapshot) -> Self {
        Self {
            link_id: l.link_id,
            is_active: l.is_active,
            speed_gbps: l.speed_gbps,
            bit_rate_gbps: l.bit_rate_gbps,
            remote_pci_bdf: l.remote_pci_bdf.clone(),
            tx_bytes: l.tx_bytes,
            rx_bytes: l.rx_bytes,
        }
    }
}

impl From<&WXgmiLinkSnapshot> for XgmiLinkSnapshot {
    fn from(w: &WXgmiLinkSnapshot) -> Self {
        Self {
            link_id: w.link_id,
            is_active: w.is_active,
            speed_gbps: w.speed_gbps,
            bit_rate_gbps: w.bit_rate_gbps,
            remote_pci_bdf: w.remote_pci_bdf.clone(),
            tx_bytes: w.tx_bytes,
            rx_bytes: w.rx_bytes,
        }
    }
}

impl From<&XgmiSnapshot> for WXgmiSnapshot {
    fn from(s: &XgmiSnapshot) -> Self {
        Self {
            gpu_index: s.gpu_index,
            gpu_name: s.gpu_name.clone(),
            link_count: s.link_count,
            link_gbps: s.link_gbps,
            correctable_errors: s.correctable_errors,
            uncorrectable_errors: s.uncorrectable_errors,
            metrics: s.metrics.as_ref().map(Into::into),
            links: s.links.iter().map(Into::into).collect(),
        }
    }
}

impl From<&WXgmiSnapshot> for XgmiSnapshot {
    fn from(w: &WXgmiSnapshot) -> Self {
        Self {
            gpu_index: w.gpu_index,
            gpu_name: w.gpu_name.clone(),
            link_count: w.link_count,
            link_gbps: w.link_gbps,
            correctable_errors: w.correctable_errors,
            uncorrectable_errors: w.uncorrectable_errors,
            metrics: w.metrics.as_ref().map(Into::into),
            links: w.links.iter().map(Into::into).collect(),
        }
    }
}

impl From<&ProcessRdmaInfo> for WProcessRdmaInfo {
    fn from(p: &ProcessRdmaInfo) -> Self {
        Self {
            pid: p.pid,
            dev: p.dev_name.clone(),
            qp_count: p.qp_count,
            user: p.user.clone(),
            nice: p.nice,
            state: p.state.to_string(),
            virt_kb: p.virt_kb,
            res_kb: p.res_kb,
            shr_kb: p.shr_kb,
            mem_pct: p.mem_pct,
            threads: p.threads,
            cmdline: p.cmdline.clone(),
        }
    }
}

impl From<&WProcessRdmaInfo> for ProcessRdmaInfo {
    fn from(w: &WProcessRdmaInfo) -> Self {
        Self {
            pid: w.pid,
            dev_name: w.dev.clone(),
            qp_count: w.qp_count,
            user: w.user.clone(),
            nice: w.nice,
            state: w.state.chars().next().unwrap_or('?'),
            virt_kb: w.virt_kb,
            res_kb: w.res_kb,
            shr_kb: w.shr_kb,
            mem_pct: w.mem_pct,
            threads: w.threads,
            cmdline: w.cmdline.clone(),
        }
    }
}

impl From<&SysInfo> for WSysInfo {
    fn from(s: &SysInfo) -> Self {
        Self {
            hostname: s.hostname.clone(),
            uptime: s.uptime.clone(),
            load_avg: s.load_avg.clone(),
            mem_total_mb: s.mem_total_mb,
            mem_used_mb: s.mem_used_mb,
            mem_pct: s.mem_pct,
            cpu_pct: s.cpu_pct,
        }
    }
}

impl From<&WSysInfo> for SysInfo {
    fn from(w: &WSysInfo) -> Self {
        Self {
            hostname: w.hostname.clone(),
            uptime: w.uptime.clone(),
            load_avg: w.load_avg.clone(),
            mem_total_mb: w.mem_total_mb,
            mem_used_mb: w.mem_used_mb,
            mem_pct: w.mem_pct,
            cpu_pct: w.cpu_pct,
            net: Default::default(),
        }
    }
}

/// Convert a local snapshot to the wire form. `mono` maps `Instant`s to
/// nanoseconds since the connection anchor.
pub fn to_wire(snap: &Snapshot, mono: impl Fn(std::time::Instant) -> u64) -> WireSnapshot {
    WireSnapshot {
        mono_ns: mono(snap.taken_at),
        sys: snap.sys.as_ref().map(Into::into),
        stats: snap.stats.as_ref().map(|s| WSample {
            mono_ns: mono(s.taken_at),
            data: s.data.iter().map(Into::into).collect(),
        }),
        ifstats: snap.ifstats.as_ref().map(|s| WSample {
            mono_ns: mono(s.taken_at),
            data: s.data.iter().map(Into::into).collect(),
        }),
        nvlink: snap.nvlink.as_ref().map(|s| WSample {
            mono_ns: mono(s.taken_at),
            data: s.data.iter().map(Into::into).collect(),
        }),
        xgmi: snap.xgmi.as_ref().map(|s| WSample {
            mono_ns: mono(s.taken_at),
            data: s.data.iter().map(Into::into).collect(),
        }),
        procs: snap
            .processes
            .as_ref()
            .map(|p| p.iter().map(Into::into).collect()),
    }
}

/// Rebuild a local snapshot from the wire form. `instant` maps nanoseconds
/// since the connection anchor back to an `Instant` on this host.
pub fn from_wire(w: &WireSnapshot, instant: impl Fn(u64) -> std::time::Instant) -> Snapshot {
    Snapshot {
        taken_at: instant(w.mono_ns),
        sys: w.sys.as_ref().map(Into::into),
        stats: w.stats.as_ref().map(|s| Sample {
            taken_at: instant(s.mono_ns),
            data: s.data.iter().map(Into::into).collect(),
        }),
        ifstats: w.ifstats.as_ref().map(|s| Sample {
            taken_at: instant(s.mono_ns),
            data: s.data.iter().map(Into::into).collect(),
        }),
        nvlink: w.nvlink.as_ref().map(|s| Sample {
            taken_at: instant(s.mono_ns),
            data: s.data.iter().map(Into::into).collect(),
        }),
        xgmi: w.xgmi.as_ref().map(|s| Sample {
            taken_at: instant(s.mono_ns),
            data: s.data.iter().map(Into::into).collect(),
        }),
        processes: w.procs.as_ref().map(|p| p.iter().map(Into::into).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn port_stat(dev: &str, port: u32, counters: &[(&str, u64)]) -> PortStat {
        let mut s = PortStat::new(dev.to_string(), port);
        for (name, value) in counters {
            s.counters.push(HwCounter {
                name: name.to_string(),
                value: *value,
            });
        }
        s
    }

    fn canned_snapshot() -> Snapshot {
        let anchor = Instant::now();
        Snapshot {
            stats: Some(Sample {
                data: vec![port_stat(
                    "mlx5_0",
                    1,
                    &[("tx_bytes", 1000), ("rx_bytes", 500)],
                )],
                taken_at: anchor,
            }),
            ifstats: Some(Sample {
                data: vec![IfStats {
                    name: "eth0".into(),
                    rx_bytes: 10,
                    tx_bytes: 20,
                }],
                taken_at: anchor,
            }),
            nvlink: None,
            xgmi: None,
            processes: Some(vec![ProcessRdmaInfo {
                pid: 42,
                dev_name: "mlx5_0".into(),
                qp_count: 3,
                user: "root".into(),
                nice: 0,
                state: 'R',
                virt_kb: 100,
                res_kb: 50,
                shr_kb: 10,
                mem_pct: 0.5,
                threads: 4,
                cmdline: "nccl".into(),
            }]),
            sys: Some(SysInfo {
                hostname: "node1".into(),
                uptime: "up 1 days, 0:00".into(),
                load_avg: "0.1, 0.2, 0.3".into(),
                mem_total_mb: 1000,
                mem_used_mb: 500,
                mem_pct: 50.0,
                cpu_pct: 12.5,
                net: Default::default(),
            }),
            taken_at: anchor,
        }
    }

    #[test]
    fn wire_roundtrip_preserves_fields() {
        let anchor = Instant::now();
        let mono = |i: Instant| i.duration_since(anchor).as_nanos() as u64;
        let instant = |ns: u64| anchor + Duration::from_nanos(ns);

        let snap = canned_snapshot();
        let wire = to_wire(&snap, mono);
        let json = serde_json::to_string(&wire).unwrap();
        let parsed: WireSnapshot = serde_json::from_str(&json).unwrap();
        let back = from_wire(&parsed, instant);

        let stats = back.stats.expect("stats survived");
        assert_eq!(stats.data.len(), 1);
        assert_eq!(stats.data[0].dev_name, "mlx5_0");
        assert_eq!(stats.data[0].counter_value("tx_bytes"), Some(1000));

        let procs = back.processes.expect("procs survived");
        assert_eq!(procs[0].pid, 42);
        assert_eq!(procs[0].state, 'R');
        assert_eq!(procs[0].qp_count, 3);

        let sys = back.sys.expect("sys survived");
        assert_eq!(sys.hostname, "node1");
        assert_eq!(sys.mem_pct, 50.0);

        let ifs = back.ifstats.expect("ifstats survived");
        assert_eq!(ifs.data[0].name, "eth0");
        assert_eq!(ifs.data[0].tx_bytes, 20);
    }

    #[test]
    fn wire_json_uses_expected_shape() {
        let mono = |_: Instant| 7u64;
        let wire = to_wire(&canned_snapshot(), mono);
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains("mono_ns"));
        assert!(json.contains("mlx5_0"));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let json = r#"{"mono_ns":1,"sys":{"hostname":"x","uptime":"y","load_avg":"z",
            "mem_total_mb":1,"mem_used_mb":1,"mem_pct":1.0,"cpu_pct":1.0},
            "future_field":123}"#;
        let w: WireSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(w.mono_ns, 1);
        assert_eq!(w.sys.unwrap().hostname, "x");
    }

    #[test]
    fn nan_in_wire_data_serializes_without_error() {
        // Documents what the agent does when a subsystem yields NaN (e.g. a
        // sysfs rate file containing "nan"): serde_json must not kill the
        // whole frame stream.
        let mut ps = PortStat::new("mlx5_bond_0".into(), 1);
        ps.link_gbps = Some(f64::NAN);
        ps.counters.push(HwCounter {
            name: "tx_bytes".into(),
            value: 1,
        });
        let snap = Snapshot {
            stats: Some(Sample {
                data: vec![ps],
                taken_at: Instant::now(),
            }),
            ifstats: None,
            nvlink: None,
            xgmi: None,
            processes: None,
            sys: None,
            taken_at: Instant::now(),
        };
        let anchor = Instant::now();
        let mono = |i: Instant| i.duration_since(anchor).as_nanos() as u64;
        let wire = to_wire(&snap, mono);
        match serde_json::to_string(&wire) {
            Ok(json) => {
                // If serialization succeeds, the NaN must round-trip losslessly
                // (as null or a number) without breaking the viewer's parse.
                let parsed: Result<WireSnapshot, _> = serde_json::from_str(&json);
                assert!(parsed.is_ok(), "viewer cannot parse agent frame: {json}");
            }
            Err(e) => {
                panic!("agent cannot serialize NaN frame — frames would stop: {e}");
            }
        }
    }

    #[test]
    fn hello_parses_defaults() {
        let h: Hello = serde_json::from_str(r#"{"v":1}"#).unwrap();
        assert_eq!(h.interval_ms, 1000);
        assert!(h.token.is_none());
    }

    #[test]
    fn control_interval_roundtrip() {
        let c = serde_json::to_string(&Control::Interval { ms: 250 }).unwrap();
        assert_eq!(c, "{\"t\":\"interval\",\"ms\":250}");
        let parsed: Control = serde_json::from_str(&c).unwrap();
        match parsed {
            Control::Interval { ms } => assert_eq!(ms, 250),
            _ => panic!("wrong variant"),
        }
    }
}
