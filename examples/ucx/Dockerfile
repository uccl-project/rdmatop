ARG BASE_IMAGE=ubuntu:22.04
FROM ${BASE_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
       ca-certificates \
       perftest ucx-utils \
       libibverbs1 librdmacm1 ibverbs-utils ibverbs-providers \
       rdma-core \
       libnuma1 \
       iproute2 iputils-ping \
  && rm -rf /var/lib/apt/lists/*

CMD ["/bin/bash"]
