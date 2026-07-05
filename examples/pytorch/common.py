"""Shared harness for the intranode collective benchmarks.

Each benchmark defines its collective and a per-iteration TX-bytes model;
run() handles setup, warmup, the timed loop, and rank-0 rate reporting.
"""

import argparse
import time

import torch
import torch.distributed as dist


def run(name, make):
    p = argparse.ArgumentParser(description=f"{name} loop for rdmatop monitoring")
    p.add_argument("--mb", type=int, default=256, help="per-rank buffer size in MiB")
    p.add_argument("--secs", type=float, default=60, help="run duration in seconds")
    args = p.parse_args()

    dist.init_process_group("nccl")  # NCCL on CUDA, RCCL on ROCm
    rank = dist.get_rank()
    world = dist.get_world_size()
    torch.cuda.set_device(rank % torch.cuda.device_count())

    n = args.mb * (1 << 20) // 2  # fp16 elements per rank
    step, tx_bytes_per_iter = make(world, n)

    for _ in range(3):  # warmup
        step()
    torch.cuda.synchronize()

    t0 = time.time()
    iters = 0
    while time.time() - t0 < args.secs:
        for _ in range(10):
            step()
            iters += 1
        torch.cuda.synchronize()
        if rank == 0:
            elapsed = time.time() - t0
            gbps = iters * tx_bytes_per_iter * 8 / 1e9 / elapsed
            print(
                f"{elapsed:6.1f}s  {iters:5d} iters  ~{gbps:7.1f} Gbps TX/GPU",
                flush=True,
            )

    dist.destroy_process_group()
