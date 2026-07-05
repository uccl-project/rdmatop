import torch
import torch.distributed as dist

import common


def make(world, n):
    # all_to_all_single sends one equal slice to each rank, so the buffer
    # is built as `world` slices of n//world elements.
    slice_len = n // world
    x = torch.randn(slice_len * world, dtype=torch.float16, device="cuda")
    out = torch.empty_like(x)
    # each GPU transmits (world-1) slices per iteration (own slice stays)
    tx = slice_len * x.element_size() * (world - 1)
    return lambda: dist.all_to_all_single(out, x), tx


if __name__ == "__main__":
    common.run("all_to_all", make)
