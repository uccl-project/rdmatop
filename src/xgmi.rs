//! XGMI data layer built on top of AMD SMI (`libamd_smi`), compiled only
//! with the `xgmi` cargo feature. The library is dlopen'd at runtime; when
//! absent or failing to init, `read_all_xgmi_stats` returns an empty vector.

use std::ffi::CStr;
use std::io;
use std::ptr;
use std::sync::OnceLock;

/// Snapshot of a single XGMI link at one sample.
#[derive(Clone, Debug)]
pub struct XgmiLinkSnapshot {
    /// Peer GPU index (also indexes amdsmi's per-peer accumulators); not
    /// contiguous since the GPU's own index is skipped.
    pub link_id: u32,
    pub is_active: bool,
    /// Link max bandwidth in Gb/s (e.g. 512 on MI325X). Used for bar scaling.
    pub speed_gbps: Option<f64>,
    /// Per-lane bit rate in Gb/s (e.g. 32). Reserved for future column display.
    #[allow(dead_code)]
    pub bit_rate_gbps: Option<f64>,
    /// BDF of the peer GPU on the other end of the link.
    pub remote_pci_bdf: Option<String>,
    /// Accumulated bytes written to the link (amdsmi `write` KB * 1024).
    pub tx_bytes: Option<u64>,
    /// Accumulated bytes read from the link (amdsmi `read` KB * 1024).
    pub rx_bytes: Option<u64>,
}

/// Snapshot of every XGMI link attached to one GPU.
#[derive(Clone, Debug)]
pub struct XgmiSnapshot {
    pub gpu_index: u32,
    pub gpu_name: String,
    /// Number of topology-confirmed XGMI peer links.
    pub link_count: u32,
    /// Sum of active links' max bandwidth in Gb/s.
    pub link_gbps: Option<f64>,
    /// XGMI_WAFL block accumulated ECC counts (per-GPU, not per-link).
    pub correctable_errors: Option<u64>,
    pub uncorrectable_errors: Option<u64>,
    pub links: Vec<XgmiLinkSnapshot>,
}

impl XgmiSnapshot {
    /// Count of links currently reported as up.
    pub fn active_links(&self) -> u32 {
        self.links.iter().filter(|l| l.is_active).count() as u32
    }
}

/// Render an `amdsmi_bdf_t` u64 as "dddd:bb:dd.f" (domain:bus:device.function).
/// Bit layout (LSB first): function 3 bits, device 5, bus 8, domain 48.
fn format_bdf(bdf: u64) -> String {
    let function = bdf & 0x7;
    let device = (bdf >> 3) & 0x1f;
    let bus = (bdf >> 8) & 0xff;
    let domain = (bdf >> 16) & 0xffff_ffff_ffff;
    format!("{:04x}:{:02x}:{:02x}.{:x}", domain, bus, device, function)
}

/// Minimal hand-rolled bindings for the amdsmi entry points rdmatop needs.
/// Layouts are pinned to the ROCm 6.x ABI and guarded by size/offset
/// assertions below; see amdsmi.h in ROCm for the reference definitions.
mod ffi {
    use std::os::raw::{c_char, c_void};

    pub type AmdsmiStatus = u32;
    pub type SocketHandle = *mut c_void;
    pub type ProcessorHandle = *mut c_void;

    pub const AMDSMI_STATUS_SUCCESS: AmdsmiStatus = 0;
    pub const AMDSMI_INIT_AMD_GPUS: u64 = 1 << 1;
    pub const AMDSMI_MAX_STRING_LENGTH: usize = 256;
    pub const AMDSMI_MAX_NUM_XGMI_PHYSICAL_LINK: usize = 64;
    pub const AMDSMI_IOLINK_TYPE_XGMI: u32 = 2;
    pub const AMDSMI_GPU_BLOCK_XGMI_WAFL: u64 = 0x80;

