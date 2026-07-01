# rdmatop

[![Crates.io](https://img.shields.io/crates/v/rdmatop)](https://crates.io/crates/rdmatop)
[![License](https://img.shields.io/crates/l/rdmatop)](LICENSE)

`htop`, but for RDMA traffic — a real-time TUI monitor for RDMA network interfaces.

<p align="center">
  <img src="images/rdmatop.gif" alt="rdmatop" width="800">
</p>

Monitors per-device throughput (Gbps, packets/s, drops), RDMA read/write counters,
retransmits, health events, and shows which processes are using each RDMA device —
all via RDMA netlink, the same interface used by [rdma statistic](https://github.com/iproute2/iproute2/blob/main/rdma/stat.c).

## Requirements

- **Linux** (netlink-based — macOS/Windows are not supported)
- RDMA-capable NICs (e.g., Mellanox/NVIDIA ConnectX, AWS EFA)

## Installation

### Ubuntu (PPA)

On Ubuntu 22.04 (jammy), 24.04 (noble), or 26.04 (resolute):

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

Press `r` in the TUI to start recording and `r` again to stop. rdmatop captures
every device's tx/rx Gbps and packets/s per interval and writes a Chrome-JSON
trace (`rdmatop-<n>.json`) you can drag into [ui.perfetto.dev](https://ui.perfetto.dev)
— each device/port becomes its own set of counter tracks. Timestamps are relative
to when you pressed `r`, so the trace spans exactly your record window.

<p align="center">
  <img src="images/perfetto.png" alt="rdmatop Perfetto recording" width="800">
</p>

## Examples

Use `rdmatop` to monitor RDMA traffic while running GPU
communication benchmarks:

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

## License

Apache-2.0
