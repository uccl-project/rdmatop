import torch
import torch.distributed as dist

import common


def make(world, n):
    x = torch.randn(n, dtype=torch.float16, device="cuda")
    # ring all-reduce: each GPU transmits ~2*(world-1)/world of its buffer
    tx = int(x.numel() * x.element_size() * 2 * (world - 1) / world)
    return lambda: dist.all_reduce(x), tx


if __name__ == "__main__":
    common.run("all_reduce", make)
