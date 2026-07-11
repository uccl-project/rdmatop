//! Shared per-GPU health metrics, filled by the NVML (nvlink) and amdsmi
//! (xgmi) readers and rendered as the GPU tabs' gauge strip.

/// Live values; each field is None when its read failed or is unsupported.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuMetrics {
    pub util_pct: Option<u32>,
    pub vram_used_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
    pub temp_c: Option<i64>,
    pub power_w: Option<f64>,
    pub clock_mhz: Option<u32>,
}
