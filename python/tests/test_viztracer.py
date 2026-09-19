import json
import os
import subprocess
import sys
import time
from functools import partial
from pathlib import Path

import pytest
from rdmatop import viztracer as rdma_viztracer
from viztracer import VizTracer


class RecordedCapture:
    def __init__(self, interval_us, devices):
        self.interval_us = interval_us
        self.devices = devices
        self.closed = False
        self.started_ns = time.time_ns()
        self.error = None

    def stop(self):
        return [
            (self.started_ns, "mlx5_0", 1, "tx_gbps", 12.5),
            (self.started_ns, "mlx5_0", 1, "rx_gbps", 3.0),
        ], self.error

    def close(self):
        self.closed = True


class CaptureFactory:
    def __init__(self):
        self.started = []

    def start(self, interval_us, devices):
        capture = RecordedCapture(interval_us, devices)
        self.started.append(capture)
        return capture


@pytest.fixture
def captures(monkeypatch):
    factory = CaptureFactory()
    monkeypatch.setattr(rdma_viztracer, "_Capture", factory.start)
    return factory.started


def unavailable_capture(*args):
    raise RuntimeError("no RDMA port matches the device filter")


def counter_events(path):
    return [
        event
        for event in json.loads(path.read_text())["traceEvents"]
        if event.get("cat") == "rdmatop"
    ]


def test_plugin_exports_aligned_counters_and_repeated_saves(tmp_path, captures):
    path = tmp_path / "trace.json"
    tracer = VizTracer(plugins=["rdmatop.viztracer"], verbose=0)
    before_us = tracer.getts()
    tracer.start()
    sum(range(100))
    tracer.stop()
    after_us = tracer.getts()
    tracer.save(str(path))
    events = counter_events(path)

    assert len(events) == 1
    assert events[0]["name"] == "rdmatop/mlx5_0:1"
    assert events[0]["ph"] == "C"
    assert events[0]["args"] == {"tx_gbps": 12.5, "rx_gbps": 3.0}
    assert before_us <= events[0]["ts"] <= after_us
    assert captures[0].closed
    tracer.save(str(path))
    assert counter_events(path) == events
    tracer.terminate()


def test_restarting_and_clearing_trace_does_not_replay_old_counters(tmp_path, captures):
    tracer = VizTracer(plugins=["rdmatop.viztracer"], verbose=0)
    path = tmp_path / "trace.json"
    for _ in range(2):
        tracer.start()
        tracer.stop()
    tracer.save(str(path))
    assert len(counter_events(path)) == 2
    tracer.clear()
    tracer.start()
    tracer.stop()
    tracer.save(str(path))
    assert len(counter_events(path)) == 1
    assert all(capture.closed for capture in captures)
    tracer.terminate()


def test_cli_options_reach_sampler(captures):
    tracer = VizTracer(
        plugins=["rdmatop.viztracer --interval 0.5 --devices mlx5_0,mlx5_1"],
        verbose=0,
    )
    tracer.start()
    tracer.stop()
    assert captures[0].interval_us == 500
    assert captures[0].devices == "mlx5_0,mlx5_1"
    tracer.terminate()


@pytest.mark.parametrize(
    "create_plugin",
    [
        rdma_viztracer.RdmatopPlugin,
        partial(rdma_viztracer.get_vizplugin, "rdmatop.viztracer"),
    ],
    ids=["python", "cli"],
)
def test_defaults_ignore_environment(create_plugin, captures, monkeypatch):
    monkeypatch.setenv("RDMATOP_INTERVAL_MS", "123")
    monkeypatch.setenv("RDMATOP_DEVICES", "mlx5_9")
    tracer = VizTracer(plugins=[create_plugin()], verbose=0)
    tracer.start()
    tracer.stop()
    tracer.terminate()
    assert captures[0].interval_us == 10_000
    assert captures[0].devices is None


def test_missing_rdma_warns_without_breaking_python_trace(tmp_path, monkeypatch):
    monkeypatch.setattr(rdma_viztracer, "_Capture", unavailable_capture)
    path = tmp_path / "trace.json"
    with (
        pytest.warns(RuntimeWarning, match="no RDMA port"),
        VizTracer(plugins=["rdmatop.viztracer"], verbose=0, output_file=str(path)),
    ):
        sum(range(100))
    trace = json.loads(path.read_text())
    assert any(event.get("ph") == "X" for event in trace["traceEvents"])
    assert not counter_events(path)


@pytest.mark.parametrize("interval", [0, -1, float("nan"), float("inf"), 0.01])
def test_invalid_sampling_interval_is_rejected(interval):
    with pytest.raises(ValueError, match="interval"):
        rdma_viztracer.RdmatopPlugin(interval_ms=interval)


def test_terminate_closes_active_capture(captures):
    tracer = VizTracer(plugins=["rdmatop.viztracer"], verbose=0)
    tracer.start()
    tracer.terminate()
    tracer.stop()
    assert captures[0].closed


def test_sampling_error_preserves_partial_counters(tmp_path, captures):
    path = tmp_path / "trace.json"
    tracer = VizTracer(plugins=["rdmatop.viztracer"], verbose=0)
    tracer.start()
    captures[0].error = "sample cap reached"
    with pytest.warns(RuntimeWarning, match="sample cap reached"):
        tracer.stop()
    tracer.save(str(path))
    tracer.terminate()
    assert len(counter_events(path)) == 1
    assert captures[0].closed


def test_import_does_not_load_torch():
    subprocess.run(
        [
            sys.executable,
            "-c",
            "import rdmatop.viztracer, sys; assert 'torch' not in sys.modules",
        ],
        check=True,
    )


@pytest.mark.skipif(sys.platform != "linux", reason="native sampler requires Linux")
def test_packaged_sampler_rejects_missing_device():
    from rdmatop._capture import Capture

    # A runner may lack the RDMA netlink protocol as well as RDMA devices.
    with pytest.raises(RuntimeError) as error:
        Capture(1000, "rdmatop-nonexistent-test-device")
    assert str(error.value)


@pytest.mark.skipif(
    not Path("/sys/class/infiniband").is_dir()
    or not list(Path("/sys/class/infiniband").glob("*/ports/*")),
    reason="no RDMA devices",
)
def test_native_sampling_exports_counter_tracks(tmp_path):
    path = tmp_path / "trace.json"
    before_ns = time.time_ns()
    plugin = rdma_viztracer.RdmatopPlugin(interval_ms=1)
    with VizTracer(plugins=[plugin], verbose=0, output_file=str(path)):
        time.sleep(0.05)
    after_ns = time.time_ns()
    trace = json.loads(path.read_text())
    events = counter_events(path)
    assert events
    base_ns = trace["viztracer_metadata"]["baseTimeNanoseconds"]
    for event in events:
        assert set(event["args"]) == {
            "tx_gbps",
            "rx_gbps",
            "tx_pps",
            "rx_pps",
            "rx_drops_per_sec",
        }
        assert before_ns <= base_ns + event["ts"] * 1000 <= after_ns
        assert event["pid"] == os.getpid()
