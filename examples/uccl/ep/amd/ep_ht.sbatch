#!/bin/bash
# High-throughput dispatch/combine bench (uccl-ep test_internode).
#   salloc -N 2 -p <partition> ./ep_ht.sbatch
# Extra args are appended to the bench command, e.g.:
#   salloc -N 2 -p <partition> ./ep_ht.sbatch --pressure-test-mode=1
DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )"
source "${DIR}/ep_common.sh"

run_ep "bench/test_internode.py --num-tokens=4096 --hidden=7168 \
  --num-topk=8 --num-experts=288 --test-ll-compatibility $*"
