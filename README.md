# backup-btrfs

A Rust tool for creating incremental Btrfs backups of multiple directories with optional LUKS encryption and live boot environment support.

## Overview

`backup-btrfs` is a reliable backup solution for Btrfs filesystems that provides:

- **Multi-source backups** - Simultaneous backup of multiple directories with consistent naming
- **Incremental transfers** - Efficient `btrfs send/receive` with changed data only
- **Live boot environments** - Bootable backup environments for system recovery
- **LUKS encryption** - Secure offsite backups with optional encryption
- **Snapper integration** - Works with existing snapper configurations
- **Resource-safe operations** - RAII patterns for automatic cleanup and error recovery

## Key Features

- 🔄 **Multi-source incremental backups** - Backup multiple directories simultaneously with efficient data transfer
- 🔒 **Optional LUKS encryption** - Secure backup storage with keyfile or environment variable auth
- 🚀 **Live boot environments** - Create bootable backup systems for disaster recovery
- 🛡️ **Safe resource management** - RAII patterns ensure clean mount/unmount and error recovery
- ⚙️ **Flexible TOML configuration** - Declarative configuration with sensible defaults
- 🔌 **Snapper integration** - Leverage existing snapper configurations for system directories
- 🎯 **Consistent volume naming** - Automatic `_vol` suffix naming across source and target
- ⚡ **Parallel error handling** - One source failure doesn't stop other backups

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
# Backup multiple source directories simultaneously
[[sources]]
path = "/"                    # Root filesystem
snapshot_dir = ".snapshots"   # Local snapshot directory
use_snapper = false           # Use manual snapshot method
snapshot_name = "backup_btrfs" # Name for manual snapshots

[[sources]]
path = "/home"                # User home directories
snapshot_dir = ".snapshots"
use_snapper = false
snapshot_name = "backup_btrfs"

# Optional: backup system directories
# [[sources]]
# path = "/var"
# snapshot_dir = ".snapshots"
# use_snapper = true           # Use snapper for system directories
# snapper_config = "var"       # Snapper config name

[target]
location = "/mnt/backup"      # Backup destination
enable_live_boot = false      # Disable live boot environment

[hooks]
copy_kernel = true            # Copy kernel to ESP after backup
regenerate_fstab = true       # Update fstab in live environment
remove_snapper_config = true  # Clean up snapper configs
```

### Running Backups

```bash
# Validate your configuration (dry-run check)
backup-btrfs validate

# Run a backup
backup-btrfs backup

# Run a dry-run backup (no changes made)
backup-btrfs backup --dry-run

# List available snapshots
backup-btrfs list-snapshots

# Prepare live boot environment (initial setup)
backup-btrfs prepare-live
```

**Configuration Validation Tips:**
- Always run `backup-btrfs validate` before your first backup
- Use `--dry-run` to test backup operations without making changes
- Validation checks source paths, target accessibility, and configuration consistency
- Fix any validation errors before proceeding with actual backups

## Configuration Guide

### Source Configuration

The source configuration defines what you're backing up. You can configure multiple source directories to backup simultaneously using the `[[sources]]` array syntax:

```toml
# Backup configuration for a source directory
[[sources]]
# Required: path to the Btrfs subvolume to backup (must be a valid subvolume)
path = "/"

# Optional: directory for local snapshots (relative to source path)
# Default: ".snapshots"
snapshot_dir = ".snapshots"

# Optional: use snapper for snapshot management instead of manual method
# Default: false
use_snapper = false

# Optional: name for manual snapshots (ignored if use_snapper = true)
# Default: "backup_btrfs"
snapshot_name = "backup_btrfs"

# Optional: snapper configuration name (required if use_snapper = true)
# If not specified, inferred from source path basename
snapper_config = "root"

# Add more [[sources]] sections for additional directories
[[sources]]
path = "/home"
use_snapper = false

