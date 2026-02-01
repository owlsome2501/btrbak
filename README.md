# btrbak

A Rust tool for creating incremental Btrfs backups of multiple directories with optional LUKS encryption and live boot environment support.

## Overview

`btrbak` is a reliable backup solution for Btrfs filesystems that provides:

- **Multi-source backups** - Simultaneous backup of multiple directories with consistent naming
- **Incremental transfers** - Efficient `btrfs send/receive` with changed data only
- **Live boot environments** - Bootable backup environments for system recovery
- **LUKS encryption** - Secure offsite backups with optional encryption
- **Snapper integration** - Works with existing snapper configurations

## Quick Start

### Installation

```bash
# Install from source
cargo install --path .
# or
make install

# Or build locally
cargo build --release
# or
make release
```

### Minimal Configuration

Create `btrbak.toml` (see the shipped [`btrbak.toml`](btrbak.toml) for a fully
annotated reference with every field):

```toml
name = "default"

[[sources]]
path = "/"

[[sources]]
path = "/home"

[target]
location = "/mnt/backup"
```

### Running Backups

```bash
btrbak validate                      # check configuration
btrbak backup                        # run backup
btrbak backup --dry-run              # dry-run (no changes)
btrbak prepare-live                  # initialize live boot environment
btrbak backup -c /path/to/config.toml  # custom config file
btrbak -v backup                     # verbose output
btrbak -q backup                     # errors only
```

## Configuration Reference

All available fields are documented below. Commented-out values in the tables
indicate defaults. The shipped [`btrbak.toml`](btrbak.toml) contains the same
information in a copy-pasteable TOML format.

### Top-level

| Field       | Required | Default    | Description                                                                                |
| ----------- | -------- | ---------- | ------------------------------------------------------------------------------------------ |
| `name`      | yes      | —          | Configuration name, used to distinguish different backup targets. Must be non-empty.       |
| `sources`   | yes      | —          | Array of source subvolume entries (see below). Alias `source` (singular) is also accepted. |
| `target`    | yes      | —          | Target backup location (see below).                                                        |
| `live_boot` | no       | —          | Live boot environment configuration. Required when `target.enable_live_boot = true`.       |
| `hooks`     | no       | all `true` | Post-backup hooks. Only effective when live boot is enabled.                               |

### `[[sources]]` — source subvolumes

At least one entry is required. Source paths are converted to target subvolume
names automatically: `/` -> `root_vol`, `/home` -> `home_vol`,
`/var/log` -> `var_log_vol`.

| Field            | Required                  | Default                     | Description                                                                           |
| ---------------- | ------------------------- | --------------------------- | ------------------------------------------------------------------------------------- |
| `path`           | yes                       | —                           | Absolute path to the btrfs subvolume to back up. Must be an existing btrfs subvolume. |
| `snapshot_dir`   | no                        | `".snapshots"`              | Directory for local snapshots, relative to the source path.                           |
| `use_snapper`    | no                        | `false`                     | Use snapper for snapshot management instead of manual creation.                       |
| `snapshot_name`  | no                        | `"btrbak"`                  | Name of the manual snapshot subvolume. Ignored when `use_snapper = true`.             |
| `snapper_config` | when `use_snapper = true` | inferred from path basename | Snapper configuration name.                                                           |

### `[target]` — backup destination

| Field                 | Required | Default                                        | Description                                                                                                                                                                                       |
| --------------------- | -------- | ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `location`            | yes      | —                                              | Backup destination. Accepts a mounted path (`/mnt/backup`) or a device identifier (`/dev/sda1`, `UUID=...`, `LABEL=...`, `PARTUUID=...`). Device identifiers are automatically mounted/unmounted. |
| `enable_live_boot`    | no       | `false`                                        | Enable live boot environment support. Requires a `[live_boot]` section.                                                                                                                           |
| `snapshot_subvolume`  | no       | `"@snapshots"` (live boot) / `"."` (otherwise) | Subvolume name for storing backup snapshots on the target.                                                                                                                                        |
| `live_boot_subvolume` | no       | `"@"`                                          | Subvolume name for the live boot root environment.                                                                                                                                                |

