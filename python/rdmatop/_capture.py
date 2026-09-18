import ctypes
import os
from pathlib import Path

_SampleCallback = ctypes.CFUNCTYPE(
    None,
    ctypes.c_void_p,
    ctypes.c_uint64,
    ctypes.c_char_p,
    ctypes.c_uint32,
    ctypes.c_char_p,
    ctypes.c_double,
)


def _load_library():
    path = Path(__file__).resolve().parent / "librdmatop.so"
    library = ctypes.CDLL(str(path))
    signatures = {
        "rdmatop_capture_start": ([ctypes.c_uint64, ctypes.c_char_p], ctypes.c_void_p),
        "rdmatop_capture_stop": ([ctypes.c_void_p], None),
        "rdmatop_capture_for_each": (
            [ctypes.c_void_p, _SampleCallback, ctypes.c_void_p],
            None,
        ),
        "rdmatop_capture_error": ([ctypes.c_void_p], ctypes.c_char_p),
        "rdmatop_capture_free": ([ctypes.c_void_p], None),
        "rdmatop_last_error": ([], ctypes.c_char_p),
    }
    for name, (arguments, result) in signatures.items():
        function = getattr(library, name)
        function.argtypes = arguments
        function.restype = result
    return library


class _SampleCollector:
    def __init__(self):
        self.samples = []
        self.error = None

    def collect(self, _context, timestamp_ns, device, port, metric, value):
        if self.error is not None:
            return
        try:
            sample = (
                timestamp_ns,
                device.decode(errors="replace"),
                port,
                metric.decode(errors="replace"),
                value,
            )
            self.samples.append(sample)
        except BaseException as error:  # noqa: BLE001
            # ctypes swallows exceptions; propagate them after returning from Rust.
            self.error = error


class Capture:
    def __init__(self, interval_us, devices):
        self._library = _load_library()
        self._pid = os.getpid()
        self._handle = self._library.rdmatop_capture_start(
            interval_us, devices.encode() if devices else None
        )
        if not self._handle:
            error = self._library.rdmatop_last_error()
            raise RuntimeError(
                error.decode(errors="replace") if error else "capture failed"
            )

    def stop(self):
        # A Rust sampling thread does not survive fork; never join its inherited handle.
        if not self._handle or self._pid != os.getpid():
            return [], None
        collector = _SampleCollector()
        callback = _SampleCallback(collector.collect)
        self._library.rdmatop_capture_for_each(self._handle, callback, None)
        if collector.error is not None:
            raise collector.error
        error = self._library.rdmatop_capture_error(self._handle)
        return collector.samples, error.decode(errors="replace") if error else None

    def close(self):
        handle, self._handle = self._handle, None
        if not handle or self._pid != os.getpid():
            return
        try:
            self._library.rdmatop_capture_stop(handle)
        finally:
            self._library.rdmatop_capture_free(handle)
