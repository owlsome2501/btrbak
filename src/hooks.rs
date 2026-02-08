use crate::btrfs;
use crate::command_runner;
use crate::error::BackupError;
use crate::ui;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str;

/// Determine the root filesystem path within the live boot environment.
/// The root filesystem is at `<live_boot_path>/<root_vol>` (e.g. `@/root_vol`).
fn find_root_vol_path(live_boot_path: &Path, config: &crate::config::Config) -> PathBuf {
    for source in &config.sources {
        if source.path == Path::new("/") {
            let vol_name = btrfs::get_subvolume_name_with_suffix(&source.path);
            return live_boot_path.join(vol_name);
        }
    }
    // Fallback if no source has path "/"
    live_boot_path.join("root_vol")
}

/// Execute post-backup hooks
pub fn run_hooks(
    live_boot_path: &Path,
    target_mount: &Path,
    esp_path: &Path,
    hook_config: &crate::config::HookConfig,
    boot_entry: &crate::config::BootEntryConfig,
    config: &crate::config::Config,
) -> Result<(), BackupError> {
    // All hooks that access root filesystem files need the root_vol path,
    // not the live boot path directly. The live boot (@) contains subvolumes
    // like root_vol, home_vol, etc. The actual root filesystem is root_vol.
    let root_vol_path = find_root_vol_path(live_boot_path, config);

    if hook_config.copy_kernel {
        ui::substep("Copying kernel and initramfs to ESP");
        copy_kernel_to_esp(&root_vol_path, esp_path, boot_entry)?;
    }

    if hook_config.regenerate_fstab {
        ui::substep("Regenerating fstab for live boot");
        regenerate_fstab(&root_vol_path, target_mount, esp_path, config)?;
    }

    if hook_config.remove_snapper_config {
        ui::substep("Removing snapper config from live boot");
        remove_snapper_config(&root_vol_path)?;
    }

    Ok(())
}

/// Copy kernel, initramfs, and microcode from live boot root volume to ESP
fn copy_kernel_to_esp(
    root_vol: &Path,
    esp_path: &Path,
    boot_entry: &crate::config::BootEntryConfig,
) -> Result<(), BackupError> {
    // Helper to convert absolute path to relative path within root_vol
    fn to_root_vol_path(root_vol: &Path, path: &Path) -> PathBuf {
        let mut result = root_vol.to_path_buf();
        // Strip leading '/' if present
        for component in path.components() {
            match component {
                std::path::Component::RootDir => continue,
                _ => result.push(component),
            }
        }
        result
    }

    let kernel_source = to_root_vol_path(root_vol, &boot_entry.kernel);
    let initramfs_source = to_root_vol_path(root_vol, &boot_entry.initramfs);

    // Fallback initramfs pattern: replace ".img" with "-fallback.img"
    let initramfs_fallback_source = if let Some(parent) = boot_entry.initramfs.parent() {
        let stem = boot_entry.initramfs.file_stem().unwrap_or_default();
        let extension = boot_entry.initramfs.extension().unwrap_or_default();
        let fallback_name = format!(
            "{}-fallback.{}",
            stem.to_string_lossy(),
            extension.to_string_lossy()
        );
        to_root_vol_path(root_vol, &parent.join(fallback_name))
    } else {
        root_vol.join("boot/initramfs-linux-fallback.img")
    };

    // Destination filenames use the source filename (not full path)
    let kernel_dest = esp_path.join(kernel_source.file_name().unwrap_or_default());
    let initramfs_dest = esp_path.join(initramfs_source.file_name().unwrap_or_default());
    let initramfs_fallback_dest =
        esp_path.join(initramfs_fallback_source.file_name().unwrap_or_default());

    // Copy kernel
    if kernel_source.exists() {
        fs::copy(&kernel_source, &kernel_dest)?;
        ui::detail(&format!(
            "Copied kernel: {} -> {}",
            kernel_source.display(),
            kernel_dest.display()
        ));
    } else {
        ui::warning(&format!("Kernel not found at: {}", kernel_source.display()));
    }

    // Copy initramfs
    if initramfs_source.exists() {
        fs::copy(&initramfs_source, &initramfs_dest)?;
        ui::detail(&format!(
            "Copied initramfs: {} -> {}",
            initramfs_source.display(),
            initramfs_dest.display()
        ));
    } else {
        ui::warning(&format!(
            "Initramfs not found at: {}",
            initramfs_source.display()
        ));
    }

    // Copy fallback initramfs if exists
    if initramfs_fallback_source.exists() {
        fs::copy(&initramfs_fallback_source, &initramfs_fallback_dest)?;
        ui::detail(&format!(
            "Copied fallback initramfs: {} -> {}",
            initramfs_fallback_source.display(),
            initramfs_fallback_dest.display()
        ));
    }

    // Copy microcode image if configured
    if let Some(microcode) = &boot_entry.microcode {
        let ucode_source = to_root_vol_path(root_vol, microcode);
        let ucode_dest = esp_path.join(ucode_source.file_name().unwrap_or_default());

        if ucode_source.exists() {
            fs::copy(&ucode_source, &ucode_dest)?;
            ui::detail(&format!(
                "Copied microcode: {} -> {}",
                ucode_source.display(),
                ucode_dest.display()
            ));
        } else {
            ui::warning(&format!(
                "Microcode not found at: {}",
                ucode_source.display()
            ));
        }
    }

    Ok(())
}

