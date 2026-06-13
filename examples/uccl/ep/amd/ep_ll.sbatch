#!/bin/bash
# Low-latency dispatch/combine bench (uccl-ep test_low_latency).
#   salloc -N 2 -p <partition> ./ep_ll.sbatch
# Extra args are appended to the bench command, e.g.:
#   salloc -N 2 -p <partition> ./ep_ll.sbatch --pressure-test-mode=1 --debug-hash
DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )"
source "${DIR}/ep_common.sh"

run_ep "bench/test_low_latency.py --num-tokens=128 --hidden=7168 \
  --num-topk=8 --num-experts=288 $*"
