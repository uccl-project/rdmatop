#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use crate::capture::{Capture, CaptureResult};
use crate::trace::PortMetrics;

pub type SampleFn = extern "C" fn(*mut c_void, u64, *const c_char, u32, *const c_char, f64);

const MIN_INTERVAL_US: u64 = 100;
const PANIC_MESSAGE: &str = "rdmatop panicked";
const METRIC_NAMES: [&CStr; 5] = [
    c"tx_gbps",
    c"rx_gbps",
    c"tx_pps",
    c"rx_pps",
    c"rx_drops_per_sec",
];

pub struct Handle {
    running: Option<Capture>,
    result: CaptureResult,
    error: Option<CString>,
}

impl Handle {
    fn stop(&mut self) -> &CaptureResult {
        if let Some(capture) = self.running.take() {
            self.result = capture.stop();
            self.error = self.result.error.clone().map(c_string);
        }
        &self.result
    }
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(message: String) {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(c_string(message)));
}

fn c_string(message: String) -> CString {
    CString::new(message.replace('\0', " ")).unwrap_or_default()
}

fn c_ptr(text: Option<&CString>) -> *const c_char {
    text.map_or(std::ptr::null(), |text| text.as_ptr())
}

fn devices_from_csv(csv: *const c_char) -> Option<Vec<String>> {
    if csv.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(csv) }.to_string_lossy();
    let names: Vec<String> = text
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(String::from)
        .collect();
    (!names.is_empty()).then_some(names)
}

pub fn handle_from(capture: Capture) -> *mut Handle {
    Box::into_raw(Box::new(Handle {
        running: Some(capture),
        result: CaptureResult::default(),
        error: None,
    }))
}

fn with_handle<T>(handle: *mut Handle, default: T, body: impl FnOnce(&mut Handle) -> T) -> T {
    if handle.is_null() {
        return default;
    }
    let handle = unsafe { &mut *handle };
    match catch_unwind(AssertUnwindSafe(|| body(handle))) {
        Ok(value) => value,
        Err(_) => {
            handle.error = Some(c_string(PANIC_MESSAGE.to_string()));
            default
        }
    }
}

fn metric_values(port: &PortMetrics) -> [f64; 5] {
    [
        port.tx_gbps,
        port.rx_gbps,
        port.tx_pps,
        port.rx_pps,
        port.rx_drops_per_sec,
    ]
}

fn emit_port(ts_ns: u64, port: &PortMetrics, cb: SampleFn, ctx: *mut c_void) {
    let dev = c_string(port.dev_name.clone());
    for (name, value) in METRIC_NAMES.iter().zip(metric_values(port)) {
        cb(ctx, ts_ns, dev.as_ptr(), port.port, name.as_ptr(), value);
    }
}

fn emit_samples(result: &CaptureResult, cb: SampleFn, ctx: *mut c_void) {
    for sample in &result.samples {
        for port in &sample.ports {
            emit_port(sample.ts_unix_ns, port, cb, ctx);
        }
    }
}

