.PHONY: build release check clippy fmt fmt-check test test-unit test-no-root test-root-required test-prepare-root-env test-cleanup-root-env test-integration clean install

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

test: test-no-root

test-unit: test-no-root

test-no-root:
	./scripts/test-no-root.sh

test-integration:
	cargo test --test backup_workflow_integration

test-root-required:
	./scripts/test-root-required.sh

test-prepare-root-env:
	./scripts/prepare-root-test-env.sh

test-cleanup-root-env:
	./scripts/cleanup-root-test-env.sh

clean:
	cargo clean

install:
	cargo install --path .
