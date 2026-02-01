.PHONY: build release check clippy fmt fmt-check test test-unit test-integration clean install

build:
	cargo build

release:
	cargo build --release

check:
	cargo check

clippy:
	cargo clippy -- -D warnings

fmt:
	cargo fmt

fmt-check:
	cargo fmt -- --check

test: test-unit

test-unit:
	cargo test

test-integration:
	bash scripts/test-integration.sh

clean:
	cargo clean

install:
	cargo install --path .
