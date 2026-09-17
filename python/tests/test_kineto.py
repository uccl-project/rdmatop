import gzip
import json
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

METRICS = {"tx_gbps", "rx_gbps", "tx_pps", "rx_pps", "rx_drops_per_sec"}


def has_rdma():
    return os.path.isdir("/sys/class/infiniband") and bool(
        os.listdir("/sys/class/infiniband")
    )


def workload(device="cpu"):
    x = torch.randn(256, 256, device=device)
    for _ in range(10):
        x = x @ x.T
        time.sleep(0.005)


def export_trace(prof, path):
    prof.export_chrome_trace(str(path))
    open_trace = gzip.open if path.suffix == ".gz" else open
    with open_trace(path, "rt") as source:
        return json.load(source)


def rdmatop_counters(trace):
    events = trace["traceEvents"]
    process_names = [
        e for e in events if e.get("ph") == "M" and e.get("name") == "process_name"
    ]
    pids = {e["pid"] for e in process_names if e["args"]["name"] == "rdmatop"}
    return [e for e in events if e.get("ph") == "C" and e["pid"] in pids]


def event_ns(trace, event):
    return trace["baseTimeNanoseconds"] + event["ts"] * 1000


def counter_window_ns(trace):
    stamps = [event_ns(trace, event) for event in rdmatop_counters(trace)]
    return min(stamps), max(stamps)


@pytest.mark.skipif(not has_rdma(), reason="no RDMA devices")
@pytest.mark.parametrize("suffix", [".json", ".json.gz"])
@pytest.mark.parametrize("device", ["cpu", "cuda"])
def test_profile_contains_rdmatop_counter_tracks(
    tmp_path, monkeypatch, capfd, suffix, device
):
    if device == "cuda" and not torch.cuda.is_available():
        pytest.skip("CUDA is unavailable")
    rdmatop.kineto.enable()
    monkeypatch.setenv("RDMATOP_INTERVAL_MS", "5")
    before_ns = time.time_ns()
    activities = [ProfilerActivity.CPU]
    if device == "cuda":
        activities.append(ProfilerActivity.CUDA)
    with profile(activities=activities) as prof:
        workload(device)
    assert "not maintained address stability" not in capfd.readouterr().err
    after_ns = time.time_ns()
    trace = export_trace(prof, tmp_path / f"trace{suffix}")
    if device == "cuda":
        assert any(event.get("cat") == "kernel" for event in trace["traceEvents"])

    counters = rdmatop_counters(trace)
    assert len(counters) >= 5, f"expected counter events, got {len(counters)}"
    for event in counters:
        assert set(event["args"]) == METRICS
        assert ":" in event["name"]
        assert before_ns <= event_ns(trace, event) <= after_ns
    port_names = {event["name"] for event in counters}
    counter_pids = {event["pid"] for event in counters}
    resources = [
        event
        for event in trace["traceEvents"]
        if event.get("ph") == "M"
        and event.get("name") == "thread_name"
        and event.get("args", {}).get("name") in port_names
    ]
    assert {event["args"]["name"] for event in resources} == port_names
    assert {event["pid"] for event in resources} == counter_pids
    assert len({event["tid"] for event in resources}) == len(port_names)


@pytest.mark.skipif(not has_rdma(), reason="no RDMA devices")
def test_scheduled_cycles_each_get_their_own_samples(tmp_path, monkeypatch):
    rdmatop.kineto.enable()
    monkeypatch.setenv("RDMATOP_INTERVAL_MS", "5")
    traces = []

    def on_trace_ready(prof):
        traces.append(export_trace(prof, tmp_path / f"cycle{len(traces)}.json"))

    cycles = schedule(wait=0, warmup=1, active=1, repeat=2)
    with profile(
        activities=[ProfilerActivity.CPU],
        schedule=cycles,
        on_trace_ready=on_trace_ready,
    ) as prof:
        for _ in range(4):
            workload()
            prof.step()

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
def test_tensorboard_handler_exports_compressed_counters(tmp_path, monkeypatch):
    rdmatop.kineto.enable()
    monkeypatch.setenv("RDMATOP_INTERVAL_MS", "5")
    handler = tensorboard_trace_handler(str(tmp_path), use_gzip=True)
    with profile(activities=[ProfilerActivity.CPU], on_trace_ready=handler):
        workload()
    paths = list(tmp_path.glob("*.pt.trace.json.gz"))
    assert len(paths) == 1
    with gzip.open(paths[0], "rt") as source:
        assert rdmatop_counters(json.load(source))
