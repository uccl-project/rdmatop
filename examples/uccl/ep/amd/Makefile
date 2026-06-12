IMAGE_NAME ?= uccl-ep
IMAGE_TAG  ?= latest
BASE_IMAGE ?= rocm/pytorch:rocm7.2.4_ubuntu24.04_py3.12_pytorch_release_2.10.0
UCCL_REF   ?= 34bdf4f49c8e33d618b983afc34fc5bec8686cfa
OUTPUT_DIR ?= $(PWD)

.PHONY: all docker sqsh clean

all: sqsh

docker:
	docker build \
	  --build-arg BASE_IMAGE=$(BASE_IMAGE) \
	  --build-arg UCCL_REF=$(UCCL_REF) \
	  -t $(IMAGE_NAME):$(IMAGE_TAG) .

sqsh: docker
	rm -f $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).sqsh
	enroot import -o $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).sqsh dockerd://$(IMAGE_NAME):$(IMAGE_TAG)

clean:
	rm -f $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).sqsh
	docker rmi -f $(IMAGE_NAME):$(IMAGE_TAG) 2>/dev/null || true
