"""VizTracer plugin: ``viztracer --plugins rdmatop.viztracer -- script.py``."""

import argparse
import shlex
import warnings

from viztracer import get_tracer
from viztracer.vizplugin import VizPluginBase

from rdmatop._capture import Capture as _Capture


class RdmatopPlugin(VizPluginBase):
    def __init__(self, interval_ms=10, devices=None):
        super().__init__()
        if not 0.1 <= interval_ms <= 3_600_000:
            raise ValueError("interval_ms must be between 0.1 and 3600000 ms")
        if devices is not None and "\0" in devices:
            raise ValueError("devices must not contain NUL characters")
        self._interval_us = int(interval_ms * 1000)
        self._devices = devices
        self._tracer = None
        self._capture = None

    def support_version(self):
        return "1.1.1"

    def message(self, m_type, payload):
        if m_type == "command" and payload["cmd_type"] == "terminate":
            self._close_capture()
            return {"success": True}
        if m_type != "event":
            return {}
        when = payload["when"]
        if when == "initialize":
            self._initialize_tracer()
        elif when == "pre-start":
            self._start_capture()
        elif when == "post-stop":
            self._stop_capture()
        return {}

    def _initialize_tracer(self):
        self._tracer = get_tracer()
        if self._tracer is None:
            raise RuntimeError("rdmatop requires VizTracer(register_global=True)")

    def _start_capture(self):
        self._close_capture()
        try:
            self._capture = _Capture(self._interval_us, self._devices)
        except (OSError, RuntimeError) as error:
            warnings.warn(f"rdmatop: {error}", RuntimeWarning, stacklevel=2)

    def _stop_capture(self):
        if self._capture is None:
            return
        try:
            samples, error = self._capture.stop()
        finally:
            self._close_capture()
        self._add_counters(samples)
        if error:
            warnings.warn(f"rdmatop: {error}", RuntimeWarning, stacklevel=2)

    def _add_counters(self, samples):
        for event in _counter_events(samples, self._tracer.get_base_time()):
            self._tracer.add_raw(event)

    def _close_capture(self):
        capture, self._capture = self._capture, None
        if capture is None:
            return
        capture.close()


def _counter_events(samples, base_time_ns):
    events = {}
    for timestamp_ns, device, port, metric, value in samples:
        key = (timestamp_ns, device, port)
        event = events.setdefault(
            key,
            {
                "ph": "C",
                "cat": "rdmatop",
                "name": f"rdmatop/{device}:{port}",
                "ts": (timestamp_ns - base_time_ns) / 1000,
                "args": {},
            },
        )
        event["args"][metric] = value
    return events.values()


def get_vizplugin(plugin):
    parser = argparse.ArgumentParser(prog="rdmatop.viztracer")
    parser.add_argument(
        "--interval", type=float, default=10, help="sampling interval in milliseconds"
    )
    parser.add_argument("--devices", help="comma-separated RDMA device names")
    options = parser.parse_args(shlex.split(plugin)[1:])
    return RdmatopPlugin(interval_ms=options.interval, devices=options.devices)
