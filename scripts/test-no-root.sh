#!/usr/bin/env bash
# Run tests that require neither prepared filesystem environment nor root.

set -euo pipefail

cargo_args=()
test_args=()
while [[ "$#" -gt 0 ]]; do
    if [[ "$1" == "--" ]]; then
        shift
        test_args=("$@")
        break
    fi
    cargo_args+=("$1")
    shift
done

cd "$(dirname "$0")/.."
echo "==> Running tests without env/root requirements..."
cargo test "${cargo_args[@]}" --lib -- --skip root_required_tests "${test_args[@]}"
