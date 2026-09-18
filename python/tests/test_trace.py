import stat

import pytest
from rdmatop._trace import convert_legacy_trace
from trace_helpers import read_trace, write_trace

RDMATOP_PROCESS_ID = 2147418112
BASE_TIME_NS = 1230000000000000000


@pytest.fixture
def legacy_counter():
    return {
        "ph": "X",
        "dur": 0,
        "cat": "privateuse1_runtime",
        "name": "mlx5_0:1",
        "pid": RDMATOP_PROCESS_ID,
        "tid": 0,
        "ts": 123.456,
        "args": {"rdmatop_counter": 1, "tx_gbps": 12.5, "rx_gbps": 3.25},
    }


@pytest.fixture
def legacy_trace(legacy_counter):
    process_name = {
        "ph": "M",
        "name": "process_name",
        "pid": RDMATOP_PROCESS_ID,
        "args": {"name": "rdmatop"},
    }
    return {
        "baseTimeNanoseconds": BASE_TIME_NS,
        "traceEvents": [process_name, legacy_counter],
    }


@pytest.fixture(params=[".json", ".json.gz"])
def trace_path(tmp_path, request):
    return tmp_path / f"trace{request.param}"


def test_legacy_export_restores_counter_fields(trace_path, legacy_trace):
    write_trace(trace_path, legacy_trace)

    convert_legacy_trace(trace_path)

    exported_counter = read_trace(trace_path)["traceEvents"][1]
    assert exported_counter == {
        "ph": "C",
        "cat": "rdmatop",
        "name": "mlx5_0:1",
        "pid": RDMATOP_PROCESS_ID,
        "tid": 0,
        "ts": 123.456,
        "args": {"tx_gbps": 12.5, "rx_gbps": 3.25},
    }


def test_legacy_export_preserves_other_events(trace_path, legacy_trace, legacy_counter):
    other_process_counter = {**legacy_counter, "pid": 42}
    unmarked_event = {**legacy_counter, "args": {"value": 9}}
    legacy_trace["traceEvents"].extend([other_process_counter, unmarked_event])
    write_trace(trace_path, legacy_trace)

    convert_legacy_trace(trace_path)

    exported_events = read_trace(trace_path)["traceEvents"]
    assert len(exported_events) == 4
    assert exported_events[0] == legacy_trace["traceEvents"][0]
    assert exported_events[2:] == [other_process_counter, unmarked_event]


def test_legacy_export_preserves_base_time(trace_path, legacy_trace):
    write_trace(trace_path, legacy_trace)

    convert_legacy_trace(trace_path)

    assert read_trace(trace_path)["baseTimeNanoseconds"] == BASE_TIME_NS


def test_legacy_export_preserves_permissions(trace_path, legacy_trace):
    write_trace(trace_path, legacy_trace)
    trace_path.chmod(0o640)

    convert_legacy_trace(trace_path)

    assert stat.S_IMODE(trace_path.stat().st_mode) == 0o640


def test_export_without_rdmatop_samples_is_unchanged(tmp_path):
    path = tmp_path / "trace.json"
    original = '{ "traceEvents": [], "metadata": "preserve formatting" }\n'
    path.write_text(original)

    convert_legacy_trace(path)

    assert path.read_text() == original
