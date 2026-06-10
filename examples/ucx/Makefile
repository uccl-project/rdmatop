IMAGE_NAME ?= ucx
IMAGE_TAG  ?= latest
BASE_IMAGE ?= ubuntu:22.04
OUTPUT_DIR ?= $(PWD)

.PHONY: all docker sqsh clean

all: sqsh

docker:
	docker build --build-arg BASE_IMAGE=$(BASE_IMAGE) -t $(IMAGE_NAME):$(IMAGE_TAG) .

sqsh: docker
	rm -f $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).sqsh
	enroot import -o $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).sqsh dockerd://$(IMAGE_NAME):$(IMAGE_TAG)

clean:
	rm -f $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).sqsh
	docker rmi -f $(IMAGE_NAME):$(IMAGE_TAG) 2>/dev/null || true
