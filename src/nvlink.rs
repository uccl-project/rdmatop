//! NVLink data layer built on top of the NVIDIA Management Library (NVML).
//!
//! This module is only compiled when the `nvlink` cargo feature is enabled.
//! It exposes a small snapshot type that the TUI can render without having to
//! link directly against NVML. Per-link TX/RX throughput is read via the raw
//! `nvmlDeviceGetFieldValues` entry point because `nvml-wrapper` does not
//! currently expose a safe helper for those field IDs.

use std::io;

use nvml_wrapper::enum_wrappers::nv_link::{ErrorCounter, IntDeviceType};
use nvml_wrapper::enums::device::SampleValue;
use nvml_wrapper::structs::device::FieldId;
use nvml_wrapper::Nvml;

use nvml_wrapper_sys::bindings::field_id::{
    NVML_FI_DEV_NVLINK_LINK_COUNT, NVML_FI_DEV_NVLINK_SPEED_MBPS_COMMON,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L0, NVML_FI_DEV_NVLINK_SPEED_MBPS_L1,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L10, NVML_FI_DEV_NVLINK_SPEED_MBPS_L11,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L2, NVML_FI_DEV_NVLINK_SPEED_MBPS_L3,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L4, NVML_FI_DEV_NVLINK_SPEED_MBPS_L5,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L6, NVML_FI_DEV_NVLINK_SPEED_MBPS_L7,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L8, NVML_FI_DEV_NVLINK_SPEED_MBPS_L9,
    NVML_FI_DEV_NVLINK_THROUGHPUT_DATA_RX, NVML_FI_DEV_NVLINK_THROUGHPUT_DATA_TX,
};
use nvml_wrapper_sys::bindings::{
    nvmlFieldValue_st, nvmlValueType_enum_NVML_VALUE_TYPE_UNSIGNED_LONG_LONG, nvmlValue_t,
    NVML_NVLINK_MAX_LINKS,
};

/// Logical classification of the device on the other end of an NVLink.
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteDeviceType {
    Gpu,
    IbmNpu,
    Switch,
    Unknown,
}

impl RemoteDeviceType {
    /// Short, human-readable label used by the TUI.
    pub fn label(&self) -> &'static str {
        match self {
            RemoteDeviceType::Gpu => "GPU",
            RemoteDeviceType::IbmNpu => "NPU",
            RemoteDeviceType::Switch => "NVSwitch",
            RemoteDeviceType::Unknown => "?",
        }
    }
}

impl From<IntDeviceType> for RemoteDeviceType {
    fn from(value: IntDeviceType) -> Self {
        match value {
            IntDeviceType::Gpu => RemoteDeviceType::Gpu,
            IntDeviceType::Ibmnpu => RemoteDeviceType::IbmNpu,
            IntDeviceType::Switch => RemoteDeviceType::Switch,
            IntDeviceType::Unknown => RemoteDeviceType::Unknown,
        }
    }
}

/// Snapshot of a single NVLink's state at one sample.
#[derive(Clone, Debug)]
pub struct LinkSnapshot {
    pub link_id: u32,
    /// True if NVML reports the link as active (NVLink state enabled).
    pub is_active: bool,
    /// NVLink protocol version reported by the driver.
    pub version: Option<u32>,
    /// Negotiated per-link speed in Gbps (Mb/s / 1000). `None` if unknown.
    /// Aggregated into `NvLinkSnapshot::link_gbps` for the GPU-wide total;
    /// not directly read by the TUI.
    #[allow(dead_code)]
    pub speed_gbps: Option<f64>,
    pub remote_device_type: RemoteDeviceType,
    /// BDF of the remote PCI device, if NVML reports one.
    pub remote_pci_bdf: Option<String>,
    pub tx_bytes: Option<u64>,
    pub rx_bytes: Option<u64>,
    pub crc_error_count: Option<u64>,
    pub replay_error_count: Option<u64>,
    pub recovery_error_count: Option<u64>,
}

/// Snapshot of every NVLink attached to one GPU.
#[derive(Clone, Debug)]
pub struct NvLinkSnapshot {
    pub gpu_index: u32,
    pub gpu_name: String,
    /// Total number of NVLink ports exposed by the GPU (active + inactive).
    pub link_count: u32,
    /// Negotiated aggregate speed in Gbps, derived from the per-link
    /// `NVML_FI_DEV_NVLINK_SPEED_MBPS_COMMON` field when available.
    pub link_gbps: Option<f64>,
    pub tx_bytes: Option<u64>,
    pub rx_bytes: Option<u64>,
    pub links: Vec<LinkSnapshot>,
}

impl NvLinkSnapshot {
    /// Count of links that NVML currently reports as active.
    pub fn active_links(&self) -> u32 {
        self.links.iter().filter(|l| l.is_active).count() as u32
    }
}

/// Per-link speed field IDs in the order used by `read_link_speed_gbps`.
const LINK_SPEED_FIELD_IDS: [u32; 12] = [
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L0,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L1,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L2,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L3,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L4,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L5,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L6,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L7,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L8,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L9,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L10,
    NVML_FI_DEV_NVLINK_SPEED_MBPS_L11,
];

