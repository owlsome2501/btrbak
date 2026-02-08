#!/usr/bin/env bash
# Shared helpers for root-required test environment workflows.

set -euo pipefail

IMG_SIZE="${IMG_SIZE:-512M}"
BTRFS_MOUNT_OPTS="${BTRBAK_TEST_BTRFS_MOUNT_OPTS:-user_subvol_rm_allowed}"
WORK_DIR=""
PROJECT_DIR=""
STATE_FILE=""
ENV_FILE=""

IMG_SRC=""
IMG_RECV=""
SRC_LOOP=""
RECV_LOOP=""
MNT_SRC=""
MNT_RECV=""

LUKS_IMG=""
LUKS_LOOP=""
LUKS_KEYFILE=""
LUKS_PASSPHRASE=""

run_root() {
    if [[ "$(id -u)" -eq 0 ]]; then
        "$@"
    else
        sudo "$@"
    fi
}

ensure_root_access() {
    if [[ "$(id -u)" -eq 0 ]]; then
        return 0
    fi

    if ! command -v sudo >/dev/null 2>&1; then
        echo "ERROR: sudo is required for test environment preparation/cleanup." >&2
        return 1
    fi

    sudo -v
}

prepare_workspace() {
    PROJECT_DIR="$1"

    if [[ -n "${BTRBAK_TEST_WORK_DIR:-}" ]]; then
        WORK_DIR="$BTRBAK_TEST_WORK_DIR"
        mkdir -p "$WORK_DIR"
    else
        WORK_DIR="$(mktemp -d /tmp/btrbak-test.XXXXXX)"
    fi

    IMG_SRC="$WORK_DIR/src.img"
    IMG_RECV="$WORK_DIR/recv.img"
    MNT_SRC="$WORK_DIR/mnt_src"
    MNT_RECV="$WORK_DIR/mnt_recv"

    STATE_FILE="$WORK_DIR/state.sh"
    ENV_FILE="$WORK_DIR/env.sh"

    echo "==> Creating img files ($IMG_SIZE each)..."
    truncate -s "$IMG_SIZE" "$IMG_SRC"
    truncate -s "$IMG_SIZE" "$IMG_RECV"
}

setup_filesystems() {
    echo "==> Attaching loop devices..."
    SRC_LOOP="$(run_root losetup --find --show "$IMG_SRC")"
    RECV_LOOP="$(run_root losetup --find --show "$IMG_RECV")"

    echo "==> Formatting as btrfs..."
    run_root mkfs.btrfs -f -q "$SRC_LOOP"
    run_root mkfs.btrfs -f -q "$RECV_LOOP"

    echo "==> Mounting filesystems..."
    mkdir -p "$MNT_SRC" "$MNT_RECV"
    if [[ -n "$BTRFS_MOUNT_OPTS" ]]; then
        run_root mount -o "$BTRFS_MOUNT_OPTS" "$SRC_LOOP" "$MNT_SRC"
        run_root mount -o "$BTRFS_MOUNT_OPTS" "$RECV_LOOP" "$MNT_RECV"
    else
        run_root mount "$SRC_LOOP" "$MNT_SRC"
        run_root mount "$RECV_LOOP" "$MNT_RECV"
    fi

    # Mounted-filesystem tests run as normal user; make mountpoints writable.
    run_root chown "$(id -u):$(id -g)" "$MNT_SRC" "$MNT_RECV"
}

setup_luks_if_available() {
    if ! command -v cryptsetup >/dev/null 2>&1; then
        echo "==> cryptsetup not found, skipping LUKS test setup."
        return 0
    fi

    echo "==> Setting up LUKS test device..."
    LUKS_IMG="$WORK_DIR/luks.img"
    LUKS_KEYFILE="$WORK_DIR/luks.key"
    LUKS_PASSPHRASE="test_passphrase"

    truncate -s "$IMG_SIZE" "$LUKS_IMG"
    dd if=/dev/urandom of="$LUKS_KEYFILE" bs=32 count=1 2>/dev/null
    chmod 600 "$LUKS_KEYFILE"

    LUKS_LOOP="$(run_root losetup --find --show "$LUKS_IMG")"
    run_root cryptsetup luksFormat --batch-mode --key-file "$LUKS_KEYFILE" "$LUKS_LOOP"
    echo -n "$LUKS_PASSPHRASE" | run_root cryptsetup luksAddKey --key-file "$LUKS_KEYFILE" "$LUKS_LOOP" -

    local setup_name="btrbak_test_setup_$$"
    run_root cryptsetup open --key-file "$LUKS_KEYFILE" "$LUKS_LOOP" "$setup_name"
    run_root mkfs.btrfs -f -q "/dev/mapper/$setup_name"
    run_root cryptsetup close "$setup_name"
}

strict_mode_enabled() {
    case "${BTRBAK_STRICT_INTEGRATION:-1}" in
        1|true|TRUE|yes|YES|on|ON) return 0 ;;
        *) return 1 ;;
    esac
}

