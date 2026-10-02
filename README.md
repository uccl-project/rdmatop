# rdmatop

[![Crates.io](https://img.shields.io/crates/v/rdmatop)](https://crates.io/crates/rdmatop)
[![PyPI](https://img.shields.io/pypi/v/rdmatop)](https://pypi.org/project/rdmatop/)
[![License](https://img.shields.io/crates/l/rdmatop)](LICENSE)

`htop`, but for RDMA traffic — a real-time TUI monitor for RDMA network interfaces.

<p align="center">
  <img src="https://raw.githubusercontent.com/uccl-project/rdmatop/main/images/rdmatop.gif" alt="rdmatop" width="800">
</p>

Monitors per-device throughput (Gbps, packets/s, drops), RDMA read/write counters,
retransmits, health events, and shows which processes are using each RDMA device —
all via RDMA netlink, the same interface used by [rdma statistic](https://github.com/iproute2/iproute2/blob/main/rdma/stat.c).

## Blogs

- [rdmatop: Cross-Provider htop for RDMA Traffic](https://uccl-project.github.io/posts/rdma-monitoring/) (2026-06-15)
- [NVSHMEM Multi-NIC Support with AWS EFA](https://www.pythonsheets.com/notes/appendix/nvshmem-multi-nic.html) (2026-03-27)

## Requirements

- **Linux** (netlink-based — macOS/Windows are not supported)
- RDMA-capable NICs (e.g., Mellanox/NVIDIA ConnectX, AWS EFA)

## Installation

### Python package

```bash
pip install rdmatop
```

### Ubuntu (PPA)

On Ubuntu 22.04 (jammy), 24.04 (noble), or 26.04 (resolute) — amd64 and arm64:

```bash
sudo add-apt-repository ppa:crazyguitar/rdmatop
sudo apt update
sudo apt install rdmatop
```

### Cargo

```bash
cargo install rdmatop
```

### From source

```bash
make         # cargo build
make install # cargo install
```

## Usage

```bash
rdmatop
```

## Perfetto recording

Press `r` to start and stop recording. rdmatop writes `rdmatop-<timestamp>.json`
to the current directory; open it in [ui.perfetto.dev](https://ui.perfetto.dev).

<p align="center">
  <img src="https://raw.githubusercontent.com/uccl-project/rdmatop/main/images/perfetto.png" alt="rdmatop Perfetto recording" width="800">
</p>

## PyTorch profiler

Build from a checkout (needs `cargo` and a C++ compiler; rebuild after
upgrading torch):

```bash
pip install "setuptools>=77" "torch>=2.9"
pip install --no-build-isolation -e .
```

Call `enable()` before profiling; RDMA counters land in the same trace:

```python
import rdmatop.kineto
from torch.profiler import ProfilerActivity, profile

rdmatop.kineto.enable()
with profile(activities=[ProfilerActivity.CPU, ProfilerActivity.CUDA]) as prof:
    train_step()
prof.export_chrome_trace("trace.json")
```

## VizTracer

Add RDMA counters to a VizTracer trace from the CLI:

```bash
pip install "viztracer>=1.1.1"
viztracer --plugins rdmatop.viztracer -- my_script.py
```

Or in code:

```python
from viztracer import VizTracer

with VizTracer(plugins=["rdmatop.viztracer"], output_file="trace.json"):
    train_step()
```

Set the sampling interval (ms, default 10) and limit to specific devices:

```bash
viztracer --plugins "rdmatop.viztracer --interval 1 --devices mlx5_0,mlx5_1" -- my_script.py
```

## Examples

Use `rdmatop` to monitor RDMA traffic while running GPU
communication benchmarks:

- [PyTorch](examples/pytorch/) — intranode NVLink/XGMI traffic
- [IB Perftest](examples/ib/) — two-node `ib_write_bw` benchmark
- [UCX Perftest](examples/ucx/) — two-node `ucx_perftest` bandwidth / latency
- [NCCL](examples/nccl/) — collective communication
- [NIXL](examples/nixl/) — point-to-point KV cache transfer
- [NVSHMEM](examples/nvshmem/) — one-sided GPU communication
- [PPLX Kernels](examples/pplx/) — MoE all-to-all dispatch/combine
- [UCCL](examples/uccl/) — DeepEP-compatible expert-parallel dispatch/combine
- [RDMA Statistics](examples/rdma/) — shell-based RDMA stats
- [Kubernetes](examples/kubernetes/) — DaemonSet deployment for Kubernetes

## How It Works

1. **Device enumeration** — `RDMA_NLDEV_CMD_GET` via netlink to discover all RDMA devices
2. **HW counters** — `RDMA_NLDEV_CMD_STAT_GET` per device/port, same as `rdma statistic show`
3. **Process detection** — `RDMA_NLDEV_CMD_RES_QP_GET` to map QPs → PIDs, enriched with `/proc` data
4. **Throughput** — Two snapshots per interval, delta / elapsed for rates

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build/test instructions,
design ground rules, and how to submit changes.

## License

Apache-2.0