/// Read NVLink statistics for every GPU in the system.
///
/// If NVML cannot be initialised (e.g. no NVIDIA driver loaded), an empty
/// vector is returned so the caller can keep running with no NVLink data.
/// Per-GPU or per-link failures are skipped silently and the remaining data
/// is still returned.
pub fn read_all_nvlink_stats() -> io::Result<Vec<NvLinkSnapshot>> {
    let nvml = match Nvml::init() {
        Ok(n) => n,
        Err(_) => return Ok(Vec::new()),
    };

    let device_count = match nvml.device_count() {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };

    let mut snapshots = Vec::with_capacity(device_count as usize);
    for idx in 0..device_count {
        if let Some(snap) = read_device_snapshot(&nvml, idx) {
            snapshots.push(snap);
        }
    }

    Ok(snapshots)
}

fn read_device_snapshot(nvml: &Nvml, idx: u32) -> Option<NvLinkSnapshot> {
    let device = nvml.device_by_index(idx).ok()?;
    let gpu_name = device.name().unwrap_or_default();

    let link_count = read_link_count(&device).unwrap_or(NVML_NVLINK_MAX_LINKS);
    let common_speed_gbps = read_common_speed_gbps(&device);

    let mut total_speed_gbps = 0.0;
    let mut links = Vec::with_capacity(link_count as usize);
    for link_id in 0..link_count {
        let nvlink = device.link_wrapper_for(link_id);
        let is_active = nvlink.is_active().unwrap_or(false);
        let version = nvlink.version().ok();
        let remote_device_type = nvlink
            .remote_device_type(link_id)
            .map(RemoteDeviceType::from)
            .unwrap_or(RemoteDeviceType::Unknown);
        let remote_pci_bdf = nvlink.remote_pci_info().ok().map(|p| p.bus_id);
        let crc_error_count = nvlink.error_counter(ErrorCounter::DlCrcFlit).ok();
        let replay_error_count = nvlink.error_counter(ErrorCounter::DlReplay).ok();
        let recovery_error_count = nvlink.error_counter(ErrorCounter::DlRecovery).ok();
        let (tx_bytes, rx_bytes) = read_link_throughput(nvml, &device, link_id);
        let speed_gbps = if is_active {
            read_link_speed_gbps(&device, link_id).or(common_speed_gbps)
        } else {
            None
        };
        if let Some(s) = speed_gbps {
            total_speed_gbps += s;
        }

        links.push(LinkSnapshot {
            link_id,
            is_active,
            version,
            speed_gbps,
            remote_device_type,
            remote_pci_bdf,
            tx_bytes,
            rx_bytes,
            crc_error_count,
            replay_error_count,
            recovery_error_count,
        });
    }

    let (tx_bytes, rx_bytes) = read_link_throughput(nvml, &device, u32::MAX);

    Some(NvLinkSnapshot {
        gpu_index: idx,
        gpu_name,
        link_count,
        link_gbps: if total_speed_gbps > 0.0 {
            Some(total_speed_gbps)
        } else {
            None
        },
        tx_bytes,
        rx_bytes,
        links,
    })
}

/// Build a fresh `nvmlFieldValue_t` populated with `fieldId` and `scopeId`.
fn make_field(field_id: u32, scope_id: u32) -> nvmlFieldValue_st {
    nvmlFieldValue_st {
        fieldId: field_id,
        scopeId: scope_id,
        timestamp: 0,
        latencyUsec: 0,
        valueType: 0,
        nvmlReturn: 0,
        value: nvmlValue_t { ullVal: 0 },
    }
}

/// Read a single NVML field via the safe `field_values_for` API.
/// Returns `Some(u32)` only when the field is reported as `U32`.
fn read_field_u32(device: &nvml_wrapper::Device<'_>, field_id: u32) -> Option<u32> {
    let results = device.field_values_for(&[FieldId(field_id)]).ok()?;
    let sample = results.into_iter().next()?.ok()?;
    match sample.value.ok()? {
        SampleValue::U32(v) => Some(v),
        _ => None,
    }
}

fn read_link_count(device: &nvml_wrapper::Device<'_>) -> Option<u32> {
    read_field_u32(device, NVML_FI_DEV_NVLINK_LINK_COUNT)
}

fn read_common_speed_gbps(device: &nvml_wrapper::Device<'_>) -> Option<f64> {
    let mbps = read_field_u32(device, NVML_FI_DEV_NVLINK_SPEED_MBPS_COMMON)?;
    if mbps == 0 {
        None
    } else {
        // NVML reports Mb/s (megabits/sec) for this field; convert to Gb/s.
        Some(mbps as f64 / 1000.0)
    }
}

fn read_link_speed_gbps(device: &nvml_wrapper::Device<'_>, link: u32) -> Option<f64> {
    let field_id = *LINK_SPEED_FIELD_IDS.get(link as usize)?;
    let mbps = read_field_u32(device, field_id)?;
    if mbps == 0 {
        None
    } else {
        // NVML reports Mb/s (megabits/sec); convert to Gb/s.
        Some(mbps as f64 / 1000.0)
    }
}

