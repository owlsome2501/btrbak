#!/usr/bin/env bash
#
# Integration test script for btrbak.
#
# Creates two btrfs filesystems backed by img files, mounts them, and runs
# cargo test with the appropriate environment variables.
#
# Root privileges (via sudo) are required only for:
#   - mkfs.btrfs  (formatting the img files)
#   - mount / umount (loop-mounting the img files)
#
# The test binary itself runs as the invoking (non-root) user.  btrfs
# subvolume operations (create, delete, send, receive) require
# CAP_SYS_ADMIN, so the test infrastructure probes for this at runtime and
# skips integration tests that cannot succeed.
#
# If sudo is unavailable the script exits 0 (skip) rather than failing.
#
# Usage:
#   bash scripts/test-integration.sh            # run all tests
#   bash scripts/test-integration.sh -- <args>  # forward extra args to cargo test
#

set -euo pipefail

IMG_SIZE="${IMG_SIZE:-512M}"
WORK_DIR=""

cleanup() {
    if [[ -n "$WORK_DIR" && -d "$WORK_DIR" ]]; then
        echo "==> Cleaning up..."
        sudo umount "$WORK_DIR/mnt_src" 2>/dev/null || true
        sudo umount "$WORK_DIR/mnt_recv" 2>/dev/null || true
        rm -rf "$WORK_DIR"
        echo "Done."
    fi
}

trap cleanup EXIT

# ── Privilege check ─────────────────────────────────────────────────────
if [[ "$(id -u)" -eq 0 ]]; then
    echo "Warning: running as root. The test binary will also run as root." >&2
elif ! sudo -v 2>/dev/null; then
    echo "Cannot obtain root privileges. Skipping integration tests."
    exit 0
fi

# ── Resolve project root ────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# ── Create img-backed btrfs filesystems ─────────────────────────────────
WORK_DIR="$(mktemp -d /tmp/btrbak-test.XXXXXX)"
IMG_SRC="$WORK_DIR/src.img"
IMG_RECV="$WORK_DIR/recv.img"
MNT_SRC="$WORK_DIR/mnt_src"
MNT_RECV="$WORK_DIR/mnt_recv"

echo "==> Creating img files ($IMG_SIZE each)..."
truncate -s "$IMG_SIZE" "$IMG_SRC"
truncate -s "$IMG_SIZE" "$IMG_RECV"

echo "==> Formatting as btrfs..."
sudo mkfs.btrfs -f -q "$IMG_SRC"
sudo mkfs.btrfs -f -q "$IMG_RECV"

echo "==> Mounting filesystems..."
mkdir -p "$MNT_SRC" "$MNT_RECV"
sudo mount -o loop "$IMG_SRC" "$MNT_SRC"
sudo mount -o loop "$IMG_RECV" "$MNT_RECV"

# Give ownership to the invoking user so basic file I/O works without root.
sudo chown "$(id -u):$(id -g)" "$MNT_SRC" "$MNT_RECV"

# ── Run tests ───────────────────────────────────────────────────────────
export BTRBAK_TEST_BTRFS_DIR="$MNT_SRC"
export BTRBAK_TEST_BTRFS_RECV_DIR="$MNT_RECV"

echo "==> Running tests..."
cd "$PROJECT_DIR"
cargo test "$@"
echo "==> All tests passed."
