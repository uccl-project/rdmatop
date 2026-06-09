#!/usr/bin/env bash
# ib.sh - launch a perftest benchmark between two hosts via mpirun.
#
# Usage:
#   ./ib.sh <host1> <host2> [perftest_binary]
#     host1 -> rank 0 -> perftest server
#     host2 -> rank 1 -> perftest client (connects to host1)
#     perftest_binary defaults to ib_write_bw
#
# Env overrides:
#   IB_DEV       RDMA device                   (default: mlx5_1)
#   IB_NETDEV    netdev paired with IB_DEV     (default: enp49s0f1np1)
#                used to look up host1's IP so host2 can dial it
#   IB_DURATION  perftest -D seconds           (default: 30)
#   IB_QPS       perftest -q queue pairs       (default: 1)
#   IB_EXTRA     extra args appended verbatim  (default: empty)
#   MPI_EXTRA    extra args passed to mpirun   (default: empty)
#
# Examples:
#   ./ib.sh amd0 amd1
#   ./ib.sh amd0 amd1 ib_read_bw
#   IB_DURATION=30 IB_QPS=4 ./ib.sh amd0 amd1 ib_write_bw
#   IB_DEV=mlx5_0 IB_NETDEV=enp49s0f0np0 ./ib.sh amd0 amd1

set -euo pipefail

IB_DEV=${IB_DEV:-mlx5_1}
IB_NETDEV=${IB_NETDEV:-enp49s0f1np1}
IB_DURATION=${IB_DURATION:-30}
IB_QPS=${IB_QPS:-1}
IB_EXTRA=${IB_EXTRA:-}
MPI_EXTRA=${MPI_EXTRA:-}

# Worker mode: mpirun re-invokes this script on each host with a rank set.
rank=${OMPI_COMM_WORLD_RANK:-${PMI_RANK:-${PMIX_RANK:-}}}
if [[ -n "${rank}" ]]; then
  test_bin=$1
  peer_ip=$2
  common=(-d "$IB_DEV" -F -D "$IB_DURATION" -q "$IB_QPS" $IB_EXTRA)
  if [[ "$rank" == "0" ]]; then
    echo "[$(hostname)] rank 0: $test_bin ${common[*]}  (server)"
    exec "$test_bin" "${common[@]}"
  else
    # Tiny delay so the server's TCP listener is up before we dial.
    sleep 1
    echo "[$(hostname)] rank 1: $test_bin ${common[*]} $peer_ip  (client)"
    exec "$test_bin" "${common[@]}" "$peer_ip"
  fi
fi

# Launcher mode
if [[ $# -lt 2 ]]; then
  sed -n '2,25p' "$0"
  exit 2
fi

host1=$1
host2=$2
test_bin=${3:-ib_write_bw}

# Resolve host1's IP on the RDMA-adjacent netdev so host2 can connect to it.
peer_ip=$(ssh -o BatchMode=yes "$host1" \
  "ip -br -4 addr show $IB_NETDEV 2>/dev/null | awk '{print \$3}' | cut -d/ -f1")
if [[ -z "$peer_ip" ]]; then
  echo "ERROR: could not resolve IPv4 on $IB_NETDEV at $host1" >&2
  echo "       set IB_NETDEV to a netdev that has an IP on both hosts." >&2
  exit 1
fi

script=$(readlink -f "$0")
echo "=== ib.sh ==="
echo "  test     : $test_bin"
echo "  hosts    : $host1 (server) -> $host2 (client)"
echo "  device   : $IB_DEV  via netdev $IB_NETDEV"
echo "  peer ip  : $peer_ip"
echo "  duration : ${IB_DURATION}s   qps=${IB_QPS}"
[[ -n "$IB_EXTRA"  ]] && echo "  extra    : $IB_EXTRA"
[[ -n "$MPI_EXTRA" ]] && echo "  mpi extra: $MPI_EXTRA"
echo

exec mpirun --host "$host1,$host2" -np 2 \
  --tag-output \
  $MPI_EXTRA \
  "$script" "$test_bin" "$peer_ip"
