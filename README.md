# backup-btrfs

A Rust tool for creating incremental Btrfs backups with optional LUKS encryption and live boot environment support.

## Overview

`backup-btrfs` is a reliable backup solution for Btrfs filesystems that provides:

- **Incremental backups** using `btrfs send/receive` for efficient storage
- **Live boot environments** that can be booted directly from backup storage
- **LUKS encryption** support for secure offsite backups
- **Snapper integration** for coordinated snapshot management
- **Resource-safe operations** using RAII patterns for automatic cleanup

## Key Features

- 🔄 **Efficient incremental backups** - Only transfers changed data between snapshots
- 🔒 **Optional encryption** - LUKS support for secure backup storage
- 🚀 **Live boot environments** - Create bootable backup environments
- 🛡️ **Safe resource management** - Automatic cleanup of mounts and temporary files
- ⚙️ **Flexible configuration** - TOML-based configuration with sensible defaults
- 🔌 **Snapper integration** - Works with existing snapper configurations

## Quick Start

### Installation

```bash
# Install from source
cargo install --path .

# Or build locally
cargo build --release
```

### Basic Configuration

Create a configuration file `backup-btrfs.toml`:

```toml
# Single source configuration
[[sources]]
path = "/"
snapshot_dir = ".snapshots"
use_snapper = false
snapshot_name = "backup_btrfs"

# Multiple sources configuration example:
# [[sources]]
# path = "/home"
# snapshot_dir = ".snapshots"
# use_snapper = false
# snapshot_name = "backup_btrfs"
# 
# [[sources]]
# path = "/var"
# snapshot_dir = ".snapshots"
# use_snapper = true
# snapper_config = "var"

[target]
location = "/mnt/backup"
enable_live_boot = false

[hooks]
copy_kernel = true
regenerate_fstab = true
remove_snapper_config = true
```

### Running Backups

```bash
# Validate your configuration
backup-btrfs validate

# Run a backup
backup-btrfs backup

# List available snapshots
backup-btrfs list-snapshots

# Prepare live boot environment (if enabled)
backup-btrfs prepare-live
```

## Configuration Guide

### Source Configuration

The source configuration defines what you're backing up. You can configure multiple source directories to backup simultaneously:

```toml
# Single source configuration
[[sources]]
path = "/"                    # Path to the Btrfs subvolume to backup
snapshot_dir = ".snapshots"   # Directory for local snapshots (relative to source)
use_snapper = false           # Use snapper for snapshot management
snapshot_name = "backup_btrfs" # Name for manual snapshots
snapper_config = "root"       # Optional: snapper config name

# Multiple sources configuration (add more [[sources]] sections)
# [[sources]]
# path = "/home"
# snapshot_dir = ".snapshots"
# use_snapper = false
# snapshot_name = "backup_btrfs"
# 
# [[sources]]
# path = "/var"
# snapshot_dir = ".snapshots"
# use_snapper = true
# snapper_config = "var"
```

**Requirements:**
- Source must be a Btrfs subvolume
- The snapshot directory must exist or be creatable
- For snapper integration, snapper must be properly configured

### Target Configuration

The target configuration defines where backups are stored:

```toml
[target]
location = "/mnt/backup"     # Either a mounted path or device identifier
enable_live_boot = false     # Enable live boot environment
snapshot_subvolume = "@snapshots"  # Optional: subvolume for snapshots
live_root_subvolume = "@"    # Optional: subvolume for live boot root
```

**Target options:**
- `MountedPath("/path")` - Use an already mounted filesystem
- `Device("/dev/sdX")` - Mount a device automatically (supports UUID/LABEL paths)

### Live Boot Environment

Create a bootable environment from your backups:

```toml
[target]
location = "/mnt/backup"
enable_live_boot = true

[live_boot]
esp_path = "/mnt/efi"        # EFI System Partition path
bootloader = "SystemdBoot"   # Currently only systemd-boot supported

[live_boot.boot_entry]
title = "Backup Environment"
kernel = "/boot/vmlinuz-linux"
initramfs = "/boot/initramfs-linux.img"
options = ["rw", "quiet"]
```

### Encryption Support

Secure your backups with LUKS encryption:

```toml
[target]
location = "/dev/disk/by-uuid/12345678-1234-1234-1234-123456789012"

[target.encryption]
keyfile = "/path/to/luks/keyfile"        # Optional: keyfile path
passphrase_env = "BACKUP_PASSPHRASE"     # Optional: environment variable
mapping_name = "backup_target"           # Optional: LUKS mapping name
```

