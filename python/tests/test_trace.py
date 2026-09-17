import gzip
import json
import stat

import pytest
from rdmatop._trace import convert_legacy_trace


@pytest.mark.parametrize("suffix", [".json", ".json.gz"])
def test_legacy_export_restores_only_rdmatop_counters(tmp_path, suffix):
    counter = {
        "ph": "X",
        "dur": 0,
        "cat": "privateuse1_runtime",
        "name": "mlx5_0:1",
        "pid": 2147418112,
        "tid": 0,
        "ts": 123.456,
        "args": {"rdmatop_counter": 1, "tx_gbps": 12.5, "rx_gbps": 3.25},
    }
    unrelated_event = {**counter, "pid": 42}
    ordinary_event = {**counter, "args": {"value": 9}}
    trace = {
        "baseTimeNanoseconds": 1230000000000000000,
        "traceEvents": [
            {
                "ph": "M",
                "name": "process_name",
                "pid": 2147418112,
                "args": {"name": "rdmatop"},
            },
            counter,
            unrelated_event,
            ordinary_event,
        ],
    }
    path = tmp_path / f"trace{suffix}"
    open_trace = gzip.open if suffix.endswith(".gz") else open
    with open_trace(path, "wt") as output:
        json.dump(trace, output)
    path.chmod(0o640)

    convert_legacy_trace(path)

    with open_trace(path, "rt") as source:
        exported = json.load(source)
    assert stat.S_IMODE(path.stat().st_mode) == 0o640
    assert exported["baseTimeNanoseconds"] == 1230000000000000000
    assert exported["traceEvents"][1] == {
        "ph": "C",
        "cat": "rdmatop",
        "name": "mlx5_0:1",
        "pid": 2147418112,
        "tid": 0,
        "ts": 123.456,
        "args": {"tx_gbps": 12.5, "rx_gbps": 3.25},
    }
    assert exported["traceEvents"][0] == trace["traceEvents"][0]
    assert exported["traceEvents"][2:] == [unrelated_event, ordinary_event]


def test_export_without_rdmatop_samples_is_unchanged(tmp_path):
    path = tmp_path / "trace.json"
    original = '{ "traceEvents": [], "metadata": "preserve formatting" }\n'
    path.write_text(original)
    convert_legacy_trace(path)
    assert path.read_text() == original
