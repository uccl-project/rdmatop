#!/bin/bash
# Shared launch logic for the uccl-ep benches, sourced by ep_ht.sbatch and
# ep_ll.sbatch. run_ep "<bench cmd>" discovers the Broadcom RDMA fabric and
# runs the bench across the Slurm allocation inside the enroot image.
set -exo pipefail

run_ep() {
  local INNER_CMD="$1"
  local DIR; DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )"
  local SQSH="${SQSH:-${DIR}/uccl-ep+latest.sqsh}"

  # HCA name prefix matched against `rdma link show` (Broadcom Thor2 = bnxt_re).
  local HCA_PREFIX="${HCA_PREFIX:-bnxt_re}"
  local NPROC_PER_NODE="${NPROC_PER_NODE:-8}"
  local NCCL_DEBUG="${NCCL_DEBUG:-WARN}"
  local SOCKET_IFNAME="${SOCKET_IFNAME:-}"   # empty = auto-detect from route to MASTER_ADDR
  local IB_GID_INDEX="${IB_GID_INDEX:-}"     # empty = auto-detect routable RoCEv2 GID from sysfs
  local MASTER_PORT="${MASTER_PORT:-29500}"

  if [[ -z "${SLURM_JOB_NODELIST:-}" ]]; then
    echo "ep: \$SLURM_JOB_NODELIST is unset; launch via salloc/sbatch" >&2
    exit 1
  fi

  local HEAD_NODE; HEAD_NODE="$(scontrol show hostnames "$SLURM_JOB_NODELIST" | head -n1)"
  # Use Slurm's NodeAddr (routable IP it uses internally) so we sidestep
  # /etc/hosts entries that map the hostname to 127.0.1.1.
  local MASTER_ADDR
  MASTER_ADDR="$(scontrol show node "$HEAD_NODE" -o | grep -oE 'NodeAddr=[^ ]+' | cut -d= -f2)"
  if [[ -z "$MASTER_ADDR" ]]; then
    echo "ep: could not resolve NodeAddr for $HEAD_NODE via scontrol" >&2
    exit 1
  fi

  # Broadcom's DKMS bnxt_re speaks kernel ABI 6 but upstream rdma-core only
  # ABI 1, so bind-mount the host's vendor lib over the container's provider.
  # See: https://rocm.docs.amd.com/en/docs-7.2.1/how-to/rocm-for-ai/system-setup/multi-node-setup.html
  local BNXT_LIB="${BNXT_LIB:-$(srun -N1 -n1 bash -c \
    "find /usr/local/lib /usr/lib64 /usr/lib -name 'libbnxt_re-rdmav*.so' -print -quit 2>/dev/null || true")}"
  if [[ -z "$BNXT_LIB" ]]; then
    echo "ep: no libbnxt_re-rdmav*.so found on the host; bnxt_re verbs would fail (set BNXT_LIB=)" >&2
    exit 1
  fi
  local MOUNT_ARGS=(--container-mounts "${BNXT_LIB}:/usr/lib/x86_64-linux-gnu/libibverbs/$(basename "$BNXT_LIB")")

  local cmd
  cmd="$(cat <<EOF
# Discover ACTIVE RDMA devices whose name starts with HCA_PREFIX.
HCAS=\$(rdma link show 2>/dev/null \\
  | awk -v p="${HCA_PREFIX}" '\$0 ~ ("link " p) && /state ACTIVE/ {print \$2}' \\
  | cut -d/ -f1 | sort -u | paste -sd,)

if [[ -z "\$HCAS" ]]; then
  echo "ep: no ACTIVE RDMA device matched prefix '${HCA_PREFIX}' on \$(hostname)" >&2
  rdma link show >&2 || true
  exit 1
fi
echo "ep: \$(hostname) selected HCAs: \$HCAS"