### `[target.encryption]` — LUKS encryption (optional)

At least one of `keyfile` or `passphrase_env` must be provided.

| Field            | Required | Default           | Description                                                               |
| ---------------- | -------- | ----------------- | ------------------------------------------------------------------------- |
| `keyfile`        | no       | —                 | Path to a LUKS keyfile. Should have restricted permissions (`chmod 600`). |
| `passphrase_env` | no       | —                 | Name of an environment variable containing the LUKS passphrase.           |
| `mapping_name`   | no       | `"backup_target"` | dm-crypt mapping name for the unlocked device.                            |

### `[live_boot]` — live boot environment (optional)

Required when `target.enable_live_boot = true`.

| Field        | Required | Default         | Description                                                 |
| ------------ | -------- | --------------- | ----------------------------------------------------------- |
| `esp_path`   | yes      | —               | Path to the EFI System Partition (ESP). Must exist.         |
| `bootloader` | no       | `"SystemdBoot"` | Bootloader type. Currently only `SystemdBoot` is supported. |

### `[live_boot.boot_entry]` — bootloader entry

| Field       | Required | Default                       | Description                                               |
| ----------- | -------- | ----------------------------- | --------------------------------------------------------- |
| `title`     | no       | `"Backup Environment"`        | Title displayed in the boot menu.                         |
| `kernel`    | no       | `"/boot/vmlinuz-linux"`       | Kernel image path inside the live boot root subvolume.    |
| `initramfs` | no       | `"/boot/initramfs-linux.img"` | Initramfs image path inside the live boot root subvolume. |
| `microcode` | no       | —                             | CPU microcode image path (e.g. `"/boot/amd-ucode.img"`).  |
| `options`   | no       | `[]`                          | Additional kernel command line options.                   |

### `[hooks]` — post-backup hooks

Hooks only run when `enable_live_boot = true` and a `[live_boot]` section is present.

| Field                   | Required | Default | Description                                                                                           |
| ----------------------- | -------- | ------- | ----------------------------------------------------------------------------------------------------- |
| `copy_kernel`           | no       | `true`  | Copy kernel, initramfs (and fallback) from the live root to the ESP.                                  |
| `regenerate_fstab`      | no       | `true`  | Regenerate `/etc/fstab` in the live environment with correct UUIDs and subvolume mounts.              |
| `remove_snapper_config` | no       | `true`  | Remove snapper configuration from the live environment to prevent it from modifying backup snapshots. |

## How It Works

### Backup Process

For each configured source, the following pipeline is executed:

1. **Validate** — load configuration, verify source paths are btrfs subvolumes, ensure target is accessible.
2. **Snapshot** — create a read-only snapshot of the source (`manual` or `snapper` method).
3. **Transfer** — pipe the snapshot to the target with `btrfs send | btrfs receive`. Incremental transfers use the previous snapshot as a parent.
4. **Cleanup** — remove the old local snapshot, keeping only the latest for the next incremental run.

```
# First run (full):
btrfs send /.snapshots/btrbak | btrfs receive /target/

# Subsequent runs (incremental):
btrfs send -p /.snapshots/btrbak_prev /.snapshots/btrbak | btrfs receive /target/
```

Each source is processed independently — a failure in one does not block the others.

### Live Boot Environment

**Initial setup** (`btrbak prepare-live`):

1. Creates `@` (live root) and `@snapshots` (backup storage) subvolumes on the target.
2. Initializes systemd-boot on the ESP.
3. Creates a bootloader entry with the configured kernel parameters.

**Post-backup updates** (automatic after each backup):

1. Atomically replaces each `@/<vol>` with the latest snapshot from `@snapshots/<vol>`.
2. Runs hooks: copy kernel to ESP, regenerate fstab, remove snapper config.

### File System Layout

**Volume naming convention:**

