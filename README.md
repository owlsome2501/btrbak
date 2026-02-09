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
When live boot is enabled and `target.location` is a device identifier,
`prepare-live` generates a LUKS-aware boot entry automatically (including
`rd.luks.name` / `rd.luks.options`).

| Field            | Required | Default           | Description                                                               |
| ---------------- | -------- | ----------------- | ------------------------------------------------------------------------- |
| `keyfile`        | no       | —                 | Path to a LUKS keyfile. Should have restricted permissions (`chmod 600`). |
| `passphrase_env` | no       | —                 | Name of an environment variable containing the LUKS passphrase.           |
| `mapping_name`   | no       | `"backup_target"` | dm-crypt mapping name for the unlocked device.                            |

### `[live_boot]` — live boot environment (optional)

Required when `target.enable_live_boot = true`.

| Field          | Required | Default         | Description                                                                                                                                                 |
| -------------- | -------- | --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `esp_location` | yes      | —               | ESP location. Accepts a mounted path or device identifier (`/dev/...`, `UUID=...`, `LABEL=...`, `PARTUUID=...`). Device identifiers are auto-mounted.   |
| `esp_path`     | no       | `"/efi"`        | Mount point path inside live boot `root_vol` (used for generated `/etc/fstab`). Example: `"/efi"` or `"/boot/efi"`.                                     |
| `bootloader`   | no       | `"SystemdBoot"` | Bootloader type. Currently only `SystemdBoot` is supported.                                                                                               |

### `[live_boot.boot_entry]` — bootloader entry

| Field       | Required | Default                       | Description                                               |
| ----------- | -------- | ----------------------------- | --------------------------------------------------------- |
| `title`     | no       | `"Backup Environment"`        | Title displayed in the boot menu.                         |
| `kernel`    | no       | `"/boot/vmlinuz-linux"`       | Kernel image path inside the live boot root subvolume.    |
| `initramfs` | no       | `"/boot/initramfs-linux.img"` | Initramfs image path inside the live boot root subvolume. |
| `microcode` | no       | —                             | CPU microcode image path (e.g. `"/boot/amd-ucode.img"`).  |
| `options`   | no       | `[]`                          | Additional kernel command line options (excluding auto-managed `root=`, `rootflags=`, and `rd.luks.*`). |

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
make test                         # tests that need no env and no root
make test-no-root          # same as `make test`
make test-prepare-root-env     # create mounted btrfs test environment for root-required tests
make test-root-required          # tests that need root privileges
make test-cleanup-root-env     # cleanup mounted btrfs test environment
make test-integration             # external integration tests in tests/
make clean             # cargo clean
make install           # cargo install --path .
```

### Tests Without Env/Root

These tests are regular unit tests that do not require pre-mounted filesystems
or root permissions:

```bash
make test-no-root
```

### Root-Required Tests

These tests require root privileges (mount/LUKS, btrfs send/receive, and
`btrfs ... show`-dependent checks):

```bash
bash scripts/prepare-root-test-env.sh
source <env.sh path printed by prepare script>
bash scripts/test-root-required.sh
bash scripts/cleanup-root-test-env.sh
```

The script first runs `cargo test --no-run`, then executes only tests matching
`root_required_tests` with `sudo`, so other test categories are not run.

For non-interactive runs, `scripts/test-root-required.sh` can read sudo
password from `BTRBAK_SUDO_PASSWORD_FILE` (default is empty; no password file
is used unless explicitly set). Trailing CR/LF in that file is stripped before
passing to `sudo -S`.

### Rust Integration Tests

`tests/backup_workflow_integration.rs` contains external integration tests using
Rust's official `tests/` layout. Run them with:

```bash
make test-integration
# or
cargo test --test backup_workflow_integration
```

These tests exercise cross-module backup workflow behavior. They expect
`BTRBAK_TEST_BTRFS_DIR` and `BTRBAK_TEST_BTRFS_RECV_DIR` to point to mounted
btrfs filesystems.

For root-only workflow tests in `tests/backup_workflow_integration.rs`, use:

```bash
cargo test --test backup_workflow_integration root_required_tests
```

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
