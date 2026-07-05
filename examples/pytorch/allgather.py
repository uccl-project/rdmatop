import torch
import torch.distributed as dist

import common


def make(world, n):
    x = torch.randn(n, dtype=torch.float16, device="cuda")
    out = [torch.empty_like(x) for _ in range(world)]
    # ring all-gather: each GPU transmits (world-1) shards per iteration
    tx = x.numel() * x.element_size() * (world - 1)
    return lambda: dist.all_gather(out, x), tx


if __name__ == "__main__":
    common.run("all_gather", make)
