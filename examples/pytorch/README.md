# PyTorch Collectives

Small `torch.distributed` loops that drive sustained traffic over the
intranode GPU interconnect — **XGMI** on AMD (RCCL) or **NVLink** on
NVIDIA (NCCL). Run one in a shell and watch the `amdgpu<N>` /
`nvidia<N>` rows in `rdmatop` from another.

Collectives are symmetric: every GPU talks to every peer, so all
interconnect rows light up with roughly equal TX/RX, and the per-peer
detail pane shows traffic on every link.

| Script             | Collective              | TX per GPU per iteration    |
|--------------------|-------------------------|-----------------------------|
| `allgather.py`     | `all_gather`            | `(world-1) * buffer`        |
| `allreduce.py`     | `all_reduce`            | `~2*(world-1)/world * buffer` |
| `alltoall.py`      | `all_to_all_single`     | `(world-1)/world * buffer`  |
| `reducescatter.py` | `reduce_scatter_tensor` | `(world-1)/world * buffer`  |

## Prerequisites

- 2+ GPUs on one node with PyTorch matching your GPU stack:
  ```bash
  # AMD (ROCm)
  pip install torch --index-url https://download.pytorch.org/whl/rocm6.4
  # NVIDIA (CUDA)
  pip install torch
  ```
- `rdmatop` to see the GPU rows (XGMI/NVLink hardware is detected at
  runtime — no build flags needed):
  ```bash
  cargo install --path .
  ```

## Usage

```bash
cd examples/pytorch
torchrun --nproc_per_node=8 allgather.py                # 60s, 256 MiB/rank
torchrun --nproc_per_node=8 allreduce.py --mb 512 --secs 300
torchrun --nproc_per_node=4 alltoall.py                 # only 4 GPUs
torchrun --nproc_per_node=8 reducescatter.py
```

Rank 0 prints an estimated per-GPU transmit rate each batch
(`~N Gbps TX/GPU`, from the ring-algorithm model in the table above).
Compare it against the per-row TX in `rdmatop`.

## Notes

- The printed rate is a ring-algorithm estimate; RCCL/NCCL may pick
  other algorithms, so treat it as a sanity reference. Ground truth on
  AMD is `amd-smi xgmi` accumulator deltas, which read the same
  counters `rdmatop` samples.
- The `nccl` backend name is historical — on ROCm builds of PyTorch it
  is backed by RCCL, no code changes needed.

## Related Links

- [torch.distributed](https://pytorch.org/docs/stable/distributed.html)
  — collective API used by the scripts
- [rccl](https://github.com/ROCm/rccl) — AMD collective library
- [nccl](https://github.com/NVIDIA/nccl) — NVIDIA collective library
