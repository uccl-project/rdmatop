pub mod agent;
pub mod capture;

/// Package version plus the git revision embedded at build time (if any).
pub fn version() -> String {
    match option_env!("RDMATOP_GIT_REV") {
        Some(rev) => format!("{} ({rev})", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_string(),
    }
}
mod ffi;
pub(crate) mod gpu;
pub mod metrics;
pub(crate) mod net;
pub(crate) mod netlink;
pub mod nodes;
pub(crate) mod nvlink;
pub(crate) mod rdma;
pub mod remote;
pub(crate) mod sampler;
pub mod stat;
pub mod sysinfo;
pub mod trace;
pub mod tui;
pub mod wire;
pub(crate) mod xgmi;
