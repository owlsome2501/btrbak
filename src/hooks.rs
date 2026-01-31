use crate::btrfs;
use crate::error::BackupError;
use log;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str;

/// Execute post-backup hooks
pub fn run_hooks(
    live_root: &Path,
    target_mount: &Path,
    esp_path: &Path,
    hook_config: &crate::config::HookConfig,
    boot_entry: &crate::config::BootEntryConfig,
    config: &crate::config::Config,
) -> Result<(), BackupError> {
    if hook_config.copy_kernel {
        copy_kernel_to_esp(live_root, esp_path, boot_entry)?;
    }

    if hook_config.regenerate_fstab {
        regenerate_fstab(live_root, target_mount, config)?;
    }

    if hook_config.remove_snapper_config {
        remove_snapper_config(live_root)?;
    }

    Ok(())
}

/// Copy kernel and initramfs from live boot root to ESP
fn copy_kernel_to_esp(
    live_root: &Path,
    esp_path: &Path,
    boot_entry: &crate::config::BootEntryConfig,
) -> Result<(), BackupError> {
    // Helper to convert absolute path to relative path within live_root
    fn to_live_root_path(live_root: &Path, path: &Path) -> PathBuf {
        let mut result = live_root.to_path_buf();
        // Strip leading '/' if present
        for component in path.components() {
            match component {
                std::path::Component::RootDir => continue,
                _ => result.push(component),
            }
        }
        result
    }

    let kernel_source = to_live_root_path(live_root, &boot_entry.kernel);
    let initramfs_source = to_live_root_path(live_root, &boot_entry.initramfs);

    // Fallback initramfs pattern: replace ".img" with "-fallback.img"
    let initramfs_fallback_source = if let Some(parent) = boot_entry.initramfs.parent() {
        let stem = boot_entry.initramfs.file_stem().unwrap_or_default();
        let extension = boot_entry.initramfs.extension().unwrap_or_default();
        let fallback_name = format!(
            "{}-fallback.{}",
            stem.to_string_lossy(),
            extension.to_string_lossy()
        );
        to_live_root_path(live_root, &parent.join(fallback_name))
    } else {
        // Fallback to default path if no parent
        live_root.join("boot/initramfs-linux-fallback.img")
    };

    // Destination filenames use the source filename (not full path)
    let kernel_dest = esp_path.join(kernel_source.file_name().unwrap_or_default());
    let initramfs_dest = esp_path.join(initramfs_source.file_name().unwrap_or_default());
    let initramfs_fallback_dest =
        esp_path.join(initramfs_fallback_source.file_name().unwrap_or_default());

    // Copy kernel
    if kernel_source.exists() {
        fs::copy(&kernel_source, &kernel_dest)?;
        log::info!(
            "Copied kernel: {} -> {}",
            kernel_source.display(),
            kernel_dest.display()
        );
    } else {
        log::warn!("Kernel not found at: {}", kernel_source.display());
    }

    // Copy initramfs
    if initramfs_source.exists() {
        fs::copy(&initramfs_source, &initramfs_dest)?;
        log::info!(
            "Copied initramfs: {} -> {}",
            initramfs_source.display(),
            initramfs_dest.display()
        );
    } else {
        log::warn!("Initramfs not found at: {}", initramfs_source.display());
    }

    // Copy fallback initramfs if exists
    if initramfs_fallback_source.exists() {
        fs::copy(&initramfs_fallback_source, &initramfs_fallback_dest)?;
        log::info!(
            "Copied fallback initramfs: {} -> {}",
            initramfs_fallback_source.display(),
            initramfs_fallback_dest.display()
        );
    }

    Ok(())
}

