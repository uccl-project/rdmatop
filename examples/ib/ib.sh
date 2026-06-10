#!/usr/bin/env bash
# ib.sh - launch a perftest benchmark between two hosts via Slurm (srun).
#
# Usage:
#   ./ib.sh <host1> <host2> [perftest_binary]
#     host1 -> rank 0 -> perftest server
#     host2 -> rank 1 -> perftest client (connects to host1)
#     perftest_binary defaults to ib_write_bw
#     host1/host2 must be Slurm NodeNames (e.g. `sinfo -N -h -o %N`)
#
# Env overrides:
#   IB_DEV       RDMA device                   (default: mlx5_1)
#   IB_NETDEV    netdev paired with IB_DEV     (default: enp49s0f1np1)
#                used to look up host1's IP so host2 can dial it
#   IB_DURATION  perftest -D seconds           (default: 30)
#   IB_QPS       perftest -q queue pairs       (default: 1)
#   IB_EXTRA     extra args appended verbatim  (default: empty)
#   SRUN_EXTRA   extra args passed to srun     (default: empty)

set -euo pipefail

IB_DEV=${IB_DEV:-mlx5_1}
IB_NETDEV=${IB_NETDEV:-enp49s0f1np1}
IB_DURATION=${IB_DURATION:-30}
IB_QPS=${IB_QPS:-1}
IB_EXTRA=${IB_EXTRA:-}
SRUN_EXTRA=${SRUN_EXTRA:-}

if [[ $# -lt 2 ]]; then
  sed -n '2,19p' "$0"
  exit 2
fi

host1=$1
host2=$2
test_bin=${3:-ib_write_bw}

# Resolve host1's IP on the RDMA-adjacent netdev so host2 can connect to it.
peer_ip=$(srun --nodelist="$host1" --nodes=1 --ntasks=1 \
  bash -c "ip -br -4 addr show $IB_NETDEV 2>/dev/null | awk '{print \$3}' | cut -d/ -f1" \
  2>/dev/null | tr -d '[:space:]')
if [[ -z "$peer_ip" ]]; then
  echo "ERROR: could not resolve IPv4 on $IB_NETDEV at $host1" >&2
  echo "       set IB_NETDEV to a netdev that has an IP on both hosts." >&2
  exit 1
fi

echo "=== ib.sh (slurm) ==="
echo "  test     : $test_bin"
echo "  hosts    : $host1 (server) -> $host2 (client)"
echo "  device   : $IB_DEV  via netdev $IB_NETDEV"
echo "  peer ip  : $peer_ip"
echo "  duration : ${IB_DURATION}s   qps=${IB_QPS}"
[[ -n "$IB_EXTRA"   ]] && echo "  extra     : $IB_EXTRA"
[[ -n "$SRUN_EXTRA" ]] && echo "  srun extra: $SRUN_EXTRA"
echo

# Pin rank 0 -> host1, rank 1 -> host2 via SLURM_HOSTFILE + arbitrary distribution.
hostfile=$(mktemp)
trap 'rm -f "$hostfile"' EXIT
printf '%s\n%s\n' "$host1" "$host2" > "$hostfile"
export SLURM_HOSTFILE="$hostfile"

export PERF_BIN="$test_bin"
export PEER_IP="$peer_ip"
export IB_DEV IB_DURATION IB_QPS IB_EXTRA

exec srun \
  --ntasks=2 \
  --distribution=arbitrary \
  --label \
  $SRUN_EXTRA \
  bash -c '
    common=(-d "$IB_DEV" -F -D "$IB_DURATION" -q "$IB_QPS" $IB_EXTRA)
    if [[ "$SLURM_PROCID" == "0" ]]; then
      echo "[$(hostname)] rank 0 server: $PERF_BIN ${common[*]}"
      exec "$PERF_BIN" "${common[@]}"
    else
      # Tiny delay so the server'\''s listener is up before we dial.
      sleep 1
      echo "[$(hostname)] rank 1 client: $PERF_BIN ${common[*]} $PEER_IP"
      exec "$PERF_BIN" "${common[@]}" "$PEER_IP"
    fi
  '