/// Regenerate fstab for live boot environment
fn regenerate_fstab(
    root_vol: &Path,
    target_mount: &Path,
    esp_path: &Path,
    config: &crate::config::Config,
) -> Result<(), BackupError> {
    let fstab_path = root_vol.join("etc/fstab");

    // Generate fstab entries
    let fstab_content = generate_basic_fstab(root_vol, target_mount, esp_path, config);

    // Backup old fstab if exists
    if fstab_path.exists() {
        let backup_path = fstab_path.with_extension("backup");
        ui::detail(&format!(
            "Backing up existing fstab to: {}",
            backup_path.display()
        ));
        fs::copy(&fstab_path, &backup_path)?;
    }

    // Analyze generated entries
    let entries: Vec<&str> = fstab_content
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
        .collect();

    ui::detail(&format!("Generated {} fstab entries", entries.len()));
    for entry in &entries {
        let parts: Vec<&str> = entry.split_whitespace().collect();
        if parts.len() >= 2 {
            ui::detail(&format!("  {} -> {}", parts[0], parts[1]));
        }
    }

    // Write new fstab
    fs::write(&fstab_path, fstab_content)?;
    ui::detail(&format!("Wrote fstab to: {}", fstab_path.display()));

    Ok(())
}

/// Generate a basic fstab for live boot environment
fn generate_basic_fstab(
    root_vol: &Path,
    target_mount: &Path,
    esp_path: &Path,
    config: &crate::config::Config,
) -> String {
    let mut lines = vec![
        "# /etc/fstab: static file system information.".to_string(),
        "#".to_string(),
        "# Use 'blkid' to print the universally unique identifier for a".to_string(),
        "# device; this may be used with UUID= as a more robust way to name devices".to_string(),
        "# that works even if disks are added and removed. See fstab(5).".to_string(),
        "#".to_string(),
        "# <file system> <dir> <type> <options> <dump> <pass>".to_string(),
        "".to_string(),
    ];

    // Get root device UUID from the actual target mount point
    let root_uuid = get_device_uuid(target_mount).unwrap_or_else(|_| "...".to_string());
    let btrfs_opts = "rw,relatime,ssd,compress-force=zstd,space_cache=v2";

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
            "UUID={}  {}  btrfs  {},subvol={}  0 0",
            root_uuid, mount_point, btrfs_opts, subvolume_path
        ));
    }
    lines.push("".to_string());

    // Add ESP mount entry if /efi directory exists in root volume
    let efi_dir = root_vol.join("efi");
    if efi_dir.exists() {
        if let Ok(esp_uuid) = get_device_uuid(esp_path) {
            lines.push(format!(
                "UUID={}  /efi  vfat  rw,relatime,fmask=0133,dmask=0022,codepage=437,iocharset=iso8859-1,shortname=mixed,utf8,errors=remount-ro  0 2",
                esp_uuid
            ));
        } else {
            ui::warning("Could not determine ESP UUID, skipping ESP fstab entry");
        }
    }

    // Add tmpfs for /tmp
    lines.push("tmpfs  /tmp  tmpfs  nodev,nosuid  0 0".to_string());
    lines.push("".to_string());

    lines.join("\n")
}