/// Regenerate fstab for live boot environment
fn regenerate_fstab(
    live_root: &Path,
    target_mount: &Path,
    config: &crate::config::Config,
) -> Result<(), BackupError> {
    log::info!("Regenerating fstab for live boot environment...");

    let fstab_path = live_root.join("etc/fstab");
    log::debug!("Target fstab path: {}", fstab_path.display());

    // Generate fstab entries
    log::info!("Generating fstab entries...");
    let fstab_content = generate_basic_fstab(target_mount, config);

    // Backup old fstab if exists
    if fstab_path.exists() {
        let backup_path = fstab_path.with_extension("fstab.backup");
        log::info!("Backing up existing fstab to: {}", backup_path.display());
        fs::copy(&fstab_path, &backup_path)?;
    } else {
        log::info!("No existing fstab found, creating new one");
    }

    // Analyze and log generated entries before writing
    let entries: Vec<&str> = fstab_content
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
        .collect();

    let line_count = fstab_content.lines().count();

    log::info!("Fstab analysis:");
    log::info!("  Total lines: {}", line_count);
    log::info!("  Filesystem entries: {}", entries.len());

    if !entries.is_empty() {
        log::info!("  Generated entries:");
        for entry in entries {
            // Parse entry for better display
            let parts: Vec<&str> = entry.split_whitespace().collect();
            if parts.len() >= 2 {
                let filesystem = parts[0];
                let mount_point = parts[1];
                log::info!("    - {} -> {}", filesystem, mount_point);
            } else {
                log::info!("    - {}", entry);
            }
        }
    }

    // Write new fstab
    log::info!("Writing new fstab to: {}", fstab_path.display());
    fs::write(&fstab_path, fstab_content)?;

    log::info!("Fstab regeneration complete!");

    Ok(())
}

/// Generate a basic fstab for live boot environment
fn generate_basic_fstab(target_mount: &Path, config: &crate::config::Config) -> String {
    let mut lines = vec![
        "# /etc/fstab: static file system information.".to_string(),
        "#".to_string(),
        "# Use 'blkid' to print the universally unique identifier for a".to_string(),
        "# device; this may be used with UUID= as a more robust way to name devices".to_string(),
        "# that works even if disks are added and removed. See fstab(5).".to_string(),
        "#".to_string(),
        "# <file system>             <mount point>  <type>  <options>  <dump>  <pass>".to_string(),
        "".to_string(),
    ];

    // Get root device UUID from the actual target mount point
    let root_uuid = get_device_uuid(target_mount).unwrap_or_else(|_| "...".to_string());

    // Generate entries for each source directory
    // For live boot environment, mount subvolumes from @/<volume_name>
    for source in &config.sources {
        let subvolume_name = btrfs::get_subvolume_name_with_suffix(&source.path);

        // Determine mount point (use source path)
        let mount_point = if source.path == Path::new("/") {
            "/".to_string()
        } else {
            source.path.display().to_string()
        };

        // Determine subvolume path - mount from @/<volume_name> in live boot environment
        let subvolume_path = format!("@/{}", subvolume_name);

        lines.push(format!(
            "# Live boot subvolume for: {}",
            source.path.display()
        ));
        lines.push(format!(
            "UUID={} {} btrfs rw,relatime,ssd,compress=zstd,space_cache=v2,subvol={} 0 0",
            root_uuid, mount_point, subvolume_path
        ));
        lines.push("".to_string());
    }

    lines.join("\n")
}

/// Get UUID of device mounted at a path
fn get_device_uuid(mount_point: &Path) -> Result<String, BackupError> {
    // Use findmnt to get device UUID
    let output = Command::new("findmnt")
        .arg("--mountpoint")
        .arg(mount_point)
        .arg("--output")
        .arg("UUID")
        .arg("--noheadings")
        .arg("--first-only")
        .output()?;

    if !output.status.success() {
        return Err(BackupError::Hook(format!(
            "Failed to get UUID for mount point {}",
            mount_point.display()
        )));
    }

    let uuid = str::from_utf8(&output.stdout)
        .map_err(|e| BackupError::Hook(format!("Invalid UTF-8 output: {}", e)))?
        .trim()
        .to_string();

    if uuid.is_empty() {
        return Err(BackupError::Hook(format!(
            "Empty UUID for mount point {}",
            mount_point.display()
        )));
    }

    Ok(uuid)
}

/// Remove snapper configuration from live boot environment
fn remove_snapper_config(live_root: &Path) -> Result<(), BackupError> {
    let snapper_config_dir = live_root.join("etc/snapper/configs");

    if snapper_config_dir.exists() {
        // Remove all snapper configs
        for entry in fs::read_dir(&snapper_config_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                fs::remove_file(&path)?;
            }
        }

        // Try to remove the configs directory
        let _ = fs::remove_dir(&snapper_config_dir);
    }

    // Also disable snapper service if present
    let snapper_service =
        live_root.join("etc/systemd/system/multi-user.target.wants/snapper-cleanup.service");
    if snapper_service.exists() {
        let _ = fs::remove_file(&snapper_service);
    }

    let snapper_timer =
        live_root.join("etc/systemd/system/timers.target.wants/snapper-cleanup.timer");
    if snapper_timer.exists() {
        let _ = fs::remove_file(&snapper_timer);
    }

    Ok(())
}

