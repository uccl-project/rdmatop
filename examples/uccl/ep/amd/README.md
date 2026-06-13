# UCCL-EP on AMD MI325X

[UCCL-EP](https://github.com/uccl-project/uccl/tree/main/ep) is a
DeepEP-compatible GPU-initiated expert-parallel communication library that
runs across heterogeneous GPUs and NICs (CX-7, EFA, Broadcom Thor2, AMD
Pollara). This example builds an enroot image with `uccl.ep` (uccl `main`
@ `34bdf4f`) on top of
`rocm/pytorch:rocm7.2.4_ubuntu24.04_py3.12_pytorch_release_2.10.0`
(gfx942 / MI325X) and runs the dispatch/combine benchmark across two nodes
via Slurm, on the Broadcom Thor2 (`bnxt_re`) RDMA fabric. Two entry points
share `ep_common.sh`:

| Script         | Bench                  | Workload                              |
|----------------|------------------------|---------------------------------------|
| `ep_ht.sbatch` | `test_internode.py`    | high throughput (`--num-tokens=4096`) |
| `ep_ll.sbatch` | `test_low_latency.py`  | low latency (`--num-tokens=128`)      |

If the host runs Broadcom's NetXtreme DKMS `bnxt_re` module (kernel ABI 6),
upstream rdma-core's provider in the image only speaks ABI 1 and verbs
calls fail. Per the
[ROCm multi-node setup guide](https://rocm.docs.amd.com/en/docs-7.2.1/how-to/rocm-for-ai/system-setup/multi-node-setup.html),
the container must run the host's matching vendor lib: `ep_common.sh` finds
`libbnxt_re-rdmav*.so` on a compute node at launch (override via
`BNXT_LIB=`) and bind-mounts it over the container's rdma-core provider.
The image itself stays vendor-free.

The benches run **strictly inside the enroot image** -- no host-side ROCm,
Python, or torch is required. At launch `ep_common.sh` discovers the ACTIVE
`bnxt_re` HCAs with `rdma link show` and exports them as a comma-separated
`UCCL_IB_HCA` / `NCCL_IB_HCA`; the libraries pick the right NIC per rank by
PCIe affinity, one rank per GPU (`torchrun --nproc_per_node=8`). Override
the matched name prefix with `HCA_PREFIX=`.

## Prerequisites

- A 2-node Slurm cluster of MI325X nodes.
- `docker` + `enroot` installed on each node. The image is built and
  imported on each node since `/var/lib/enroot` is node-local.
- `pyxis` (for `srun --container-image`).
- If the host uses Broadcom's NetXtreme DKMS `bnxt_re`, the matching
  userspace lib (`libbnxt_re-rdmav*.so`) installed on each compute node
  (auto-discovered at launch; override `BNXT_LIB=`). The lib ships with
  Broadcom's NetXtreme driver bundle (`bnxt_rocelib`).
- A GPU peer-mem kernel module loaded on the host for RDMA registration of
  GPU memory -- one of `ib_peer_mem` (Broadcom `bnxt_re` binds this) /
  `nvidia_peermem` / `efa_nv_peermem`
  (see [upstream notes](https://github.com/uccl-project/uccl/tree/main/ep#prerequisite)).

## Build

Run on **each** node (no shared FS assumed):

```bash
cd examples/uccl/ep/amd
make            # docker build -> enroot import -> uccl-ep+latest.sqsh
                # default: uccl main @ 34bdf4f
```

Override knobs:

```bash
# Different base image / uccl ref (tag, branch, or commit SHA)
BASE_IMAGE=rocm/pytorch:rocm7.2.4_ubuntu22.04_py3.10_pytorch_release_2.10.0 \
UCCL_REF=v0.1.1 make
```

## Run

```bash
# High-throughput bench
salloc -N 2 -p <partition> ./ep_ht.sbatch

# Low-latency bench
salloc -N 2 -p <partition> ./ep_ll.sbatch

# Extra flags are appended to the bench command
salloc -N 2 -p <partition> ./ep_ll.sbatch --pressure-test-mode=1 --debug-hash

# Match a different HCA name prefix (default bnxt_re)
HCA_PREFIX=bnxt_re0 salloc -N 2 -p <partition> ./ep_ht.sbatch
```

While the bench runs, on a separate shell run `rdmatop` on one of the
nodes. You should see traffic on `bnxt_re*`.

## Env Vars

| Variable                       | Default                            | Purpose                                                                                                  |
|--------------------------------|------------------------------------|----------------------------------------------------------------------------------------------------------|
| `HCA_PREFIX`                   | `bnxt_re`                          | Name prefix matched against `rdma link show`.                                                            |
| `NPROC_PER_NODE`               | `8`                                | Ranks per node (matches GPUs).                                                                            |
| `SOCKET_IFNAME`                | auto (iface that owns `MASTER_ADDR`, else `ip route get`) | Bootstrap netdev. Exported as `UCCL_SOCKET_IFNAME` / `NCCL_SOCKET_IFNAME`. Override if auto-detection picks the wrong iface. |
| `IB_GID_INDEX`                 | auto (first routable RoCEv2 GID in sysfs) | RDMA GID index. Exported as `UCCL_IB_GID_INDEX` / `NCCL_IB_GID_INDEX`. Inspect with `ibv_devinfo -d <hca> -v`. |
| `NCCL_DEBUG`                   | `WARN`                             | RCCL log level. Set `INFO` to debug NET/IB transport selection.                                          |
| `MASTER_PORT`                  | `29500`                            | torchrun rendezvous port.                                                                                 |
| `SQSH`                         | `./uccl-ep+latest.sqsh`            | Path to the enroot image.                                                                                 |
| `UCCL_IB_MAX_INFLIGHT_BYTES`   | `1572864` (auto, `bnxt_re` only)   | Broadcom Thor2 flow-control cap (per upstream).                                                           |
| `UCCL_IB_MAX_INFLIGHT_NORMAL`  | `1` (auto, `bnxt_re` only)         | Broadcom Thor2 flow-control cap (per upstream).                                                           |
| `BNXT_LIB`                     | auto (`find` over `/usr/local/lib`, `/usr/lib64`, `/usr/lib`) | Host path of Broadcom's vendor `bnxt_re` userspace driver, bind-mounted over the container's rdma-core provider. |

## Related Links

- [UCCL](https://github.com/uccl-project/uccl) -- the upstream repo
- [UCCL-EP README](https://github.com/uccl-project/uccl/tree/main/ep) -- bench scripts, env-var reference, perf tables
- [DeepEP](https://github.com/deepseek-ai/DeepEP) -- the API uccl-ep is compatible with
- [enroot](https://github.com/NVIDIA/enroot) + [pyxis](https://github.com/NVIDIA/pyxis) -- used for `--container-image`
- [Slurm](https://slurm.schedmd.com/) -- `srun` / `salloc` launcher