probe_btrfs_subvolume_ops() {
    local base="$1"
    local probe="$base/.btrbak_probe_$$"
    btrfs subvolume create "$probe" >/dev/null 2>&1 || return 1
    btrfs subvolume delete "$probe" >/dev/null 2>&1
}

verify_btrfs_capability_or_fail() {
    if ! strict_mode_enabled; then
        return 0
    fi

    if ! probe_btrfs_subvolume_ops "$BTRBAK_TEST_BTRFS_DIR"; then
        echo "ERROR: strict mode enabled but cannot create+delete btrfs subvolume on source mount: $BTRBAK_TEST_BTRFS_DIR" >&2
        echo "       Re-prepare env via scripts/prepare-root-test-env.sh so it mounts with option: user_subvol_rm_allowed" >&2
        exit 1
    fi

    if ! probe_btrfs_subvolume_ops "$BTRBAK_TEST_BTRFS_RECV_DIR"; then
        echo "ERROR: strict mode enabled but cannot create+delete btrfs subvolume on receive mount: $BTRBAK_TEST_BTRFS_RECV_DIR" >&2
        echo "       Re-prepare env via scripts/prepare-root-test-env.sh so it mounts with option: user_subvol_rm_allowed" >&2
        exit 1
    fi
}

_write_state_var() {
    local key="$1"
    local value="$2"
    printf '%s=%q\n' "$key" "$value" >> "$STATE_FILE"
}

write_state_file() {
    : > "$STATE_FILE"
    _write_state_var "WORK_DIR" "$WORK_DIR"
    _write_state_var "IMG_SRC" "$IMG_SRC"
    _write_state_var "IMG_RECV" "$IMG_RECV"
    _write_state_var "SRC_LOOP" "$SRC_LOOP"
    _write_state_var "RECV_LOOP" "$RECV_LOOP"
    _write_state_var "MNT_SRC" "$MNT_SRC"
    _write_state_var "MNT_RECV" "$MNT_RECV"
    _write_state_var "LUKS_IMG" "$LUKS_IMG"
    _write_state_var "LUKS_LOOP" "$LUKS_LOOP"
    _write_state_var "LUKS_KEYFILE" "$LUKS_KEYFILE"
    _write_state_var "LUKS_PASSPHRASE" "$LUKS_PASSPHRASE"
}

write_env_file() {
    : > "$ENV_FILE"
    printf 'export BTRBAK_TEST_BTRFS_DIR=%q\n' "$MNT_SRC" >> "$ENV_FILE"
    printf 'export BTRBAK_TEST_BTRFS_RECV_DIR=%q\n' "$MNT_RECV" >> "$ENV_FILE"
    printf 'export BTRBAK_TEST_ENV_STATE_FILE=%q\n' "$STATE_FILE" >> "$ENV_FILE"

    if [[ -n "$LUKS_LOOP" ]]; then
        printf 'export BTRBAK_TEST_LUKS_LOOP=%q\n' "$LUKS_LOOP" >> "$ENV_FILE"
    fi
    if [[ -n "$LUKS_KEYFILE" ]]; then
        printf 'export BTRBAK_TEST_LUKS_KEYFILE=%q\n' "$LUKS_KEYFILE" >> "$ENV_FILE"
    fi
    if [[ -n "$LUKS_PASSPHRASE" ]]; then
        printf 'export BTRBAK_TEST_LUKS_PASSPHRASE=%q\n' "$LUKS_PASSPHRASE" >> "$ENV_FILE"
    fi
}

load_state_file() {
    local state="$1"

    if [[ ! -f "$state" ]]; then
        echo "ERROR: state file does not exist: $state" >&2
        return 1
    fi

    # shellcheck disable=SC1090
    source "$state"

    : "${WORK_DIR:?missing WORK_DIR in state file}"
    : "${MNT_SRC:?missing MNT_SRC in state file}"
    : "${MNT_RECV:?missing MNT_RECV in state file}"
}

close_lingering_mappings() {
    if command -v dmsetup >/dev/null 2>&1 && command -v cryptsetup >/dev/null 2>&1; then
        dmsetup ls 2>/dev/null | awk '/^btrbak_test_/{print $1}' | while read -r name; do
            run_root cryptsetup close "$name" 2>/dev/null || true
        done || true
    fi
}

cleanup_from_state() {
    close_lingering_mappings

    run_root umount "$MNT_SRC" 2>/dev/null || true
    run_root umount "$MNT_RECV" 2>/dev/null || true

    if [[ -n "${SRC_LOOP:-}" ]]; then
        run_root losetup -d "$SRC_LOOP" 2>/dev/null || true
    fi
    if [[ -n "${RECV_LOOP:-}" ]]; then
        run_root losetup -d "$RECV_LOOP" 2>/dev/null || true
    fi
    if [[ -n "${LUKS_LOOP:-}" ]]; then
        run_root losetup -d "$LUKS_LOOP" 2>/dev/null || true
    fi

    rm -rf "$WORK_DIR"
}
