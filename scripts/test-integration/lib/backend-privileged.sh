#!/usr/bin/env bash
# Privileged backend for integration test setup/cleanup.

privileged_setup_filesystems() {
    echo "==> Formatting as btrfs..."
    run_root mkfs.btrfs -f -q "$IMG_SRC"
    run_root mkfs.btrfs -f -q "$IMG_RECV"

    echo "==> Mounting filesystems..."
    mkdir -p "$MNT_SRC" "$MNT_RECV"
    run_root mount -o loop "$IMG_SRC" "$MNT_SRC"
    run_root mount -o loop "$IMG_RECV" "$MNT_RECV"

    # Give ownership to the invoking user so basic file I/O works without root.
    run_root chown "$(id -u):$(id -g)" "$MNT_SRC" "$MNT_RECV"
}

privileged_setup_luks() {
    # Create sparse image and attach to loop device.
    LUKS_LOOP="$(run_root losetup --find --show "$LUKS_IMG")"

    # Format as LUKS with keyfile.
    run_root cryptsetup luksFormat --batch-mode --key-file "$LUKS_KEYFILE" "$LUKS_LOOP"

    # Add a text passphrase keyslot so passphrase_env tests work.
    echo -n "$LUKS_PASSPHRASE" | run_root cryptsetup luksAddKey --key-file "$LUKS_KEYFILE" "$LUKS_LOOP" -

    # Open, create btrfs inside, then close.
    local setup_name="btrbak_test_setup_$$"
    run_root cryptsetup open --key-file "$LUKS_KEYFILE" "$LUKS_LOOP" "$setup_name"
    run_root mkfs.btrfs -f -q "/dev/mapper/$setup_name"
    run_root cryptsetup close "$setup_name"
}

privileged_close_lingering_mappings() {
    if command -v dmsetup &>/dev/null && command -v cryptsetup &>/dev/null; then
        dmsetup ls 2>/dev/null | awk '/^btrbak_test_/{print $1}' | while read -r name; do
            run_root cryptsetup close "$name" 2>/dev/null || true
        done || true
    fi
}

privileged_cleanup() {
    privileged_close_lingering_mappings

    run_root umount "$MNT_SRC" 2>/dev/null || true
    run_root umount "$MNT_RECV" 2>/dev/null || true

    if [[ -n "$SRC_LOOP" ]]; then
        run_root losetup -d "$SRC_LOOP" 2>/dev/null || true
    fi

    if [[ -n "$RECV_LOOP" ]]; then
        run_root losetup -d "$RECV_LOOP" 2>/dev/null || true
    fi

    if [[ -n "$LUKS_LOOP" ]]; then
        run_root losetup -d "$LUKS_LOOP" 2>/dev/null || true
    fi
}