    /// One entry of `amdsmi_link_metrics_t.links[]`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct LinkMetricsEntry {
        pub bdf: u64,
        pub bit_rate: u32,      // current link speed in Gb/s
        pub max_bandwidth: u32, // max link bandwidth in Gb/s
        pub link_type: u32,     // amdsmi_link_type_t
        pub read_kb: u64,       // total data received per link (KB)
        pub write_kb: u64,      // total data transmitted per link (KB)
        pub reserved: [u64; 2],
    }

    #[repr(C)]
    pub struct LinkMetrics {
        pub num_links: u32,
        pub links: [LinkMetricsEntry; AMDSMI_MAX_NUM_XGMI_PHYSICAL_LINK],
        pub reserved: [u64; 7],
    }

    #[repr(C)]
    pub struct AsicInfo {
        pub market_name: [c_char; AMDSMI_MAX_STRING_LENGTH],
        pub vendor_id: u32,
        pub vendor_name: [c_char; AMDSMI_MAX_STRING_LENGTH],
        pub subvendor_id: u32,
        pub device_id: u64,
        pub rev_id: u32,
        pub asic_serial: [c_char; AMDSMI_MAX_STRING_LENGTH],
        pub oam_id: u32,
        pub num_of_compute_units: u32,
        pub target_graphics_version: u64,
        pub reserved: [u32; 22],
    }

    #[repr(C)]
    pub struct ErrorCount {
        pub correctable_count: u64,
        pub uncorrectable_count: u64,
        pub deferred_count: u64,
        pub reserved: [u64; 5],
    }

    /// Prefix of `amdsmi_pcie_info_t` (512 bytes total). Only the two leading
    /// `pcie_static` fields are read; the remainder is opaque padding.
    #[repr(C)]
    pub struct PcieInfo {
        pub max_pcie_width: u16,
        pub max_pcie_speed: u32, // MT/s at runtime (header comment says GT/s)
        pub _rest: [u64; 63],
    }

    // ABI guards: sizes/offsets computed from amdsmi.h (ROCm 6.4.2). A silent
    // layout drift would corrupt every field after the divergence point.
    const _: () = assert!(std::mem::size_of::<LinkMetricsEntry>() == 56);
    const _: () = assert!(std::mem::offset_of!(LinkMetricsEntry, read_kb) == 24);
    const _: () = assert!(std::mem::size_of::<LinkMetrics>() == 3648);
    const _: () = assert!(std::mem::offset_of!(LinkMetrics, links) == 8);
    const _: () = assert!(std::mem::size_of::<AsicInfo>() == 896);
    const _: () = assert!(std::mem::offset_of!(AsicInfo, device_id) == 520);
    const _: () = assert!(std::mem::size_of::<ErrorCount>() == 64);
    const _: () = assert!(std::mem::size_of::<PcieInfo>() == 512);
    const _: () = assert!(std::mem::offset_of!(PcieInfo, max_pcie_speed) == 4);
    const _: () = assert!(std::mem::offset_of!(PcieInfo, _rest) == 8);
}

/// Resolved amdsmi function pointers. `_lib` keeps the dlopen handle alive
/// for the lifetime of the pointers.
struct Amdsmi {
    _lib: libloading::Library,
    get_socket_handles: unsafe extern "C" fn(*mut u32, *mut ffi::SocketHandle) -> ffi::AmdsmiStatus,
    get_processor_handles: unsafe extern "C" fn(
        ffi::SocketHandle,
        *mut u32,
        *mut ffi::ProcessorHandle,
    ) -> ffi::AmdsmiStatus,
    get_gpu_asic_info:
        unsafe extern "C" fn(ffi::ProcessorHandle, *mut ffi::AsicInfo) -> ffi::AmdsmiStatus,
    get_link_metrics:
        unsafe extern "C" fn(ffi::ProcessorHandle, *mut ffi::LinkMetrics) -> ffi::AmdsmiStatus,
    get_gpu_device_bdf: unsafe extern "C" fn(ffi::ProcessorHandle, *mut u64) -> ffi::AmdsmiStatus,
    get_pcie_info:
        unsafe extern "C" fn(ffi::ProcessorHandle, *mut ffi::PcieInfo) -> ffi::AmdsmiStatus,
    topo_get_link_type: unsafe extern "C" fn(
        ffi::ProcessorHandle,
        ffi::ProcessorHandle,
        *mut u64,
        *mut u32,
    ) -> ffi::AmdsmiStatus,
    get_gpu_ecc_count:
        unsafe extern "C" fn(ffi::ProcessorHandle, u64, *mut ffi::ErrorCount) -> ffi::AmdsmiStatus,
}

