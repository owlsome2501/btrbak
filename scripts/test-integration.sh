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
#   - cryptsetup   (LUKS setup, optional)
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
LUKS_LOOP=""

cleanup() {
    if [[ -n "$WORK_DIR" && -d "$WORK_DIR" ]]; then
        echo "==> Cleaning up..."

        # Close any btrbak_test_* dm-crypt mappings left behind by tests
        if command -v dmsetup &>/dev/null; then
            dmsetup ls 2>/dev/null | awk '/^btrbak_test_/{print $1}' | while read -r name; do
                sudo cryptsetup close "$name" 2>/dev/null || true
            done
        fi

        sudo umount "$WORK_DIR/mnt_src" 2>/dev/null || true
        sudo umount "$WORK_DIR/mnt_recv" 2>/dev/null || true

        # Detach LUKS loop device
        if [[ -n "$LUKS_LOOP" ]]; then
            sudo losetup -d "$LUKS_LOOP" 2>/dev/null || true
        fi

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

# ── LUKS test device (optional) ─────────────────────────────────────────
if command -v cryptsetup &>/dev/null; then
    echo "==> Setting up LUKS test device..."
    LUKS_IMG="$WORK_DIR/luks.img"
    LUKS_KEYFILE="$WORK_DIR/luks.key"
    LUKS_PASSPHRASE="test_passphrase"

    # Create sparse image and attach to loop device
    truncate -s "$IMG_SIZE" "$LUKS_IMG"
    LUKS_LOOP="$(sudo losetup --find --show "$LUKS_IMG")"

    # Generate random keyfile
    dd if=/dev/urandom of="$LUKS_KEYFILE" bs=32 count=1 2>/dev/null
    chmod 600 "$LUKS_KEYFILE"

    # Format as LUKS with keyfile
    sudo cryptsetup luksFormat --batch-mode --key-file "$LUKS_KEYFILE" "$LUKS_LOOP"

    # Add a text passphrase keyslot so passphrase_env tests work
    echo -n "$LUKS_PASSPHRASE" | sudo cryptsetup luksAddKey --key-file "$LUKS_KEYFILE" "$LUKS_LOOP" -

    # Open, create btrfs inside, close — so the container has a valid filesystem
    LUKS_SETUP_NAME="btrbak_test_setup_$$"
    sudo cryptsetup open --key-file "$LUKS_KEYFILE" "$LUKS_LOOP" "$LUKS_SETUP_NAME"
    sudo mkfs.btrfs -f -q "/dev/mapper/$LUKS_SETUP_NAME"
    sudo cryptsetup close "$LUKS_SETUP_NAME"

    export BTRBAK_TEST_LUKS_LOOP="$LUKS_LOOP"
    export BTRBAK_TEST_LUKS_KEYFILE="$LUKS_KEYFILE"
    export BTRBAK_TEST_LUKS_PASSPHRASE="$LUKS_PASSPHRASE"
    echo "    LUKS loop device: $LUKS_LOOP"
else
    echo "==> cryptsetup not found, skipping LUKS test setup."
fi

# ── Run tests ───────────────────────────────────────────────────────────
export BTRBAK_TEST_BTRFS_DIR="$MNT_SRC"
export BTRBAK_TEST_BTRFS_RECV_DIR="$MNT_RECV"

echo "==> Running tests..."
cd "$PROJECT_DIR"
cargo test "$@"
echo "==> All tests passed."