| Source Path | Target Subvolume Name |
| ----------- | --------------------- |
| `/`         | `root_vol`            |
| `/home`     | `home_vol`            |
| `/var`      | `var_vol`             |
| `/var/log`  | `var_log_vol`         |

**Source layout layout without snapper:**

Each source directory must have a location for local snapshots (default: `.snapshots` within the source).

```
# Source filesystem (live system)
/
├── .snapshots/            # Local snapshot directory (for /)
│   └── btrbak-config-name # Read-only snapshot for backup
├── home/
│   ├── user/
│   └── .snapshots/        # Local snapshot directory (for /home)
│       └── btrbak-config-name
└── var/
    └── .snapshots/        # Local snapshot directory (for /var)
        └── btrbak-config-name
```

**Source layout layout with snapper integration:**

```
/
├── .snapshots/
│   ├── 1/
│   │   └── snapshot     # Snapper snapshot #1
│   ├── 2/
│   │   └── snapshot     # Snapper snapshot #2 (btrbak)
│   └── ...
└── ...
```

**Target layout without live boot:**

```
/                        (target btrfs root)
├── root_vol/            backup of /
├── home_vol/            backup of /home
└── var_vol/             backup of /var
```

**Target layout with live boot:**

```
/                        (target btrfs root)
├── @snapshots/          read-only backup storage
│   ├── root_vol/
│   ├── home_vol/
│   └── var_vol/
└── @/                   live boot environment (writable)
    ├── root_vol         mounted as / at boot
    ├── home_vol         mounted as /home at boot
    └── var_vol          mounted as /var at boot
```

**ESP layout:**

```
/efi/
├── EFI/systemd/         systemd-boot files
├── vmlinuz-linux        kernel copied from live environment
├── initramfs-linux.img  initramfs copied from live environment
└── loader/
    ├── loader.conf
    └── entries/
        └── backup.conf  boot menu entry
```

## Testing

A `Makefile` is provided for common tasks:

```bash
make build             # cargo build
make release           # cargo build --release
make check             # cargo check
make clippy            # cargo clippy -- -D warnings
make fmt               # cargo fmt
make fmt-check         # cargo fmt -- --check
make test              # unit tests (no root required)
make test-integration  # integration tests (requires sudo & btrfs-progs)
make clean             # cargo clean
make install           # cargo install --path .
```

### Unit tests

Unit tests cover configuration parsing, error handling, UI formatting, volume naming,
and other logic that does not require a real btrfs filesystem:

```bash
make test
# or
cargo test
```

### Integration tests

Integration tests exercise real btrfs operations (subvolume create/delete, snapshot,
send/receive, incremental backup) on temporary img-backed btrfs filesystems.

```bash
make test-integration
```

The script `scripts/test-integration.sh` handles all setup and teardown automatically:

1. Creates two sparse img files (default 512 MB each, configurable via `IMG_SIZE`)
2. Formats them as btrfs and loop-mounts them via `sudo`
3. Runs `cargo test` with `BTRBAK_TEST_BTRFS_DIR` and `BTRBAK_TEST_BTRFS_RECV_DIR` set
4. Unmounts and removes the img files on exit

Root privileges (`sudo`) are only used for filesystem setup and teardown.
If `sudo` is unavailable the script exits cleanly and integration tests are skipped.
Tests that require btrfs privileges also detect this at runtime and skip automatically,
so a plain `cargo test` always succeeds regardless of the environment.

## Security Considerations

1. **Encryption keys** - Store LUKS keyfiles with minimal permissions and consider using hardware security modules for production use
2. **Environment variables** - Use secure environment variable management (e.g., systemd service files with `LoadCredential`)
3. **Backup storage** - Ensure backup media is physically secure when containing sensitive data
4. **Network security** - When backing up over networks, use encrypted transport (SSH, VPN, etc.)

## Contributing

1. Ensure all tests pass: `make test`
2. Check code quality: `make clippy`
3. Check formatting: `make fmt-check`
4. Add tests for new functionality
5. Update documentation for any configuration changes

## License

This project is available under standard open-source licenses. See the LICENSE file for details.
