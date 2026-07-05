import torch
import torch.distributed as dist

import common


def make(world, n):
    # reduce_scatter_tensor reduces the input into one shard per rank, so
    # the buffer is built as `world` shards of n//world elements.
    shard_len = n // world
    x = torch.randn(shard_len * world, dtype=torch.float16, device="cuda")
    out = torch.empty(shard_len, dtype=torch.float16, device="cuda")
    # ring reduce-scatter: each GPU transmits (world-1) shards per iteration
    tx = shard_len * x.element_size() * (world - 1)
    return lambda: dist.reduce_scatter_tensor(out, x), tx


if __name__ == "__main__":
    common.run("reduce_scatter", make)
