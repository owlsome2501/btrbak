#!/usr/bin/env bash
# Shared functions for integration test orchestration.

IMG_SIZE="${IMG_SIZE:-512M}"
WORK_DIR=""
BACKEND=""
UDISKSCTL_BIN=""
SRC_LOOP=""
RECV_LOOP=""
LUKS_LOOP=""
MNT_SRC=""
MNT_RECV=""
PROJECT_DIR=""
IMG_SRC=""
IMG_RECV=""
LUKS_IMG=""
LUKS_KEYFILE=""
LUKS_PASSPHRASE=""

find_udisksctl() {
    if command -v udisksctl &>/dev/null; then
        echo "udisksctl"
        return 0
    fi
    return 1
}

run_root() {
    if [[ "$BACKEND" == "sudo" ]]; then
        sudo "$@"
    else
        "$@"
    fi
}

extract_device_path() {
    awk '{for (i = 1; i <= NF; i++) if ($i ~ /^\/dev\//) { gsub(/\.$/, "", $i); print $i; exit }}'
}

extract_last_device_path() {
    awk '{for (i = NF; i >= 1; i--) if ($i ~ /^\/dev\//) { gsub(/\.$/, "", $i); print $i; exit }}'
}

extract_mount_path() {
    awk '/^Mounted / {for (i = 1; i <= NF; i++) if ($i == "at") {print $(i + 1); exit}}' | sed 's/\.$//'
}

select_backend() {
    if [[ "$(id -u)" -eq 0 ]]; then
        BACKEND="root"
        echo "Warning: running as root. The test binary will also run as root." >&2
        return 0
    fi

    if UDISKSCTL_BIN="$(find_udisksctl)"; then
        if udisks_requirements_ok; then
            BACKEND="udisks"
            echo "==> Using user-space backend: $UDISKSCTL_BIN (+ gdbus)"
            return 0
        fi

        if sudo -v 2>/dev/null; then
            BACKEND="sudo"
            echo "==> gdbus not found for UDisks2 formatting; falling back to sudo backend"
            return 0
        fi

        echo "Cannot prepare integration test environment (gdbus missing and cannot use sudo). Skipping integration tests."
        exit 0
    fi

    if sudo -v 2>/dev/null; then
        BACKEND="sudo"
        echo "==> user-space disk tools not found; falling back to sudo backend"
        return 0
    fi

    echo "Cannot prepare integration test environment (missing udisksctl and sudo). Skipping integration tests."
    exit 0
}

prepare_workspace() {
    PROJECT_DIR="$1"

    WORK_DIR="$(mktemp -d /tmp/btrbak-test.XXXXXX)"
    IMG_SRC="$WORK_DIR/src.img"
    IMG_RECV="$WORK_DIR/recv.img"
    MNT_SRC="$WORK_DIR/mnt_src"
    MNT_RECV="$WORK_DIR/mnt_recv"

    echo "==> Creating img files ($IMG_SIZE each)..."
    truncate -s "$IMG_SIZE" "$IMG_SRC"
    truncate -s "$IMG_SIZE" "$IMG_RECV"
}

setup_filesystems() {
    case "$BACKEND" in
        udisks)
            udisks_setup_filesystems
            ;;
        sudo|root)
            privileged_setup_filesystems
            ;;
        *)
            echo "Unknown backend: $BACKEND" >&2
            exit 1
            ;;
    esac
}

setup_luks_if_available() {
    if ! command -v cryptsetup &>/dev/null; then
        echo "==> cryptsetup not found, skipping LUKS test setup."
        return
    fi

    echo "==> Setting up LUKS test device..."
    LUKS_IMG="$WORK_DIR/luks.img"
    LUKS_KEYFILE="$WORK_DIR/luks.key"
    LUKS_PASSPHRASE="test_passphrase"

    truncate -s "$IMG_SIZE" "$LUKS_IMG"
    dd if=/dev/urandom of="$LUKS_KEYFILE" bs=32 count=1 2>/dev/null
    chmod 600 "$LUKS_KEYFILE"

    case "$BACKEND" in
        udisks)
            udisks_setup_luks
            ;;
        sudo|root)
            privileged_setup_luks
            ;;
        *)
            echo "Unknown backend: $BACKEND" >&2
            exit 1
            ;;
    esac

    export BTRBAK_TEST_LUKS_LOOP="$LUKS_LOOP"
    export BTRBAK_TEST_LUKS_KEYFILE="$LUKS_KEYFILE"
    export BTRBAK_TEST_LUKS_PASSPHRASE="$LUKS_PASSPHRASE"
    echo "    LUKS loop device: $LUKS_LOOP"
}

strict_mode_enabled() {
    case "${BTRBAK_STRICT_INTEGRATION:-0}" in
        1|true|TRUE|yes|YES|on|ON) return 0 ;;
        *) return 1 ;;
    esac
}

probe_btrfs_subvolume_ops() {
    local base="$1"
    local probe="$base/.btrbak_probe_$$"
    if btrfs subvolume create "$probe" >/dev/null 2>&1; then
        btrfs subvolume delete "$probe" >/dev/null 2>&1 || true
        return 0
    fi
    return 1
}

verify_btrfs_capability_or_fail() {
    if ! strict_mode_enabled; then
        return 0
    fi

    if ! probe_btrfs_subvolume_ops "$MNT_SRC"; then
        echo "ERROR: strict mode enabled but cannot create btrfs subvolume on source mount: $MNT_SRC" >&2
        echo "       This would cause false-positive 'ok' tests via skip paths; fix permissions or run in a privileged environment." >&2
        exit 1
    fi

    if ! probe_btrfs_subvolume_ops "$MNT_RECV"; then
        echo "ERROR: strict mode enabled but cannot create btrfs subvolume on receive mount: $MNT_RECV" >&2
        echo "       This would cause false-positive 'ok' tests via skip paths; fix permissions or run in a privileged environment." >&2
        exit 1
    fi
}

run_tests() {
    export BTRBAK_TEST_BTRFS_DIR="$MNT_SRC"
    export BTRBAK_TEST_BTRFS_RECV_DIR="$MNT_RECV"
    export RUST_TEST_THREADS="${RUST_TEST_THREADS:-1}"
    export BTRBAK_STRICT_INTEGRATION="${BTRBAK_STRICT_INTEGRATION:-1}"

    echo "==> Source mount:   $MNT_SRC"
    echo "==> Receive mount:  $MNT_RECV"
    echo "==> Test threads:   $RUST_TEST_THREADS"
    echo "==> Strict checks:  $BTRBAK_STRICT_INTEGRATION"

    verify_btrfs_capability_or_fail

    echo "==> Running tests..."
    cd "$PROJECT_DIR"
    cargo test "$@"
    echo "==> All tests passed."
}

cleanup() {
    if [[ -z "$WORK_DIR" || ! -d "$WORK_DIR" ]]; then
        return
    fi

    echo "==> Cleaning up..."

    case "$BACKEND" in
        udisks)
            udisks_cleanup
            ;;
        sudo|root)
            privileged_cleanup
            ;;
    esac

    rm -rf "$WORK_DIR"
    echo "Done."
}
