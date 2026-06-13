# UCX Perftest

[UCX](https://openucx.org/) (Unified Communication X) provides a
high-performance messaging API that abstracts over verbs, TCP, shared
memory, CUDA-IPC, ROCm-IPC, and other transports. `ucx_perftest` is its
micro-benchmark — bandwidth and latency tests for tag matching, active
messages, RMA put/get, streams, atomics, and more. Like `perftest`, each
test is a two-process program: one rank is the server, the other dials in.

`examples/ucx/ucx.sh` is a thin wrapper that launches both ranks across
two hosts with `srun`, so you can benchmark inter-node UCX bandwidth or
latency in one command and watch the traffic in `rdmatop` from another
shell.

## Prerequisites

- A working Slurm cluster with both hosts as compute nodes, and
  `ucx_perftest` installed on both:
  ```bash
  sudo apt install -y ucx-utils
  ```
- `srun` reachable from where you launch the script (any login or
  compute node in the cluster).
- An RDMA device that is ACTIVE on both ends (`rdma link`, `ucx_info -d`)
  and a netdev with an IPv4 address reachable between the two hosts —
  UCX uses it for the wire-up handshake.

## Usage

```bash
./examples/ucx/ucx.sh <host1> <host2> [test_name]
#   host1 -> rank 0 -> server
#   host2 -> rank 1 -> client (dials host1)
#   test_name defaults to tag_bw; run `ucx_perftest -h` for the full list
#   host1/host2 must be Slurm NodeNames (see `sinfo -N -h -o %N`)
```

`ucx_perftest` is **iteration-based** (`-n N`), not duration-based like
`ib_write_bw`'s `-D 30`. The default `PERF_ITER=5000000` is sized for
roughly 30 s at 64 KB messages on a 100 GbE link.

## Examples

```bash
# UCP tag matching bandwidth (default test), ~30 s
./examples/ucx/ucx.sh host1 host2

# UCP tag matching latency
./examples/ucx/ucx.sh host1 host2 tag_lat

# UCP RMA put bandwidth, 1 MiB messages, 1 M iterations (~10 s)
PERF_SIZE=1048576 PERF_ITER=1000000 \
  ./examples/ucx/ucx.sh host1 host2 ucp_put_bw

# UCP RMA get bandwidth
./examples/ucx/ucx.sh host1 host2 ucp_get

# UCP active message bandwidth at 1 MiB messages
PERF_SIZE=1048576 ./examples/ucx/ucx.sh host1 host2 ucp_am_bw

# Sweep small-message tag latency (more iters because each one is cheap)
PERF_SIZE=8 PERF_ITER=50000000 \
  ./examples/ucx/ucx.sh host1 host2 tag_lat

# Pin to a different RDMA device + matching netdev
UCX_DEV=mlx5_0:1 UCX_NETDEV=enp49s0f0np0 \
  ./examples/ucx/ucx.sh host1 host2

# Data NIC whose netdev has no IPv4 (e.g. bnxt_re): set only UCX_DEV; add UCX_IB_GID_INDEX=3 if the RoCEv2 GID is wrong.
UCX_DEV=bnxt_re0:1 ./examples/ucx/ucx.sh host1 host2

# Pin to a Slurm partition / extra srun args
SRUN_EXTRA="-p all" ./examples/ucx/ucx.sh host1 host2

# Run inside the enroot container image (see Container Image below)
SRUN_EXTRA="--container-image=$HOME/changning/rdmatop/examples/ucx/ucx+latest.sqsh" \
  ./examples/ucx/ucx.sh host1 host2
```

## Container Image

`examples/ucx/Dockerfile` is a slim 87 MB Ubuntu 22.04 image with
`perftest` + `ucx-utils` and the verbs runtime libs.

```bash
# Build the image and import to ucx+latest.sqsh next to the Makefile.
# Run on each node (no shared FS):
cd examples/ucx && make

# Run inside the container:
SRUN_EXTRA="--container-image=$PWD/ucx+latest.sqsh" \
  ./ucx.sh host1 host2 tag_bw

# For HIP / rocm-smi inside, rebuild with the ROCm SDK base:
BASE_IMAGE=rocm/dev-ubuntu-22.04:6.2.4 make
```

## Environment Variables

| Variable                    | Default                              | Purpose                                                                                                                   |
|-----------------------------|--------------------------------------|---------------------------------------------------------------------------------------------------------------------------|
| `UCX_DEV`                   | `mlx5_1:1`                           | Exported as `UCX_NET_DEVICES`                                                                                             |
| `UCX_NETDEV`                | `enp49s0f1np1`                       | Netdev used to look up host1's IP for wire-up; need not match `UCX_DEV`                                                   |
| `PERF_SIZE`                 | `65536`                              | Message size, passed as `-s`                                                                                              |
| `PERF_ITER`                 | `5000000`                            | Iterations, passed as `-n` (~30 s at 64 KB / 100 GbE)                                                                     |
| `PERF_EXTRA`                | (empty)                              | Extra args appended verbatim to `ucx_perftest`                                                                            |
| `SRUN_EXTRA`                | (empty)                              | Extra args passed to `srun` (e.g. `--gres=gpu:1`, `--container-image=...`)                                                |
| `UCX_TLS`                   | `rc_verbs,ud_verbs,self,sm,tcp`      | Transports UCX is allowed to use.                                                                                         |
| `UCX_SOCKADDR_TLS_PRIORITY` | `tcp`                                | Wire-up handshake transport. TCP stays robust when multiple HCAs are present.                                             |
| `UCX_WARN_UNUSED_ENV_VARS`  | `n`                                  | Silences UCX's warning about unrecognised `UCX_*` vars when you set additional ones in the launching shell.               |

## Related Links

- [UCX](https://openucx.org/) — Unified Communication X framework
- [`ucx_perftest` source](https://github.com/openucx/ucx/tree/master/src/tools/perf)
- [Slurm](https://slurm.schedmd.com/) — `srun` launcher
- [enroot](https://github.com/NVIDIA/enroot) + [pyxis](https://github.com/NVIDIA/pyxis) — used for the `--container-image` path
