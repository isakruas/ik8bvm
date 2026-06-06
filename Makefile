.PHONY: all build clean

all: build

build:
	@echo "Building ik8bvm..."
	docker run --rm -v "$$(pwd):/workspace" -w /workspace rust:latest cargo build --release
	docker run --rm -v "$$(pwd):/workspace" -w /workspace rust:latest chown -R $$(id -u):$$(id -g) target || true

clean:
	@echo "Cleaning ik8bvm..."
	docker run --rm -v "$$(pwd):/workspace" -w /workspace rust:latest cargo clean
