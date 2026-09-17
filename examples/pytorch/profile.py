import argparse
import os

import torch
import torch.distributed as dist
from torch.profiler import ProfilerActivity, profile

import rdmatop.kineto


def main():
    parser = argparse.ArgumentParser(
        description="all_reduce loop with rdmatop counters in the profiler trace"
    )
    parser.add_argument("--mb", type=int, default=256)
    parser.add_argument("--iters", type=int, default=50)
    parser.add_argument("--out", default="trace.json")
    args = parser.parse_args()

    if int(os.environ.get("LOCAL_RANK", 0)) == 0:
        rdmatop.kineto.enable()
    dist.init_process_group("nccl")
    rank = dist.get_rank()
    torch.cuda.set_device(rank % torch.cuda.device_count())
    payload = torch.ones(args.mb * (1 << 20) // 2, dtype=torch.float16, device="cuda")

    with profile(activities=[ProfilerActivity.CPU, ProfilerActivity.CUDA]) as prof:
        for _ in range(args.iters):
            dist.all_reduce(payload)
        torch.cuda.synchronize()

    if rank == 0:
        prof.export_chrome_trace(args.out)
        print(f"wrote {args.out}")
    dist.destroy_process_group()


if __name__ == "__main__":
    main()
