# Calyx: build and test everything (Rust compiler + C runtime).

.PHONY: all build test lint fmt clean

all: build

build:
	cargo build --release
	$(MAKE) -C runtime

test:
	cargo test
	$(MAKE) -C runtime test

lint:
	cargo fmt --all --check
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt --all

clean:
	cargo clean
	$(MAKE) -C runtime clean