/// Library names tried in order: bare sonames first (ld.so search path),
/// then the conventional ROCm install location.
const LIB_CANDIDATES: [&str; 3] = [
    "libamd_smi.so",
    "libamd_smi.so.25",
    "/opt/rocm/lib/libamd_smi.so",
];

impl Amdsmi {
    fn load() -> Option<Self> {
        for path in LIB_CANDIDATES {
            if let Ok(lib) = unsafe { libloading::Library::new(path) } {
                if let Some(api) = Self::from_lib(lib) {
                    return Some(api);
                }
            }
        }
        None
    }

    fn from_lib(lib: libloading::Library) -> Option<Self> {
        // Fn pointers are copied out of the Symbol wrappers; they stay valid
        // because `_lib` is stored alongside them.
        unsafe {
            let init: unsafe extern "C" fn(u64) -> ffi::AmdsmiStatus =
                *lib.get(b"amdsmi_init\0").ok()?;
            let api = Amdsmi {
                get_socket_handles: *lib.get(b"amdsmi_get_socket_handles\0").ok()?,
                get_processor_handles: *lib.get(b"amdsmi_get_processor_handles\0").ok()?,
                get_gpu_asic_info: *lib.get(b"amdsmi_get_gpu_asic_info\0").ok()?,
                get_link_metrics: *lib.get(b"amdsmi_get_link_metrics\0").ok()?,
                get_gpu_device_bdf: *lib.get(b"amdsmi_get_gpu_device_bdf\0").ok()?,
                get_pcie_info: *lib.get(b"amdsmi_get_pcie_info\0").ok()?,
                topo_get_link_type: *lib.get(b"amdsmi_topo_get_link_type\0").ok()?,
                get_gpu_ecc_count: *lib.get(b"amdsmi_get_gpu_ecc_count\0").ok()?,
                _lib: lib,
            };
            if init(ffi::AMDSMI_INIT_AMD_GPUS) != ffi::AMDSMI_STATUS_SUCCESS {
                return None;
            }
            Some(api)
        }
    }
}

/// dlopen + amdsmi_init exactly once per process; amdsmi_shut_down is never
/// called (process exit tears it down). A failed init is cached as `None`.
fn instance() -> Option<&'static Amdsmi> {
    static INSTANCE: OnceLock<Option<Amdsmi>> = OnceLock::new();
    INSTANCE.get_or_init(Amdsmi::load).as_ref()
}

/// C out-param wrapper with trailing headroom so a newer libamd_smi whose
/// struct outgrew our pinned ROCm 6.x layout cannot overrun the allocation.
#[repr(C)]
struct Padded<T> {
    value: T,
    _headroom: [u8; 4096],
}

impl<T> Padded<T> {
    /// All-integer repr(C) PODs only, mirroring the zeroed C-side init.
    fn zeroed() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// Read XGMI statistics for every AMD GPU in the system. Missing library or
/// zero GPUs yield an empty vector; per-GPU failures skip that GPU and
/// per-field failures degrade to `None`.
pub fn read_all_xgmi_stats() -> io::Result<Vec<XgmiSnapshot>> {
    let api = match instance() {
        Some(a) => a,
        None => return Ok(Vec::new()),
    };

    let handles = enumerate_gpus(api);
    if handles.is_empty() {
        return Ok(Vec::new());
    }
    let bdfs: Vec<Option<u64>> = handles.iter().map(|&h| read_bdf(api, h)).collect();
    let names = gpu_names(api, &handles);
    let speeds = link_speeds(api, &handles);

    let mut snapshots = Vec::with_capacity(handles.len());
    for idx in 0..handles.len() {
        if let Some(snap) = read_processor_snapshot(api, &handles, &bdfs, names, speeds, idx) {
            snapshots.push(snap);
        }
    }
    Ok(snapshots)
}

/// GPU marketing names, read once and cached by GPU index:
/// `amdsmi_get_gpu_asic_info` leaks one fd per call on ROCm 6.4.x, so
/// calling it every refresh exhausts the fd table within minutes.
fn gpu_names(api: &Amdsmi, handles: &[ffi::ProcessorHandle]) -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        handles
            .iter()
            .map(|&h| read_gpu_name(api, h).unwrap_or_default())
            .collect()
    })
}