#[no_mangle]
pub extern "C" fn rdmatop_capture_start(
    interval_us: u64,
    devices_csv: *const c_char,
) -> *mut Handle {
    let devices = devices_from_csv(devices_csv);
    let interval = Duration::from_micros(interval_us.max(MIN_INTERVAL_US));
    let started = catch_unwind(AssertUnwindSafe(|| Capture::start(interval, devices)))
        .unwrap_or_else(|_| Err(io::Error::other(PANIC_MESSAGE)));
    match started {
        Ok(capture) => handle_from(capture),
        Err(error) => {
            set_last_error(error.to_string());
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn rdmatop_capture_stop(handle: *mut Handle) {
    with_handle(handle, (), |handle| {
        handle.stop();
    });
}

#[no_mangle]
pub extern "C" fn rdmatop_capture_for_each(
    handle: *mut Handle,
    cb: Option<SampleFn>,
    ctx: *mut c_void,
) {
    let Some(cb) = cb else {
        return;
    };
    with_handle(handle, (), |handle| emit_samples(handle.stop(), cb, ctx));
}

#[no_mangle]
pub extern "C" fn rdmatop_capture_error(handle: *mut Handle) -> *const c_char {
    with_handle(handle, std::ptr::null(), |handle| {
        c_ptr(handle.error.as_ref())
    })
}

#[no_mangle]
pub extern "C" fn rdmatop_capture_free(handle: *mut Handle) {
    if handle.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(handle) });
}

#[no_mangle]
pub extern "C" fn rdmatop_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| c_ptr(slot.borrow().as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::growing_reader;

    struct SeenSample {
        ts_ns: u64,
        dev: String,
        port: u32,
        metric: String,
        value: f64,
    }

    struct Seen(Vec<SeenSample>);

    extern "C" fn record_sample(
        ctx: *mut c_void,
        ts_ns: u64,
        dev: *const c_char,
        port: u32,
        metric: *const c_char,
        value: f64,
    ) {
        let seen = unsafe { &mut *(ctx as *mut Seen) };
        seen.0.push(SeenSample {
            ts_ns,
            dev: c_str_to_string(dev),
            port,
            metric: c_str_to_string(metric),
            value,
        });
    }

    fn c_str_to_string(text: *const c_char) -> String {
        unsafe { CStr::from_ptr(text) }
            .to_str()
            .unwrap()
            .to_string()
    }

    fn for_each_into(handle: *mut Handle, seen: &mut Seen) {
        rdmatop_capture_for_each(
            handle,
            Some(record_sample),
            seen as *mut Seen as *mut c_void,
        );
    }

    fn running_handle() -> *mut Handle {
        let capture =
            Capture::start_with_reader(growing_reader(&["efa0"]), Duration::from_millis(2), None)
                .unwrap();
        let handle = handle_from(capture);
        std::thread::sleep(Duration::from_millis(30));
        handle
    }

    #[test]
    fn for_each_yields_five_named_metrics_per_port() {
        let handle = running_handle();
        let mut seen = Seen(Vec::new());
        for_each_into(handle, &mut seen);
        assert!(rdmatop_capture_error(handle).is_null());
        assert!(!seen.0.is_empty());
        assert_eq!(seen.0.len() % 5, 0);
        let names: Vec<_> = seen.0[..5].iter().map(|s| s.metric.as_str()).collect();
        assert_eq!(
            names,
            ["tx_gbps", "rx_gbps", "tx_pps", "rx_pps", "rx_drops_per_sec"]
        );
        assert_eq!(seen.0[0].dev, "efa0");
        assert_eq!(seen.0[0].port, 1);
        assert!(seen.0[0].ts_ns > 0);
        assert!(seen.0[0].value > 0.0);
        rdmatop_capture_free(handle);
    }

    #[test]
    fn for_each_after_stop_replays_the_same_samples() {
        let handle = running_handle();
        let mut seen = Seen(Vec::new());
        for_each_into(handle, &mut seen);
        let count = seen.0.len();
        std::thread::sleep(Duration::from_millis(20));
        for_each_into(handle, &mut seen);
        assert_eq!(seen.0.len(), count * 2);
        rdmatop_capture_stop(handle);
        rdmatop_capture_free(handle);
    }

    #[test]
    fn failed_start_returns_null_and_sets_last_error() {
        let devices = CString::new("no-such-device").unwrap();
        let handle = rdmatop_capture_start(1000, devices.as_ptr());
        assert!(handle.is_null());
        let err = rdmatop_last_error();
        assert!(!err.is_null());
        assert!(!unsafe { CStr::from_ptr(err) }.to_bytes().is_empty());
    }

    #[test]
    fn devices_csv_is_split_and_trimmed() {
        let csv = CString::new(" mlx5_0, mlx5_1 ,,").unwrap();
        assert_eq!(
            devices_from_csv(csv.as_ptr()),
            Some(vec!["mlx5_0".to_string(), "mlx5_1".to_string()])
        );
        let empty = CString::new(" , ").unwrap();
        assert_eq!(devices_from_csv(empty.as_ptr()), None);
        assert_eq!(devices_from_csv(std::ptr::null()), None);
    }

    #[test]
    fn null_handle_is_ignored() {
        rdmatop_capture_stop(std::ptr::null_mut());
        assert!(rdmatop_capture_error(std::ptr::null_mut()).is_null());
        rdmatop_capture_free(std::ptr::null_mut());
    }
}
