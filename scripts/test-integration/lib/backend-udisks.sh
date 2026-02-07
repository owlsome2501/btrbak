#!/usr/bin/env bash
# UDisks2 user-space backend for integration test setup.

udisks_requirements_ok() {
    command -v gdbus &>/dev/null
}

udisks_object_path() {
    local dev="$1"
    local out obj

    out="$($UDISKSCTL_BIN info --block-device "$dev")"
    obj="$(printf '%s\n' "$out" | awk 'NR == 1 { sub(/:$/, "", $1); print $1 }')"

    if [[ -z "$obj" ]]; then
        echo "Failed to parse UDisks2 object path for $dev" >&2
        exit 1
    fi

    printf '%s\n' "$obj"
}

udisks_loop_setup() {
    local img="$1"
    local out dev

    out="$($UDISKSCTL_BIN loop-setup --file "$img" --no-user-interaction)"
    dev="$(printf '%s\n' "$out" | extract_device_path)"

    if [[ -z "$dev" ]]; then
        echo "Failed to parse loop device from: $out" >&2
        exit 1
    fi

    printf '%s\n' "$dev"
}

udisks_format_btrfs() {
    local dev="$1"
    local obj

    obj="$(udisks_object_path "$dev")"
    gdbus call \
        --system \
        --dest org.freedesktop.UDisks2 \
        --object-path "$obj" \
        --method org.freedesktop.UDisks2.Block.Format \
        btrfs "{}" >/dev/null
}

udisks_mount_device() {
    local dev="$1"
    local out mnt

    out="$($UDISKSCTL_BIN mount --block-device "$dev" --no-user-interaction)"
    mnt="$(printf '%s\n' "$out" | extract_mount_path)"

    if [[ -z "$mnt" ]]; then
        echo "Failed to parse mount point from: $out" >&2
        exit 1
    fi

    printf '%s\n' "$mnt"
}

udisks_setup_filesystems() {
    echo "==> Configuring loop devices via $UDISKSCTL_BIN..."
    SRC_LOOP="$(udisks_loop_setup "$IMG_SRC")"
    RECV_LOOP="$(udisks_loop_setup "$IMG_RECV")"

    echo "==> Formatting as btrfs..."
    udisks_format_btrfs "$SRC_LOOP"
    udisks_format_btrfs "$RECV_LOOP"

    echo "==> Mounting filesystems..."
    MNT_SRC="$(udisks_mount_device "$SRC_LOOP")"
    MNT_RECV="$(udisks_mount_device "$RECV_LOOP")"
}

udisks_setup_luks() {
    # Format LUKS container directly in image file.
    cryptsetup luksFormat --batch-mode --key-file "$LUKS_KEYFILE" "$LUKS_IMG"
    echo -n "$LUKS_PASSPHRASE" | cryptsetup luksAddKey --key-file "$LUKS_KEYFILE" "$LUKS_IMG" -

    # Attach as loop, unlock via udisksctl, then format cleartext via UDisks2 D-Bus.
    LUKS_LOOP="$(udisks_loop_setup "$LUKS_IMG")"
    local unlock_out mapped
    unlock_out="$($UDISKSCTL_BIN unlock --block-device "$LUKS_LOOP" --key-file "$LUKS_KEYFILE" --no-user-interaction)"
    mapped="$(printf '%s\n' "$unlock_out" | extract_last_device_path)"
    if [[ -z "$mapped" ]]; then
        echo "Failed to parse unlocked LUKS mapping from: $unlock_out" >&2
        exit 1
    fi

    udisks_format_btrfs "$mapped"
    "$UDISKSCTL_BIN" lock --block-device "$LUKS_LOOP" --no-user-interaction
}

udisks_cleanup() {
    if [[ -n "$SRC_LOOP" ]]; then
        "$UDISKSCTL_BIN" unmount --block-device "$SRC_LOOP" --no-user-interaction 2>/dev/null || true
        "$UDISKSCTL_BIN" loop-delete --block-device "$SRC_LOOP" --no-user-interaction 2>/dev/null || true
    fi

    if [[ -n "$RECV_LOOP" ]]; then
        "$UDISKSCTL_BIN" unmount --block-device "$RECV_LOOP" --no-user-interaction 2>/dev/null || true
        "$UDISKSCTL_BIN" loop-delete --block-device "$RECV_LOOP" --no-user-interaction 2>/dev/null || true
    fi

    if [[ -n "$LUKS_LOOP" ]]; then
        "$UDISKSCTL_BIN" lock --block-device "$LUKS_LOOP" --no-user-interaction 2>/dev/null || true
        "$UDISKSCTL_BIN" loop-delete --block-device "$LUKS_LOOP" --no-user-interaction 2>/dev/null || true
    fi
}