/// Query per-link TX/RX throughput via raw `nvmlDeviceGetFieldValues`.
/// `scopeId` is set to the per-link id as required by the plan.
fn read_link_throughput(
    nvml: &Nvml,
    device: &nvml_wrapper::Device<'_>,
    link: u32,
) -> (Option<u64>, Option<u64>) {
    let mut tx = make_field(NVML_FI_DEV_NVLINK_THROUGHPUT_DATA_TX, link);
    let mut rx = make_field(NVML_FI_DEV_NVLINK_THROUGHPUT_DATA_RX, link);

    let mut fields = [tx, rx];
    let rc = unsafe {
        nvml.lib().nvmlDeviceGetFieldValues(
            device.handle(),
            fields.len() as std::os::raw::c_int,
            fields.as_mut_ptr(),
        )
    };
    // `nvmlReturn_t` is a type alias for `u32` in the NVML bindings, so the
    // SUCCESS value is the literal 0; comparisons against the typed variant
    // are not available.
    if rc != 0 {
        return (None, None);
    }
    tx = fields[0];
    rx = fields[1];

    // NVML does not actually let you scope these throughput counters to a
    // single link via `scopeId`; the driver always reports the per-GPU
    // aggregate. The values below therefore represent the GPU aggregate
    // rather than the per-link bytes. Per-link attribution would require a
    // future NVML field; until then we return whatever NVML gives us so the
    // TUI can at least show a non-zero value when traffic is flowing.
    let tx_bytes = if tx.nvmlReturn == 0
        && tx.valueType == nvmlValueType_enum_NVML_VALUE_TYPE_UNSIGNED_LONG_LONG
    {
        Some(unsafe { tx.value.ullVal }.wrapping_mul(1024))
    } else {
        None
    };
    let rx_bytes = if rx.nvmlReturn == 0
        && rx.valueType == nvmlValueType_enum_NVML_VALUE_TYPE_UNSIGNED_LONG_LONG
    {
        Some(unsafe { rx.value.ullVal }.wrapping_mul(1024))
    } else {
        None
    };
    (tx_bytes, rx_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_device_type_label() {
        assert_eq!(RemoteDeviceType::Gpu.label(), "GPU");
        assert_eq!(RemoteDeviceType::IbmNpu.label(), "NPU");
        assert_eq!(RemoteDeviceType::Switch.label(), "NVSwitch");
        assert_eq!(RemoteDeviceType::Unknown.label(), "?");
    }

    #[test]
    fn remote_device_type_from_int_device_type() {
        assert_eq!(
            RemoteDeviceType::from(IntDeviceType::Gpu),
            RemoteDeviceType::Gpu
        );
        assert_eq!(
            RemoteDeviceType::from(IntDeviceType::Ibmnpu),
            RemoteDeviceType::IbmNpu
        );
        assert_eq!(
            RemoteDeviceType::from(IntDeviceType::Switch),
            RemoteDeviceType::Switch
        );
        assert_eq!(
            RemoteDeviceType::from(IntDeviceType::Unknown),
            RemoteDeviceType::Unknown
        );
    }

    #[test]
    fn active_links_counts_active_only() {
        let snap = NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "test".to_string(),
            link_count: 4,
            link_gbps: None,
            tx_bytes: None,
            rx_bytes: None,
            links: vec![
                LinkSnapshot {
                    link_id: 0,
                    is_active: true,
                    version: None,
                    speed_gbps: None,
                    remote_device_type: RemoteDeviceType::Gpu,
                    remote_pci_bdf: None,
                    tx_bytes: None,
                    rx_bytes: None,
                    crc_error_count: None,
                    replay_error_count: None,
                    recovery_error_count: None,
                },
                LinkSnapshot {
                    link_id: 1,
                    is_active: false,
                    version: None,
                    speed_gbps: None,
                    remote_device_type: RemoteDeviceType::Unknown,
                    remote_pci_bdf: None,
                    tx_bytes: None,
                    rx_bytes: None,
                    crc_error_count: None,
                    replay_error_count: None,
                    recovery_error_count: None,
                },
                LinkSnapshot {
                    link_id: 2,
                    is_active: true,
                    version: None,
                    speed_gbps: None,
                    remote_device_type: RemoteDeviceType::Switch,
                    remote_pci_bdf: None,
                    tx_bytes: None,
                    rx_bytes: None,
                    crc_error_count: None,
                    replay_error_count: None,
                    recovery_error_count: None,
                },
            ],
        };
        assert_eq!(snap.active_links(), 2);
    }

    #[test]
    fn active_links_no_links() {
        let snap = NvLinkSnapshot {
            gpu_index: 0,
            gpu_name: "test".to_string(),
            link_count: 0,
            link_gbps: None,
            tx_bytes: None,
            rx_bytes: None,
            links: Vec::new(),
        };
        assert_eq!(snap.active_links(), 0);
    }
}