/// Per-GPU (bit_rate, bandwidth) Gb/s, read once and cached by GPU index:
/// link speed is static and `amdsmi_get_pcie_info` costs ~0.5 ms per call —
/// roughly half of the whole refresh when polled for every GPU each second.
fn link_speeds(
    api: &Amdsmi,
    handles: &[ffi::ProcessorHandle],
) -> &'static [(Option<f64>, Option<f64>)] {
    static SPEEDS: OnceLock<Vec<(Option<f64>, Option<f64>)>> = OnceLock::new();
    SPEEDS.get_or_init(|| handles.iter().map(|&h| read_link_speed(api, h)).collect())
}

/// Flatten socket/processor enumeration into one ordered GPU handle list.
/// This order defines `gpu_index` and matches amdsmi's accumulator indexing.
fn enumerate_gpus(api: &Amdsmi) -> Vec<ffi::ProcessorHandle> {
    let mut handles = Vec::new();
    let mut socket_count: u32 = 0;
    let rc = unsafe { (api.get_socket_handles)(&mut socket_count, ptr::null_mut()) };
    if rc != ffi::AMDSMI_STATUS_SUCCESS || socket_count == 0 {
        return handles;
    }
    let mut sockets: Vec<ffi::SocketHandle> = vec![ptr::null_mut(); socket_count as usize];
    let rc = unsafe { (api.get_socket_handles)(&mut socket_count, sockets.as_mut_ptr()) };
    if rc != ffi::AMDSMI_STATUS_SUCCESS {
        return handles;
    }
    for &socket in sockets.iter().take(socket_count as usize) {
        let mut proc_count: u32 = 0;
        let rc = unsafe { (api.get_processor_handles)(socket, &mut proc_count, ptr::null_mut()) };
        if rc != ffi::AMDSMI_STATUS_SUCCESS || proc_count == 0 {
            continue;
        }
        let mut procs: Vec<ffi::ProcessorHandle> = vec![ptr::null_mut(); proc_count as usize];
        let rc =
            unsafe { (api.get_processor_handles)(socket, &mut proc_count, procs.as_mut_ptr()) };
        if rc != ffi::AMDSMI_STATUS_SUCCESS {
            continue;
        }
        handles.extend(procs.iter().take(proc_count as usize).copied());
    }
    handles
}

fn read_bdf(api: &Amdsmi, handle: ffi::ProcessorHandle) -> Option<u64> {
    let mut bdf: u64 = 0;
    let rc = unsafe { (api.get_gpu_device_bdf)(handle, &mut bdf) };
    (rc == ffi::AMDSMI_STATUS_SUCCESS).then_some(bdf)
}

/// Build one GPU's snapshot. A "link" is an XGMI peer connection: topology
/// says which destination GPUs are XGMI-reachable, link_metrics supplies the
/// per-peer accumulators (indexed by destination GPU), pcie_info the speed.
fn read_processor_snapshot(
    api: &Amdsmi,
    handles: &[ffi::ProcessorHandle],
    bdfs: &[Option<u64>],
    names: &[String],
    speeds: &[(Option<f64>, Option<f64>)],
    idx: usize,
) -> Option<XgmiSnapshot> {
    let handle = handles[idx];
    let mut metrics: Padded<ffi::LinkMetrics> = Padded::zeroed();
    let rc = unsafe { (api.get_link_metrics)(handle, &mut metrics.value) };
    if rc != ffi::AMDSMI_STATUS_SUCCESS {
        return None;
    }
    let metrics = &metrics.value;

    let gpu_name = names.get(idx).cloned().unwrap_or_default();
    let (bit_rate_gbps, speed_gbps) = speeds.get(idx).copied().unwrap_or((None, None));
    let (correctable_errors, uncorrectable_errors) = read_wafl_errors(api, handle);

    let n = (metrics.num_links as usize).min(ffi::AMDSMI_MAX_NUM_XGMI_PHYSICAL_LINK);
    let mut links = Vec::new();
    let mut total_gbps = 0.0;
    for (peer_idx, &peer) in handles.iter().enumerate() {
        if peer_idx == idx || !is_xgmi_peer(api, handle, peer) {
            continue;
        }
        // Accumulators are indexed by destination GPU; peers beyond the
        // reported entry count carry no counters but still show the link.
        // The positional mapping is trusted only while the entry's own bdf
        // is unset (always zero on ROCm 6.4) or agrees with the peer's.
        let (tx_bytes, rx_bytes) = match metrics.links.get(peer_idx) {
            Some(entry)
                if peer_idx < n && (entry.bdf == 0 || Some(entry.bdf) == bdfs[peer_idx]) =>
            {
                (
                    Some(entry.write_kb.wrapping_mul(1024)),
                    Some(entry.read_kb.wrapping_mul(1024)),
                )
            }
            _ => (None, None),
        };
        total_gbps += speed_gbps.unwrap_or(0.0);
        links.push(XgmiLinkSnapshot {
            link_id: peer_idx as u32,
            // amdsmi_get_gpu_xgmi_link_status reports DOWN/DISABLE on
            // working MI300 links, so topo-confirmed peers show as active.
            is_active: true,
            speed_gbps,
            bit_rate_gbps,
            remote_pci_bdf: bdfs[peer_idx].map(format_bdf),
            tx_bytes,
            rx_bytes,
        });
    }

    // A GPU with no XGMI peers (e.g. consumer card) contributes no row.
    if links.is_empty() {
        return None;
    }

    Some(XgmiSnapshot {
        gpu_index: idx as u32,
        gpu_name,
        link_count: links.len() as u32,
        link_gbps: (total_gbps > 0.0).then_some(total_gbps),
        correctable_errors,
        uncorrectable_errors,
        links,
    })
}