[[sources]]
path = "/var"
use_snapper = true
snapper_config = "var"  # Requires snapper config "var" to exist
```

**Source Configuration Notes:**
1. **Volume Naming**: Source paths are converted to subvolume names with `_vol` suffix
   - `/` → `root_vol`
   - `/home` → `home_vol`
   - `/var/log` → `var_log_vol`
2. **Snapshot Storage**: Local snapshots are created in `source_path/snapshot_dir/`
3. **Snapper Integration**: When enabled, uses snapper with "backup_btrfs" description
4. **Incremental Backups**: Preserves previous snapshot for next incremental backup

**Requirements:**
- Source must be a Btrfs subvolume (verified during validation)
- The snapshot directory must exist or be creatable
- For snapper integration, snapper must be properly configured with the specified config

### Target Configuration

The target configuration defines where backups are stored and how they're organized:

```toml
[target]
# Required: backup destination - can be a mounted path or device identifier
# Examples:
#   "/mnt/backup" (already mounted filesystem)
#   "/dev/sda1" (device path)
#   "UUID=1234-5678" (device by UUID)
#   "LABEL=backup_drive" (device by label)
location = "/mnt/backup"

# Optional: enable live boot environment creation
# Default: false
enable_live_boot = false

# Optional: subvolume name for storing snapshots
# Default: "@snapshots" if live boot enabled, "." (root subvolume) otherwise
snapshot_subvolume = "@snapshots"

# Optional: subvolume name for live boot root environment
# Default: "@"
live_root_subvolume = "@"

# Optional: LUKS encryption configuration
[target.encryption]
# Optional: path to keyfile for automatic LUKS unlocking
keyfile = "/path/to/luks/keyfile"

# Optional: environment variable containing LUKS passphrase
passphrase_env = "BACKUP_PASSPHRASE"

# Optional: custom name for LUKS device mapping
# Default: "backup_target"
mapping_name = "custom_backup"
```

**Target Location Options:**

1. **Mounted Path**: Already mounted Btrfs filesystem
   ```toml
   location = "/mnt/backup"
   ```

2. **Device Path**: Raw device (automatically mounted)
   ```toml
   location = "/dev/sda1"
   ```

3. **Device by UUID**: More stable identifier
   ```toml
   location = "UUID=12345678-1234-1234-1234-123456789012"
   ```

4. **Device by Label**: Human-readable identifier
   ```toml
   location = "LABEL=backup_drive"
   ```

**Encryption Notes:**
- Provide either `keyfile` or `passphrase_env` (or both for fallback)
- Keyfiles should have restricted permissions (`chmod 600`)
- Environment variables are safer than hardcoded passphrases
- The device is automatically mounted/unmounted during backup operations

### Live Boot Environment

Create a bootable environment from your backups that can be used for system recovery:

```toml
[target]
location = "/mnt/backup"
enable_live_boot = true  # Required for live boot functionality

# Live boot configuration (required when enable_live_boot = true)
[live_boot]
# Required: path to EFI System Partition (ESP)
esp_path = "/mnt/efi"

# Optional: bootloader type (currently only systemd-boot supported)
# Default: SystemdBoot
bootloader = "SystemdBoot"

# Optional: bootloader entry configuration
[live_boot.boot_entry]
# Optional: title displayed in boot menu
# Default: "Backup Environment"
title = "Backup Environment"

# Optional: kernel path (relative to live boot root)
# Default: "/boot/vmlinuz-linux"
kernel = "/boot/vmlinuz-linux"

# Optional: initramfs path (relative to live boot root)
# Default: "/boot/initramfs-linux.img"
initramfs = "/boot/initramfs-linux.img"

# Optional: additional kernel command line options
# Default: empty vector
options = ["rw", "quiet", "rootflags=subvol=@/root_vol"]
```

**Live Boot Setup Process:**

1. **Initial Preparation**: Run `backup-btrfs prepare-live` once to:
   - Create `@` and `@snapshots` subvolumes on target
   - Initialize systemd-boot on the ESP
   - Create bootloader entries

2. **Post-Backup Updates**: After each backup:
   - Live environment is updated with latest snapshots
   - Hooks are executed (kernel copy, fstab regeneration, etc.)
   - Bootable environment mirrors your latest backup state

**Live Boot File System Layout:**
```
(btrfs top-level subvolume)
├── @snapshots         # Read-only backup storage
│   ├── root_vol       # Root filesystem backups
│   ├── home_vol       # Home directory backups
│   └── var_vol        # System directory backups
└── @                  # Live boot environment (writable)
    ├── root_vol       -> / (mounted at boot)
    ├── home_vol       -> /home (mounted at boot)
    └── var_vol        -> /var (mounted at boot)