**Security Notes:**
- Store keyfiles with restricted permissions (e.g., `chmod 600`)
- Use environment variables from secure sources (secrets managers, etc.)
- Never commit credentials to version control

### Hooks

Post-backup automation:

```toml
[hooks]
copy_kernel = true           # Copy kernel/initramfs to ESP
regenerate_fstab = true      # Update fstab in live environment
remove_snapper_config = true # Prevent snapper from modifying live environment
```

## File System Layout

Understanding the Btrfs subvolume layout is essential for effective backup management.

### Source Layout Requirements

The source filesystem must have a location for local snapshots.
By default, this is a `.snapshots` directory or subvolume within the source being backed up.

**Simple layout example:**
```
(btrfs top-level subvolume)
├── root_vol                ->  /
├── root_snapshot_vol       ->  /.snapshots
├── home_user_vol           ->  /home/user
├── home_user_snapshot_vol  ->  /home/user/.snapshots
├── other_vol               ->  /any/mount/point/other
└── other_snapshot_vol      ->  /any/mount/point/other/.snapshots
```

**Advanced layout with separate snapshot subvolume:**
```
(esp)                  ->  /efi

(btrfs top-level subvolume)
├── @snapshots
│   ├── root_vol       ->  /.snapshots
│   └── home_user_vol  ->  /home/user/.snapshots
└── @
    ├── root_vol       ->  /
    ├── home_user_vol  ->  /home/user
    ├── var_vol        ->  /var
    ├── ...
    └── swap_vol       ->  /swap
```

### Target Layout

The target layout varies based on whether live boot is enabled:

**Without live boot environment:**
```
(target subvolume)
├── root_vol
├── home_user_vol
└── other_vol
```

**With live boot environment:**
```
(esp)                  ->  /efi

(btrfs top-level subvolume)
├── @snapshots         # Backup storage subvolume
│   ├── root_vol
│   └── home_user_vol
└── @                  # Live boot environment
    ├── root_vol       ->  /
    └── home_user_vol  ->  /home/user
```

## How It Works

### Incremental Backup Process

1. **Local snapshot creation** - Creates a snapshot of the source subvolume
2. **Parent identification** - Finds the previous snapshot for incremental transfer
3. **Data transfer** - Uses `btrfs send -p <parent> <snapshot> | btrfs receive`
4. **Live environment update** - Updates the live boot environment if enabled
5. **Cleanup** - Removes old local snapshots while preserving the latest for next backup

### Live Boot Environment Management

**Preparation phase:**
- Creates `@` and `@snapshots` subvolumes on target
- Initializes systemd-boot on the ESP (if provided)
- Creates boot loader entries

**Post-backup updates:**
- Replaces the live environment with the new snapshot
- Runs configured hooks (kernel copy, fstab regeneration, etc.)
- Maintains a bootable environment that mirrors your latest backup

## Architecture

`backup-btrfs` uses modern Rust patterns for reliable operation:

### Resource Management
- **RAII patterns** - `MountGuard` automatically manages mount points and LUKS mappings
- **Automatic cleanup** - Temporary directories and encrypted mappings are cleaned up on drop
- **Error safety** - Operations are atomic where possible, with proper rollback on failure

### Core Components
- **Configuration parsing** - Type-safe TOML configuration with validation
- **Device management** - Unified handling of mounted paths and raw devices
- **Encryption support** - Optional LUKS integration with keyfile or environment variable auth
- **Hook system** - Extensible post-backup operations

## Testing

Run the comprehensive test suite:

```bash
# Run all tests
cargo test

# Run library tests only
cargo test --lib

# Build with all warnings
cargo clippy -- -D warnings
```

**Test coverage includes:**
- Configuration parsing and validation
- Error type conversions and display
- Default value correctness
- File I/O operations with temporary files

Integration tests requiring external commands (btrfs, cryptsetup, etc.) are not included to avoid impacting real systems during development.

## Security Considerations

1. **Encryption keys** - Store LUKS keyfiles with minimal permissions and consider using hardware security modules for production use
2. **Environment variables** - Use secure environment variable management (e.g., systemd service files with `LoadCredential`)
3. **Backup storage** - Ensure backup media is physically secure when containing sensitive data
4. **Network security** - When backing up over networks, use encrypted transport (SSH, VPN, etc.)

## Contributing

1. Ensure all tests pass: `cargo test`
2. Check code quality: `cargo clippy -- -D warnings`
3. Maintain consistent formatting
4. Add tests for new functionality
5. Update documentation for any configuration changes

## License

This project is available under standard open-source licenses. See the LICENSE file for details.