/// True when topology reports an XGMI io-link between the two GPUs.
fn is_xgmi_peer(api: &Amdsmi, src: ffi::ProcessorHandle, dst: ffi::ProcessorHandle) -> bool {
    let mut hops: u64 = 0;
    let mut link_type: u32 = 0;
    let rc = unsafe { (api.topo_get_link_type)(src, dst, &mut hops, &mut link_type) };
    rc == ffi::AMDSMI_STATUS_SUCCESS && link_type == ffi::AMDSMI_IOLINK_TYPE_XGMI
}

/// Per-link (bit_rate, bandwidth) in Gb/s from PCIe static info, mirroring
/// amd-smi: bit_rate = max_pcie_speed/1000, bandwidth = bit_rate * width.
fn read_link_speed(api: &Amdsmi, handle: ffi::ProcessorHandle) -> (Option<f64>, Option<f64>) {
    let mut info: Padded<ffi::PcieInfo> = Padded::zeroed();
    let rc = unsafe { (api.get_pcie_info)(handle, &mut info.value) };
    if rc != ffi::AMDSMI_STATUS_SUCCESS || info.value.max_pcie_speed == 0 {
        return (None, None);
    }
    let info = &info.value;
    // Runtime value is MT/s (e.g. 32000); tolerate GT/s (e.g. 32) as-is.
    let bit_rate = if info.max_pcie_speed >= 1000 {
        info.max_pcie_speed as f64 / 1000.0
    } else {
        info.max_pcie_speed as f64
    };
    let bandwidth = (info.max_pcie_width > 0).then_some(bit_rate * info.max_pcie_width as f64);
    (Some(bit_rate), bandwidth)
}

