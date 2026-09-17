"""Counter export compatibility for Kineto versions without native counters."""

import gzip
import json
import os
import stat
import tempfile
from pathlib import Path


def restore_counter_events(trace):
    """Convert marked rdmatop events in place and report whether any changed."""
    events = trace["traceEvents"]
    process_ids = {
        event["pid"]
        for event in events
        if event.get("ph") == "M"
        and event.get("name") == "process_name"
        and event.get("args", {}).get("name") == "rdmatop"
    }
    changed = False
    for event in events:
        if (
            event.get("pid") in process_ids
            and event.get("ph") == "X"
            and event.get("args", {}).get("rdmatop_counter") == 1
        ):
            event["ph"] = "C"
            event["cat"] = "rdmatop"
            event.pop("dur", None)
            del event["args"]["rdmatop_counter"]
            changed = True
    return changed


def convert_legacy_trace(path):
    """Atomically restore counter events in a plain or gzip Chrome trace."""
    path = Path(path)
    open_trace = gzip.open if path.suffix == ".gz" else open
    with open_trace(path, "rt", encoding="utf-8") as source:
        trace = json.load(source)
    if not restore_counter_events(trace):
        return
    temporary_path = None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as temporary:
            temporary_path = temporary.name
        with open_trace(temporary_path, "wt", encoding="utf-8") as output:
            json.dump(trace, output, ensure_ascii=False)
        os.chmod(temporary_path, stat.S_IMODE(path.stat().st_mode))
        os.replace(temporary_path, path)
    finally:
        if temporary_path is not None and os.path.exists(temporary_path):
            os.unlink(temporary_path)