```



### Hooks

Post-backup automation for live boot environments:

```toml
[hooks]
# Optional: copy kernel and initramfs to ESP after backup
# Default: true
copy_kernel = true

# Optional: regenerate fstab with correct subvolume mounts
# Default: true
regenerate_fstab = true

# Optional: remove snapper configuration from live environment
# Default: true
remove_snapper_config = true
```

**Hook Details:**

1. **copy_kernel**: Copies kernel (`vmlinuz-*`) and initramfs (`initramfs-*.img`) from `root_vol/boot/` in the live boot environment to the ESP. Also copies fallback initramfs if available. This ensures the backup environment can boot with the same kernel as the source system.

2. **regenerate_fstab**: Creates a new `/etc/fstab` in the live boot environment with:
   - Correct UUID-based device identifiers for the Btrfs filesystem
   - Proper subvolume mounts (`@/root_vol`, `@/home_vol`, etc.)
   - ESP mount entry if `/efi` directory exists in the live environment
   - Swap configuration if present

3. **remove_snapper_config**: Cleans up snapper configuration from the live boot environment to prevent snapper from modifying backup snapshots.

**Note**: Hooks only run when `enable_live_boot = true` and a live boot configuration is provided.

## File System Layout

Understanding the Btrfs subvolume naming and organization is crucial for effective backup management. `backup-btrfs` uses consistent naming conventions across source and target systems.

### Volume Naming Convention

Source paths are converted to standardized subvolume names with `_vol` suffix:

| Source Path | Subvolume Name | Description |
|-------------|----------------|-------------|
| `/` | `root_vol` | Root filesystem |
| `/home` | `home_vol` | User home directories |
| `/var` | `var_vol` | System variable data |
| `/var/log` | `var_log_vol` | Nested paths use underscores |
| `/opt/app` | `opt_app_vol` | Application directory |

### Source Layout Requirements

Each source directory must have a location for local snapshots (default: `.snapshots` within the source).

**Example source layout:**
```
# Source filesystem (live system)
/
├── .snapshots/           # Local snapshot directory (for /)
│   └── backup_btrfs     # Read-only snapshot for backup
├── home/
│   ├── user/
│   └── .snapshots/      # Local snapshot directory (for /home)
│       └── backup_btrfs
└── var/
    └── .snapshots/      # Local snapshot directory (for /var)
        └── backup_btrfs
```

**With snapper integration:**
```
/
├── .snapshots/
│   ├── 1/
│   │   └── snapshot     # Snapper snapshot #1
│   ├── 2/
│   │   └── snapshot     # Snapper snapshot #2 (backup_btrfs)
│   └── ...
└── ...
```

### Target Layout

The target layout depends on whether live boot environment is enabled.

**Without live boot environment (simple backup):**
```
# Target filesystem (backup destination)
/
├── root_vol/            # Backups of /
├── home_vol/            # Backups of /home
└── var_vol/             # Backups of /var
```

**With live boot environment (bootable backup):**
```
# Target filesystem (backup destination with boot capability)
/
├── @snapshots/          # Read-only backup storage
│   ├── root_vol/        # Root filesystem backups
│   ├── home_vol/        # Home directory backups
│   └── var_vol/         # System directory backups
└── @/                   # Live boot environment (writable)
    ├── root_vol -> /    # Mounted at boot as root
    ├── home_vol -> /home # Mounted at boot as /home
    └── var_vol -> /var  # Mounted at boot as /var
```

**ESP (EFI System Partition) layout:**
```
# ESP is a separate FAT32 partition (typically mounted at /efi or /boot/efi)
/efi/                    # ESP mount point
├── EFI/
│   └── systemd/        # systemd-boot files
├── vmlinuz-linux       # Copied kernel from live environment
├── initramfs-linux.img # Copied initramfs from live environment
└── loader/             # Bootloader configuration
    ├── loader.conf
    └── entries/
        └── backup.conf # Boot menu entry for backup environment
```

## How It Works

`backup-btrfs` performs parallel backup of multiple source directories with incremental transfers and optional live boot environment updates.

### Multi-Source Backup Process

For each configured source directory, the following steps are executed:

**1. Configuration & Validation**
- Load and validate TOML configuration
- Ensure target device is accessible (mount if needed)
- Verify all source paths are valid Btrfs subvolumes

**2. Per-Source Backup Pipeline**
Each source is processed independently with error isolation:

```
for each source in configuration.sources:
  1. Create local snapshot (manual or via snapper)
  2. Identify parent snapshot for incremental transfer
  3. Send snapshot to target with btrfs send/receive
  4. Clean up old local snapshots