fn read_gpu_name(api: &Amdsmi, handle: ffi::ProcessorHandle) -> Option<String> {
    let mut info: Padded<ffi::AsicInfo> = Padded::zeroed();
    let rc = unsafe { (api.get_gpu_asic_info)(handle, &mut info.value) };
    if rc != ffi::AMDSMI_STATUS_SUCCESS {
        return None;
    }
    let info = &mut info.value;
    // Ensure NUL-termination even if amdsmi fills the buffer completely.
    info.market_name[ffi::AMDSMI_MAX_STRING_LENGTH - 1] = 0;
    let name = unsafe { CStr::from_ptr(info.market_name.as_ptr() as *const std::os::raw::c_char) };
    let name = name.to_string_lossy().trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn read_wafl_errors(api: &Amdsmi, handle: ffi::ProcessorHandle) -> (Option<u64>, Option<u64>) {
    let mut ec: Padded<ffi::ErrorCount> = Padded::zeroed();
    let rc =
        unsafe { (api.get_gpu_ecc_count)(handle, ffi::AMDSMI_GPU_BLOCK_XGMI_WAFL, &mut ec.value) };
    if rc != ffi::AMDSMI_STATUS_SUCCESS {
        return (None, None);
    }
    (
        Some(ec.value.correctable_count),
        Some(ec.value.uncorrectable_count),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(id: u32, active: bool) -> XgmiLinkSnapshot {
        XgmiLinkSnapshot {
            link_id: id,
            is_active: active,
            speed_gbps: None,
            bit_rate_gbps: None,
            remote_pci_bdf: None,
            tx_bytes: None,
            rx_bytes: None,
        }
    }

    #[test]
    fn active_links_counts_active_only() {
        let snap = XgmiSnapshot {
            gpu_index: 0,
            gpu_name: "test".to_string(),
            link_count: 3,
            link_gbps: None,
            correctable_errors: None,
            uncorrectable_errors: None,
            links: vec![link(0, true), link(1, false), link(2, true)],
        };
        assert_eq!(snap.active_links(), 2);
    }

    #[test]
    fn active_links_no_links() {
        let snap = XgmiSnapshot {
            gpu_index: 0,
            gpu_name: "test".to_string(),
            link_count: 0,
            link_gbps: None,
            correctable_errors: None,
            uncorrectable_errors: None,
            links: Vec::new(),
        };
        assert_eq!(snap.active_links(), 0);
    }

    #[test]
    fn format_bdf_matches_lspci_form() {
        // 0000:05:00.0 -> domain 0, bus 5, device 0, function 0
        assert_eq!(format_bdf(0x0000_0000_0000_0500), "0000:05:00.0");
        // 0000:f5:00.0 (amd1 GPU7)
        assert_eq!(format_bdf(0x0000_0000_0000_f500), "0000:f5:00.0");
        // function/device bits: bus 0x65, device 0x1f, function 7
        // bdf = domain<<16 | bus<<8 | device<<3 | function
        assert_eq!(format_bdf((0x65 << 8) | (0x1f << 3) | 0x7), "0000:65:1f.7");
    }

    /// Hardware smoke test: run with `cargo test --features xgmi -- --ignored`
    /// on a machine with AMD GPUs.
    #[test]
    #[ignore]
    fn smoke_read_all_xgmi_stats() {
        let snaps = read_all_xgmi_stats().expect("io error");
        assert!(!snaps.is_empty(), "expected XGMI-capable GPUs on this host");
        assert!(
            snaps.iter().any(|s| s.link_count == 7),
            "expected at least one GPU with 7 XGMI links on amd1"
        );
        for s in &snaps {
            assert!(s.link_count > 0);
            assert!(
                s.active_links() > 0,
                "gpu{} has no active links",
                s.gpu_index
            );
            for l in &s.links {
                let bdf = l.remote_pci_bdf.as_deref().unwrap_or("<none>");
                assert!(
                    l.remote_pci_bdf.is_some() && bdf != "0000:00:00.0",
                    "gpu{} link{} has null BDF",
                    s.gpu_index,
                    l.link_id
                );
                assert!(
                    l.speed_gbps.unwrap_or(0.0) > 0.0,
                    "gpu{} link{} has zero speed",
                    s.gpu_index,
                    l.link_id
                );
                // Guards the positional accumulator mapping: every peer on
                // this host must carry counters (catches off-by-one in `n`
                // and bdf-mismatch degradation).
                assert!(
                    l.tx_bytes.is_some() && l.rx_bytes.is_some(),
                    "gpu{} link{} has no accumulator data",
                    s.gpu_index,
                    l.link_id
                );
            }
        }
        // Print compact dump of gpu0's snapshot for eyeballing.
        if let Some(s) = snaps.first() {
            let fl = s.links.first();
            eprintln!(
                "gpu0 name={:?} link_gbps={:?} first_link={:?}",
                s.gpu_name, s.link_gbps, fl
            );
        }

        // Regression check: repeated polling must not leak fds (the TUI
        // calls this every refresh; amdsmi_get_gpu_asic_info leaks on
        // ROCm 6.4.x when called per refresh, so names are cached).
        let count_fds = || std::fs::read_dir("/proc/self/fd").unwrap().count();
        let before = count_fds();
        for _ in 0..100 {
            read_all_xgmi_stats().expect("io error");
        }
        let after = count_fds();
        assert!(
            after <= before + 2,
            "fd leak: {} -> {} after 100 polls",
            before,
            after
        );
    }
}
