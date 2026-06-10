# IB Perftest

[perftest](https://github.com/linux-rdma/perftest) is the standard
suite of micro-benchmarks for RDMA verbs — `ib_write_bw`, `ib_read_bw`,
`ib_send_bw`, `ib_write_lat`, and friends. Each test is a two-process
program: one rank acts as a server, the other connects as a client.

`examples/ib/ib.sh` is a thin wrapper that launches both ranks across
two hosts with `srun`, so you can benchmark inter-node RDMA with a
single command and observe the traffic in `rdmatop` from another shell.

## Prerequisites

- A working Slurm cluster with both hosts as compute nodes, and
  `perftest` installed on both:
  ```bash
  sudo apt install -y perftest
  ```
- `srun` reachable from where you launch the script (any login or
  compute node in the cluster).
- An RDMA device that is ACTIVE on both ends (`rdma link`) and a netdev
  with an IPv4 address reachable between the two hosts.

## Usage

```bash
./examples/ib/ib.sh <host1> <host2> [perftest_binary]
#   host1 -> rank 0 -> server
#   host2 -> rank 1 -> client (dials host1)
#   host1/host2 must be Slurm NodeNames (see `sinfo -N -h -o %N`)
```

`perftest_binary` defaults to `ib_write_bw`. Any perftest tool that
follows the `<tool> [opts] [server_ip]` convention works.

## Examples

```bash
# RDMA write bandwidth, 30s, 1 QP, mlx5_1
./examples/ib/ib.sh host1 host2

# RDMA read bandwidth
./examples/ib/ib.sh host1 host2 ib_read_bw

# Send latency
./examples/ib/ib.sh host1 host2 ib_send_lat

# Longer run with 4 queue pairs (closer to line rate)
IB_DURATION=30 IB_QPS=4 ./examples/ib/ib.sh host1 host2

# Sweep all message sizes
IB_EXTRA="-a" ./examples/ib/ib.sh host1 host2

# Use a different RDMA device + matching netdev
IB_DEV=mlx5_0 IB_NETDEV=enp49s0f0np0 ./examples/ib/ib.sh host1 host2

# RoCE (e.g. Broadcom bnxt_re). See "Picking a GID on RoCE" below
# for what `-x 3` means and how to find the right index.
IB_DEV=bnxt_re0 IB_NETDEV=enp49s0f1np1 IB_EXTRA="-x 3" \
  ./examples/ib/ib.sh host1 host2

# Pin to a specific Slurm partition
SRUN_EXTRA="-p all" ./examples/ib/ib.sh host1 host2
```

## Picking a GID on RoCE

From `ib_write_bw --help`:

```
-x, --gid-index=<index>   Test uses GID with GID index taken from command line
```

So when `IB_EXTRA="-x 3"` is passed to perftest it expands to
`ib_write_bw -d bnxt_re0 -F -x 3 ...`. The `-x 3` tells the NIC
"use GID index 3 from `bnxt_re0`'s GID table."

Mapping the three env vars to what they do:

| In your command         | Becomes perftest flag   | Meaning                                                          |
|-------------------------|-------------------------|------------------------------------------------------------------|
| `IB_DEV=bnxt_re0`       | `-d bnxt_re0`           | which RDMA NIC to open                                           |
| `IB_NETDEV=enp49s0f1np1`| *(not a perftest flag)* | used only by `ib.sh` to look up host1's IPv4 for the TCP handshake |
| `IB_EXTRA="-x 3"`       | `-x 3`                  | which GID entry on that NIC to use for the RDMA connection       |

You **are** using GID — `-x 3` is literally selecting GID index 3.
The word "GID" doesn't appear in the command because perftest's
flag for it is just `-x`. Without `-x`, perftest defaults to index
0, which on many RoCE NICs is RoCEv1 link-local and won't route
between hosts.

To list the GID table (when `show_gids` isn't installed):

```bash
DEV=bnxt_re0
for i in /sys/class/infiniband/$DEV/ports/1/gids/*; do
  idx=$(basename "$i"); gid=$(cat "$i")
  [[ "$gid" == "0000:0000:0000:0000:0000:0000:0000:0000" ]] && continue
  type=$(cat /sys/class/infiniband/$DEV/ports/1/gid_attrs/types/$idx)
  ndev=$(cat /sys/class/infiniband/$DEV/ports/1/gid_attrs/ndevs/$idx)
  echo "idx=$idx type=$type ndev=$ndev gid=$gid"
done
```

Pick a `RoCE v2` entry on the right `ndev`, prefer a routable
(non `fe80::`) GID, and use the same index on both hosts.

## Environment Variables

| Variable      | Default          | Purpose                                                |
|---------------|------------------|--------------------------------------------------------|
| `IB_DEV`      | `mlx5_1`         | RDMA device passed as `-d`                             |
| `IB_NETDEV`   | `enp49s0f1np1`   | Netdev paired with `IB_DEV`; used to look up host1's IP |
| `IB_DURATION` | `30`             | Seconds, passed as `-D`                                |
| `IB_QPS`      | `1`              | Queue pairs, passed as `-q`                            |
| `IB_EXTRA`    | (empty)          | Extra args appended verbatim to the perftest binary    |
| `SRUN_EXTRA`  | (empty)          | Extra args passed to `srun` (e.g. `-p <partition>`)    |

## Related Links

- [perftest](https://github.com/linux-rdma/perftest)
  — upstream RDMA verbs benchmarks
- [Slurm](https://slurm.schedmd.com/)
  — `srun` launcher
- [rdma-core](https://github.com/linux-rdma/rdma-core)
  — userspace RDMA libraries and drivers
