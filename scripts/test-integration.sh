#!/usr/bin/env bash
#
# Integration test script for btrbak.
#
# Creates two btrfs filesystems backed by img files, mounts them, and runs
# cargo test with the appropriate environment variables.
#
# Preferred backend (non-root):
#   - udisksctl loop setup + mount
#   - UDisks2 D-Bus Block.Format (via gdbus) for mkfs
#
# Fallback backend (requires sudo):
#   - mkfs.btrfs, mount/umount, losetup, cryptsetup
#
# The test binary itself runs as the invoking user.
#
# Usage:
#   bash scripts/test-integration.sh            # run all tests
#   bash scripts/test-integration.sh -- <args>  # forward extra args to cargo test
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/test-integration/lib/common.sh"
source "$SCRIPT_DIR/test-integration/lib/backend-udisks.sh"
source "$SCRIPT_DIR/test-integration/lib/backend-privileged.sh"

trap cleanup EXIT

select_backend
prepare_workspace "$(dirname "$SCRIPT_DIR")"
setup_filesystems
setup_luks_if_available
run_tests "$@"
