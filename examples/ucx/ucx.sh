#!/usr/bin/env bash
# ucx.sh - launch a ucx_perftest benchmark between two hosts via Slurm (srun).
#
# Usage:
#   ./ucx.sh <host1> <host2> [test_name]
#     host1 -> rank 0 -> ucx_perftest server
#     host2 -> rank 1 -> ucx_perftest client (connects to host1)
#     test_name defaults to tag_bw (see `ucx_perftest -h` for the full list)
#     host1/host2 must be Slurm NodeNames (e.g. `sinfo -N -h -o %N`)
#
# Env overrides:
#   UCX_DEV       UCX_NET_DEVICES value           (default: mlx5_1:1)
#   UCX_NETDEV    netdev paired with UCX_DEV      (default: enp49s0f1np1)
#                 used to look up host1's IP so host2 can dial it
#   PERF_SIZE     ucx_perftest -s message size    (default: 65536)
#   PERF_ITER     ucx_perftest -n iterations      (default: 5000000)
#                 ucx_perftest is iteration-based (no -D duration flag);
#                 5M iters @ 64KB ~= 30s on a 100GbE RoCE link.
#   PERF_EXTRA    extra args appended verbatim    (default: empty)
#   SRUN_EXTRA    extra args passed to srun       (default: empty)
#
#   UCX_TLS                    transports (default: rc_verbs,ud_verbs,self,sm,tcp)
#                              We skip mlx5 DV accelerated transports by default
#                              because they were crashing with REM_ACCESS_ERR on
#                              this RoCE setup. Override to "all" or
#                              "rc_mlx5,..." to re-enable.
#   UCX_SOCKADDR_TLS_PRIORITY  wireup transport (default: tcp). Forces the
#                              control handshake to go over TCP instead of
#                              rdma_cm, which is more robust when multiple
#                              HCAs are present.

set -euo pipefail

UCX_DEV=${UCX_DEV:-mlx5_1:1}
UCX_NETDEV=${UCX_NETDEV:-enp49s0f1np1}
PERF_SIZE=${PERF_SIZE:-65536}
PERF_ITER=${PERF_ITER:-5000000}
PERF_EXTRA=${PERF_EXTRA:-}
SRUN_EXTRA=${SRUN_EXTRA:-}

# UCX defaults — overridable by exporting before calling this script.
: ${UCX_TLS:=rc_verbs,ud_verbs,self,sm,tcp}
: ${UCX_SOCKADDR_TLS_PRIORITY:=tcp}
: ${UCX_WARN_UNUSED_ENV_VARS:=n}
export UCX_TLS UCX_SOCKADDR_TLS_PRIORITY UCX_WARN_UNUSED_ENV_VARS

if [[ $# -lt 2 ]]; then
  sed -n '2,21p' "$0"
  exit 2
fi

host1=$1
host2=$2
test_name=${3:-tag_bw}

# Resolve host1's IP on the RDMA-adjacent netdev so host2 can connect to it.
peer_ip=$(srun --nodelist="$host1" --nodes=1 --ntasks=1 \
  bash -c "ip -br -4 addr show $UCX_NETDEV 2>/dev/null | awk '{print \$3}' | cut -d/ -f1" \
  2>/dev/null | tr -d '[:space:]')
if [[ -z "$peer_ip" ]]; then
  echo "ERROR: could not resolve IPv4 on $UCX_NETDEV at $host1" >&2
  echo "       set UCX_NETDEV to a netdev that has an IP on both hosts." >&2
  exit 1
fi

echo "=== ucx.sh (slurm) ==="
echo "  test     : $test_name"
echo "  hosts    : $host1 (server) -> $host2 (client)"
echo "  device   : UCX_NET_DEVICES=$UCX_DEV  via netdev $UCX_NETDEV"
echo "  peer ip  : $peer_ip"
echo "  msg size : $PERF_SIZE   iter=$PERF_ITER"
[[ -n "$PERF_EXTRA" ]] && echo "  extra     : $PERF_EXTRA"
[[ -n "$SRUN_EXTRA" ]] && echo "  srun extra: $SRUN_EXTRA"
echo

# Pin rank 0 -> host1 (server), rank 1 -> host2 (client) via SLURM_HOSTFILE + arbitrary.
hostfile=$(mktemp)
trap 'rm -f "$hostfile"' EXIT
printf '%s\n%s\n' "$host1" "$host2" > "$hostfile"
export SLURM_HOSTFILE="$hostfile"

export PERF_TEST="$test_name"
export PEER_IP="$peer_ip"
export UCX_NET_DEVICES="$UCX_DEV"
export PERF_SIZE PERF_ITER PERF_EXTRA

exec srun \
  --ntasks=2 \
  --distribution=arbitrary \
  --label \
  $SRUN_EXTRA \
  bash -c '
    common=(-t "$PERF_TEST" -s "$PERF_SIZE" -n "$PERF_ITER")
    common+=($PERF_EXTRA)
    if [[ "$SLURM_PROCID" == "0" ]]; then
      echo "[$(hostname)] rank 0 server: ucx_perftest ${common[*]}"
      exec ucx_perftest "${common[@]}"
    else
      # Tiny delay so the server'\''s listener is up before we dial.
      sleep 1
      echo "[$(hostname)] rank 1 client: ucx_perftest ${common[*]} $PEER_IP"
      exec ucx_perftest "${common[@]}" "$PEER_IP"
    fi
  '
