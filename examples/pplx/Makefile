IMAGE_NAME ?= pplx
IMAGE_TAG  ?= latest
BASE_IMAGE ?= efa:latest
OUTPUT_DIR ?= $(PWD)

.PHONY: all docker sqsh clean

all: sqsh

docker:
	docker build --build-arg BASE_IMAGE=$(BASE_IMAGE) -t $(IMAGE_NAME):$(IMAGE_TAG) .
	docker save $(IMAGE_NAME):$(IMAGE_TAG) | pigz > $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).tar.gz

sqsh: docker
	rm -f $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).sqsh
	enroot import -o $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).sqsh dockerd://$(IMAGE_NAME):$(IMAGE_TAG)

clean:
	rm -f $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).tar.gz $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).sqsh
	docker rmi -f $(IMAGE_NAME):$(IMAGE_TAG) 2>/dev/null || true