# Pick the first routable RoCEv2 GID index from the first selected HCA's sysfs,
# unless IB_GID_INDEX= overrides it. Skip fe80 link-local and all-zero slots
# (the routable RoCEv2 GID may be IPv4-mapped or IPv6/ULA).
IB_GID_INDEX="${IB_GID_INDEX}"
if [[ -z "\$IB_GID_INDEX" ]]; then
  GID_DIR="/sys/class/infiniband/\${HCAS%%,*}/ports/1"
  for idx in \$(ls "\$GID_DIR/gid_attrs/types" | sort -n); do
    [[ "\$(cat "\$GID_DIR/gid_attrs/types/\$idx" 2>/dev/null)" == "RoCE v2" ]] || continue
    gid=\$(cat "\$GID_DIR/gids/\$idx")
    [[ "\$gid" == fe80:* || "\$gid" == 0000:0000:0000:0000:0000:0000:0000:0000 ]] && continue
    IB_GID_INDEX=\$idx
    break
  done
  if [[ -z "\$IB_GID_INDEX" ]]; then
    echo "ep: no routable RoCEv2 GID under \$GID_DIR; set IB_GID_INDEX=<idx>" >&2
    exit 1
  fi
fi
echo "ep: \$(hostname) IB_GID_INDEX=\$IB_GID_INDEX"

# Auto-detect bootstrap netdev. Prefer the iface that actually owns
# MASTER_ADDR (correct on the master node). On peer nodes that don't host
# MASTER_ADDR, fall back to the iface used to route to it.
SOCKET_IFNAME="${SOCKET_IFNAME}"
if [[ -z "\$SOCKET_IFNAME" ]]; then
  SOCKET_IFNAME=\$(ip -o -4 addr show 2>/dev/null \\
    | awk -v ip="${MASTER_ADDR}" '\$2 != "lo" && \$4 ~ ("^" ip "/") {print \$2; exit}')
fi
if [[ -z "\$SOCKET_IFNAME" ]]; then
  SOCKET_IFNAME=\$(ip -o route get ${MASTER_ADDR} 2>/dev/null \\
    | sed -n 's/.* dev \\([^ ]*\\).*/\\1/p' | head -n1)
fi
if [[ -z "\$SOCKET_IFNAME" || "\$SOCKET_IFNAME" == "lo" ]]; then
  echo "ep: could not auto-detect non-loopback SOCKET_IFNAME on \$(hostname); set SOCKET_IFNAME=<iface>" >&2
  exit 1
fi
echo "ep: \$(hostname) SOCKET_IFNAME=\$SOCKET_IFNAME"

export UCCL_IB_HCA=\$HCAS
export NCCL_IB_HCA=\$HCAS
export UCCL_SOCKET_IFNAME=\$SOCKET_IFNAME
export NCCL_SOCKET_IFNAME=\$SOCKET_IFNAME
export UCCL_IB_GID_INDEX=\$IB_GID_INDEX
export NCCL_IB_GID_INDEX=\$IB_GID_INDEX
export NCCL_DEBUG=${NCCL_DEBUG}

# Broadcom Thor2 needs strict flow control to avoid CQE error 12.
if [[ "${HCA_PREFIX}" == bnxt_re ]]; then
  export UCCL_IB_MAX_INFLIGHT_BYTES=1572864
  export UCCL_IB_MAX_INFLIGHT_NORMAL=1
fi

cd /opt/uccl-ep

torchrun \\
  --nnodes=\${SLURM_NNODES} \\
  --nproc_per_node=${NPROC_PER_NODE} \\
  --node_rank=\${SLURM_NODEID} \\
  --master_addr=${MASTER_ADDR} \\
  --master_port=${MASTER_PORT} \\
  ${INNER_CMD}
EOF
)"

  srun --container-image "${SQSH}" \
       --container-name uccl-ep \
       "${MOUNT_ARGS[@]}" \
       --ntasks-per-node=1 \
       bash -c "${cmd}"
}
