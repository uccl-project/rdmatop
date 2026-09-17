import os
import time

import pytest
import rdmatop.kineto
import torch
from torch.profiler import (
    ProfilerActivity,
    profile,
    schedule,
    tensorboard_trace_handler,
)
from trace_helpers import read_trace

METRICS = {"tx_gbps", "rx_gbps", "tx_pps", "rx_pps", "rx_drops_per_sec"}


def has_rdma():
    return os.path.isdir("/sys/class/infiniband") and bool(
        os.listdir("/sys/class/infiniband")
    )


def run_workload(device="cpu"):
    matrix = torch.randn(256, 256, device=device)
    for _ in range(10):
        matrix = matrix @ matrix.T
        time.sleep(0.005)


def export_trace(profiler, path):
    profiler.export_chrome_trace(str(path))
    return read_trace(path)


def rdmatop_counters(trace):
    events = trace["traceEvents"]
    process_names = [
        event
        for event in events
        if event.get("ph") == "M" and event.get("name") == "process_name"
    ]
    process_ids = {
        event["pid"] for event in process_names if event["args"]["name"] == "rdmatop"
    }
    return [
        event
        for event in events
        if event.get("ph") == "C" and event["pid"] in process_ids
    ]


def event_ns(trace, event):
    return trace["baseTimeNanoseconds"] + event["ts"] * 1000


def counter_window_ns(trace):
    timestamps_ns = [event_ns(trace, event) for event in rdmatop_counters(trace)]
    return min(timestamps_ns), max(timestamps_ns)


def assert_counter_tracks(trace, counters):
    port_names = {event["name"] for event in counters}
    counter_process_ids = {event["pid"] for event in counters}
    port_tracks = [
        event
        for event in trace["traceEvents"]
        if event.get("ph") == "M"
        and event.get("name") == "thread_name"
        and event.get("args", {}).get("name") in port_names
    ]
    assert {event["args"]["name"] for event in port_tracks} == port_names
    assert {event["pid"] for event in port_tracks} == counter_process_ids
    assert len({event["tid"] for event in port_tracks}) == len(port_names)


@pytest.fixture
def rdma_sampling(monkeypatch):
    rdmatop.kineto.enable()
    monkeypatch.setenv("RDMATOP_INTERVAL_MS", "5")


def assert_rdmatop_counters(trace, before_ns, after_ns):
    counters = rdmatop_counters(trace)
    assert len(counters) >= 5, f"expected counter events, got {len(counters)}"
    for event in counters:
        assert set(event["args"]) == METRICS
        assert ":" in event["name"]
        assert before_ns <= event_ns(trace, event) <= after_ns
    assert_counter_tracks(trace, counters)


@pytest.mark.skipif(not has_rdma(), reason="no RDMA devices")
@pytest.mark.parametrize("suffix", [".json", ".json.gz"])
def test_cpu_profile_contains_rdmatop_counter_tracks(
    tmp_path, capfd, rdma_sampling, suffix
):
    before_ns = time.time_ns()
    with profile(activities=[ProfilerActivity.CPU]) as profiler:
        run_workload("cpu")
    after_ns = time.time_ns()
    trace = export_trace(profiler, tmp_path / f"trace{suffix}")

    assert "not maintained address stability" not in capfd.readouterr().err
    assert_rdmatop_counters(trace, before_ns, after_ns)


@pytest.mark.skipif(not torch.cuda.is_available(), reason="CUDA is unavailable")
@pytest.mark.skipif(not has_rdma(), reason="no RDMA devices")
@pytest.mark.parametrize("suffix", [".json", ".json.gz"])
def test_cuda_profile_contains_kernels_and_rdmatop_counter_tracks(
    tmp_path, capfd, rdma_sampling, suffix
):
    before_ns = time.time_ns()
    with profile(activities=[ProfilerActivity.CPU, ProfilerActivity.CUDA]) as profiler:
        run_workload("cuda")
    after_ns = time.time_ns()
    trace = export_trace(profiler, tmp_path / f"trace{suffix}")

    assert "not maintained address stability" not in capfd.readouterr().err
    assert any(event.get("cat") == "kernel" for event in trace["traceEvents"])
    assert_rdmatop_counters(trace, before_ns, after_ns)


@pytest.mark.skipif(not has_rdma(), reason="no RDMA devices")
def test_scheduled_cycles_each_get_their_own_samples(tmp_path, rdma_sampling):
    traces = []

    def on_trace_ready(profiler):
        traces.append(export_trace(profiler, tmp_path / f"cycle{len(traces)}.json"))

    cycles = schedule(wait=0, warmup=1, active=1, repeat=2)
    with profile(
        activities=[ProfilerActivity.CPU],
        schedule=cycles,
        on_trace_ready=on_trace_ready,
    ) as profiler:
        for _ in range(4):
            run_workload()
            profiler.step()

    assert len(traces) == 2
    for trace in traces:
        assert rdmatop_counters(trace), "cycle without rdmatop counters"
    assert counter_window_ns(traces[0])[1] < counter_window_ns(traces[1])[0]


def test_enable_is_idempotent():
    rdmatop.kineto.enable()
    exporter = profile.export_chrome_trace
    rdmatop.kineto.enable()
    assert profile.export_chrome_trace is exporter


@pytest.mark.skipif(not has_rdma(), reason="no RDMA devices")
def test_tensorboard_handler_exports_compressed_counters(tmp_path, rdma_sampling):
    handler = tensorboard_trace_handler(str(tmp_path), use_gzip=True)
    with profile(activities=[ProfilerActivity.CPU], on_trace_ready=handler):
        run_workload()
    paths = list(tmp_path.glob("*.pt.trace.json.gz"))
    assert len(paths) == 1
    assert rdmatop_counters(read_trace(paths[0]))
