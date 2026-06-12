ARG BASE_IMAGE=rocm/pytorch:rocm7.2.4_ubuntu24.04_py3.12_pytorch_release_2.10.0
FROM ${BASE_IMAGE}
ARG UCCL_REF=34bdf4f49c8e33d618b983afc34fc5bec8686cfa
ARG TORCH_CUDA_ARCH_LIST=gfx942

ENV DEBIAN_FRONTEND=noninteractive

###################################################
## Sanity check: base must ship torch + ROCm/HIP.
RUN python -c "import torch; assert torch.version.hip, 'base image lacks ROCm torch'; \
print('torch', torch.__version__, 'hip', torch.version.hip)"

###################################################
## RDMA + build tools that rocm/pytorch doesn't ship.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential cmake git ninja-build \
        rdma-core libibverbs-dev libnuma-dev iproute2 \
        libnl-3-200 libnl-route-3-200 \
        libgflags-dev libgtest-dev libelf-dev \
        pkg-config zlib1g-dev libhiredis-dev \
        hipcub \
    && rm -rf /var/lib/apt/lists/*

###################################################
## uccl.ep's setup.py requires nanobind.
RUN python -m pip install --no-cache-dir nanobind

###################################################
## Build & install uccl.ep via `setup.py install` (upstream's documented
## path). `bdist_wheel` breaks on setuptools>=80:
## https://github.com/pypa/setuptools/issues/4759
# fetch + FETCH_HEAD so UCCL_REF can be a tag, branch, or commit SHA.
RUN git init -q /opt/uccl \
    && git -C /opt/uccl fetch --depth 1 \
        https://github.com/uccl-project/uccl.git ${UCCL_REF} \
    && git -C /opt/uccl checkout -q FETCH_HEAD \
    && cd /opt/uccl/ep \
    && TORCH_CUDA_ARCH_LIST=${TORCH_CUDA_ARCH_LIST} \
       python setup.py install \
    && python -c "import torch; import uccl.ep; print('uccl.ep OK')" \
    && mkdir -p /opt/uccl-ep \
    && cp -a /opt/uccl/ep/bench /opt/uccl-ep/bench \
    && rm -rf /opt/uccl

WORKDIR /opt/uccl-ep
CMD ["bash"]
