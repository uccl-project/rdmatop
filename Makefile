IMAGE_NAME ?= efa
IMAGE_TAG ?= latest
OUTPUT_DIR ?= $(PWD)

.PHONY: all build clean docker fmt install

all: build

build:
	cargo build --release

clean:
	cargo clean

fmt:
	cargo fmt

install:
	cargo install --path .

docker:
	docker build -t $(IMAGE_NAME) .
	docker save $(IMAGE_NAME):$(IMAGE_TAG) | pigz > $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).tar.gz
	enroot import -o $(OUTPUT_DIR)/$(IMAGE_NAME)+$(IMAGE_TAG).sqsh dockerd://$(IMAGE_NAME):$(IMAGE_TAG)