/// Get UUID of device mounted at a path
fn get_device_uuid(mount_point: &Path) -> Result<String, BackupError> {
    let mut cmd = Command::new("findmnt");
    cmd.arg("--mountpoint")
        .arg(mount_point)
        .arg("--output")
        .arg("UUID")
        .arg("--noheadings")
        .arg("--first-only");
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = command_runner::output(&mut cmd)?;

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
///
/// Removes config files from `etc/snapper/configs` and disables snapper systemd services.
fn remove_snapper_config(live_boot_path: &Path) -> Result<(), BackupError> {
    let snapper_config_dir = live_boot_path.join("etc/snapper/configs");

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
        live_boot_path.join("etc/systemd/system/multi-user.target.wants/snapper-cleanup.service");
    if snapper_service.exists() {
        let _ = fs::remove_file(&snapper_service);
    }

    let snapper_timer =
        live_boot_path.join("etc/systemd/system/timers.target.wants/snapper-cleanup.timer");
    if snapper_timer.exists() {
        let _ = fs::remove_file(&snapper_timer);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use tempfile::TempDir;

    // remove_snapper_config tests

    #[test]
    fn test_remove_snapper_config_removes_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create snapper config files
        let config_dir = root.join("etc/snapper/configs");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("root"), "SNAPPER_CONFIG").unwrap();
        fs::write(config_dir.join("home"), "SNAPPER_CONFIG").unwrap();

        remove_snapper_config(root).unwrap();

        // Config files should be removed
        assert!(!config_dir.join("root").exists());
        assert!(!config_dir.join("home").exists());
    }

    #[test]
    fn test_remove_snapper_config_removes_services() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create systemd service files
        let service_dir = root.join("etc/systemd/system/multi-user.target.wants");
        fs::create_dir_all(&service_dir).unwrap();
        fs::write(service_dir.join("snapper-cleanup.service"), "").unwrap();

        let timer_dir = root.join("etc/systemd/system/timers.target.wants");
        fs::create_dir_all(&timer_dir).unwrap();
        fs::write(timer_dir.join("snapper-cleanup.timer"), "").unwrap();

        remove_snapper_config(root).unwrap();

        assert!(!service_dir.join("snapper-cleanup.service").exists());
        assert!(!timer_dir.join("snapper-cleanup.timer").exists());
    }

    #[test]
    fn test_remove_snapper_config_no_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // No snapper directories exist - should succeed without error
        let result = remove_snapper_config(root);
        assert!(result.is_ok());
    }

    // find_root_vol_path tests

    #[test]
    fn test_find_root_vol_path_with_root_source() {
        let config = Config {
            name: "test".to_string(),
            sources: vec![
                SourceConfig {
                    path: PathBuf::from("/home"),
                    snapshot_dir: PathBuf::from(".snapshots"),
                    use_snapper: false,
                    snapshot_name: "btrbak".to_string(),
                    snapper_config: None,
                },
                SourceConfig {
                    path: PathBuf::from("/"),
                    snapshot_dir: PathBuf::from(".snapshots"),
                    use_snapper: false,
                    snapshot_name: "btrbak".to_string(),
                    snapper_config: None,
                },
            ],
            target: TargetConfig {
                location: TargetLocation::MountedPath(PathBuf::from("/mnt")),
                enable_live_boot: false,
                snapshot_subvolume: None,
                live_boot_subvolume: None,
                encryption: None,
            },
            live_boot: None,
            hooks: HookConfig::default(),
        };

        let live_boot_path = Path::new("/mnt/@");
        let result = find_root_vol_path(live_boot_path, &config);
        assert_eq!(result, PathBuf::from("/mnt/@/root_vol"));
    }

    #[test]
    fn test_find_root_vol_path_fallback() {
        let config = Config {
            name: "test".to_string(),
            sources: vec![SourceConfig {
                path: PathBuf::from("/home"),
                snapshot_dir: PathBuf::from(".snapshots"),
                use_snapper: false,
                snapshot_name: "btrbak".to_string(),
                snapper_config: None,
            }],
            target: TargetConfig {
                location: TargetLocation::MountedPath(PathBuf::from("/mnt")),
                enable_live_boot: false,
                snapshot_subvolume: None,
                live_boot_subvolume: None,
                encryption: None,
            },
            live_boot: None,
            hooks: HookConfig::default(),
        };

        let live_boot_path = Path::new("/mnt/@");
        let result = find_root_vol_path(live_boot_path, &config);
        // No "/" source, should fallback to "root_vol"
        assert_eq!(result, PathBuf::from("/mnt/@/root_vol"));
    }

    // copy_kernel_to_esp tests

    #[test]
    fn test_copy_kernel_to_esp_copies_files() {
        let tmp = TempDir::new().unwrap();
        let root_vol = tmp.path().join("root_vol");
        let esp = tmp.path().join("esp");
        fs::create_dir_all(&esp).unwrap();

        // Create kernel and initramfs in root_vol
        let boot_dir = root_vol.join("boot");
        fs::create_dir_all(&boot_dir).unwrap();
        fs::write(boot_dir.join("vmlinuz-linux"), "KERNEL_DATA").unwrap();
        fs::write(boot_dir.join("initramfs-linux.img"), "INITRAMFS_DATA").unwrap();

        let boot_entry = BootEntryConfig {
            title: "Test".to_string(),
            kernel: PathBuf::from("/boot/vmlinuz-linux"),
            initramfs: PathBuf::from("/boot/initramfs-linux.img"),
            microcode: None,
            options: vec![],
        };

        copy_kernel_to_esp(&root_vol, &esp, &boot_entry).unwrap();

        assert_eq!(
            fs::read_to_string(esp.join("vmlinuz-linux")).unwrap(),
            "KERNEL_DATA"
        );
        assert_eq!(
            fs::read_to_string(esp.join("initramfs-linux.img")).unwrap(),
            "INITRAMFS_DATA"
        );
    }

    #[test]
    fn test_copy_kernel_to_esp_missing_kernel() {
        let tmp = TempDir::new().unwrap();
        let root_vol = tmp.path().join("root_vol");
        let esp = tmp.path().join("esp");
        fs::create_dir_all(&root_vol).unwrap();
        fs::create_dir_all(&esp).unwrap();

        let boot_entry = BootEntryConfig {
            title: "Test".to_string(),
            kernel: PathBuf::from("/boot/vmlinuz-linux"),
            initramfs: PathBuf::from("/boot/initramfs-linux.img"),
            microcode: None,
            options: vec![],
        };

        // Should succeed (just warns, doesn't error)
        let result = copy_kernel_to_esp(&root_vol, &esp, &boot_entry);
        assert!(result.is_ok());
        // No files should be copied
        assert!(!esp.join("vmlinuz-linux").exists());
    }

    // generate_basic_fstab tests

    #[test]
    fn test_generate_fstab_structure() {
        let tmp = TempDir::new().unwrap();
        let root_vol = tmp.path().join("root_vol");
        let target_mount = tmp.path().join("target");
        let esp = tmp.path().join("esp");
        fs::create_dir_all(&root_vol).unwrap();
        fs::create_dir_all(&target_mount).unwrap();
        fs::create_dir_all(&esp).unwrap();

        let config = Config {
            name: "test".to_string(),
            sources: vec![
                SourceConfig {
                    path: PathBuf::from("/"),
                    snapshot_dir: PathBuf::from(".snapshots"),
                    use_snapper: false,
                    snapshot_name: "btrbak".to_string(),
                    snapper_config: None,
                },
                SourceConfig {
                    path: PathBuf::from("/home"),
                    snapshot_dir: PathBuf::from(".snapshots"),
                    use_snapper: false,
                    snapshot_name: "btrbak".to_string(),
                    snapper_config: None,
                },
            ],
            target: TargetConfig {
                location: TargetLocation::MountedPath(PathBuf::from("/mnt")),
                enable_live_boot: true,
                snapshot_subvolume: None,
                live_boot_subvolume: None,
                encryption: None,
            },
            live_boot: None,
            hooks: HookConfig::default(),
        };

        let fstab = generate_basic_fstab(&root_vol, &target_mount, &esp, &config);

        // Should contain header comments
        assert!(fstab.contains("# /etc/fstab"));
        // Should contain entries for both sources
        assert!(fstab.contains("subvol=@/root_vol"));
        assert!(fstab.contains("subvol=@/home_vol"));
        // Should contain tmpfs
        assert!(fstab.contains("tmpfs  /tmp"));
    }

    #[test]
    fn test_generate_fstab_single_source() {
        let tmp = TempDir::new().unwrap();
        let root_vol = tmp.path().join("root_vol");
        let target_mount = tmp.path().join("target");
        let esp = tmp.path().join("esp");
        fs::create_dir_all(&root_vol).unwrap();
        fs::create_dir_all(&target_mount).unwrap();
        fs::create_dir_all(&esp).unwrap();

        let config = Config {
            name: "test".to_string(),
            sources: vec![SourceConfig {
                path: PathBuf::from("/"),
                snapshot_dir: PathBuf::from(".snapshots"),
                use_snapper: false,
                snapshot_name: "btrbak".to_string(),
                snapper_config: None,
            }],
            target: TargetConfig {
                location: TargetLocation::MountedPath(PathBuf::from("/mnt")),
                enable_live_boot: true,
                snapshot_subvolume: None,
                live_boot_subvolume: None,
                encryption: None,
            },
            live_boot: None,
            hooks: HookConfig::default(),
        };

        let fstab = generate_basic_fstab(&root_vol, &target_mount, &esp, &config);

        // Should contain the root subvol entry with correct path
        assert!(fstab.contains("subvol=@/root_vol"));
        // Mount point for "/" source should be "/"
        assert!(fstab.contains("  /  btrfs"));
    }
}