```

**3. Local Snapshot Creation**
- **Manual method**: Creates read-only snapshot at `source/.snapshots/backup_btrfs`
- **Snapper method**: Creates snapper snapshot with "backup_btrfs" description
- Preserves previous snapshot as parent for next incremental backup

**4. Incremental Data Transfer**
```
# Full backup (first run):
btrfs send /source/.snapshots/backup_btrfs | btrfs receive /target/volume_vol

# Incremental backup (subsequent runs):
btrfs send -p /source/.snapshots/backup_btrfs_prev /source/.snapshots/backup_btrfs | btrfs receive /target/volume_vol
```

**5. Error Handling & Recovery**
- One source failure doesn't stop other backups
- Errors are collected and reported at the end
- Failed sources can be retried independently

### Live Boot Environment Management

**Initial Setup (`prepare-live` command):**
1. Creates `@` (live root) and `@snapshots` (backup storage) subvolumes
2. Initializes systemd-boot on the ESP partition
3. Creates bootloader entry with correct kernel parameters

**Post-Backup Updates:**
1. **Subvolume Replacement**: Atomically replaces each `@/volume_vol` with latest snapshot from `@snapshots/volume_vol`
2. **Hook Execution**:
   - `copy_kernel`: Copies kernel/initramfs to ESP
   - `regenerate_fstab`: Updates `/etc/fstab` with correct UUIDs and subvolume mounts
   - `remove_snapper_config`: Prevents snapper from modifying backup environment
3. **Boot Consistency**: Maintains bootable environment that mirrors latest backup state

### Volume Naming & Organization

The tool maintains consistent naming across source and target:

```
# Source (live system)
/                     -> snapshot at /.snapshots/backup_btrfs
/home                 -> snapshot at /home/.snapshots/backup_btrfs

# Target (backup storage)
@snapshots/root_vol   <- received snapshot of /
@snapshots/home_vol   <- received snapshot of /home

# Live boot environment
@/root_vol            -> mounted as / at boot
@/home_vol            -> mounted as /home at boot
```

## Architecture

`backup-btrfs` is built with reliability and safety as primary concerns, using modern Rust patterns and careful error handling.

### Resource Management & Safety

**RAII Patterns**
- `MountGuard`: Automatically manages mount points and LUKS mappings
- Cleanup on drop: Ensures no leftover mounts or encrypted mappings
- Atomic operations: Where possible, uses atomic renames and transactions

**Error Handling**
- Per-source error isolation: One source failure doesn't stop others
- Comprehensive error collection: All failures reported at end of backup
- Safe rollback: Failed operations attempt to clean up their state

### Core Components

**1. Configuration System**
- Type-safe TOML parsing with serde
- Runtime validation of paths, subvolumes, and dependencies
- Sensible defaults with explicit overrides

**2. Multi-Source Backup Engine**
- Parallel-sequential processing: Sources processed in order with error isolation
- Volume name normalization: Consistent `_vol` suffix naming
- Snapshot method abstraction: Unified interface for manual and snapper snapshots

**3. Device & Filesystem Management**
- Unified device handling: Mounted paths, raw devices, UUID/LABEL identifiers
- LUKS integration: Optional encryption with keyfile or environment variable
- Btrfs operations: Safe wrappers around btrfs commands with proper error checking

**4. Live Boot Environment**
- Bootloader management: systemd-boot initialization and configuration
- Atomic subvolume replacement: Safe updates of live environment
- Hook system: Extensible post-backup automation

**5. Hook System**
- Pluggable design: Easy to add new post-backup operations
- Conditional execution: Hooks only run when live boot is enabled
- Safe execution: Hook failures don't roll back successful backups

### Design Principles

1. **Explicit over implicit**: Configuration requires explicit enabling of features
2. **Safe defaults**: Operations default to safe, non-destructive behavior
3. **Comprehensive validation**: Validate early and often, fail fast
4. **Clean resource management**: RAII patterns for all external resources
5. **Informative errors**: Clear error messages with context and recovery suggestions

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
