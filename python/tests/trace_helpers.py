import gzip
import json


def read_trace(path):
    open_trace = gzip.open if path.suffix == ".gz" else open
    with open_trace(path, "rt") as source:
        return json.load(source)


def write_trace(path, trace):
    open_trace = gzip.open if path.suffix == ".gz" else open
    with open_trace(path, "wt") as output:
        json.dump(trace, output)
