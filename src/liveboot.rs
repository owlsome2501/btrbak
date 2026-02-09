use crate::btrfs;
use crate::command_runner;
use crate::config::{BootEntryConfig, LiveBootConfig};
use crate::error::BackupError;
use crate::ui;
use std::path::Path;
use std::process::Command;

/// Prepare live boot environment on a mounted btrfs filesystem
pub fn prepare_live_boot(
    btrfs_mount: &Path,
    config: &LiveBootConfig,
    esp_mount_path: &Path,
    live_boot_subvolume: &str,
    snapshot_subvolume: &str,
) -> Result<(), BackupError> {
    // Create subvolumes if they don't exist
    let snapshots_subvol = btrfs_mount.join(snapshot_subvolume);
    let root_subvol = btrfs_mount.join(live_boot_subvolume);

    if !snapshots_subvol.exists() {
        ui::substep(&format!(
            "Creating snapshots subvolume: {}",
            snapshot_subvolume
        ));
        btrfs::create_subvolume(&snapshots_subvol)?;
    }

    if !root_subvol.exists() {
        ui::substep(&format!(
            "Creating live boot subvolume: {}",
            live_boot_subvolume
        ));
        btrfs::create_subvolume(&root_subvol)?;
    }

    // Initialize bootloader if an ESP mount path is provided.
    if esp_mount_path.exists() {
        ui::substep("Initializing systemd-boot");
        init_systemd_boot(esp_mount_path)?;
        ui::substep("Creating boot entry");
        create_boot_entry(
            btrfs_mount,
            esp_mount_path,
            &config.boot_entry,
            live_boot_subvolume,
        )?;
    }

    Ok(())
}

/// Initialize systemd-boot on ESP
fn init_systemd_boot(esp_path: &Path) -> Result<(), BackupError> {
    let mut cmd = Command::new("bootctl");
    cmd.arg("--esp-path").arg(esp_path).arg("install");
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = command_runner::output(&mut cmd)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Bootloader(format!(
            "Failed to install systemd-boot to {}: {}",
            esp_path.display(),
            stderr
        )));
    }

    Ok(())
}

fn build_entry_options(
    entry_config: &BootEntryConfig,
    root_device: &str,
    live_boot_subvolume: &str,
) -> Vec<String> {
    let mut options: Vec<String> = entry_config
        .options
        .iter()
        .filter(|opt| !opt.starts_with("root=") && !opt.starts_with("rootflags=") && *opt != "rw")
        .cloned()
        .collect();
    options.push(format!("root=UUID={}", root_device));
    options.push(format!("rootflags=subvol={}/root_vol", live_boot_subvolume));
    options.push("rw".to_string());
    options
}

fn build_entry_content(entry_config: &BootEntryConfig, options: &[String]) -> String {
    let mut entry_lines = vec![
        format!("title   {}", entry_config.title),
        format!("linux   {}", entry_config.kernel.display()),
    ];

    // Add microcode initrd before main initramfs (required for CPU microcode loading).
    if let Some(microcode) = &entry_config.microcode {
        entry_lines.push(format!("initrd  {}", microcode.display()));
    }

    entry_lines.push(format!("initrd  {}", entry_config.initramfs.display()));
    entry_lines.push(format!("options {}", options.join(" ")));
    format!("{}\n", entry_lines.join("\n"))
}

/// Create a bootloader entry for the live environment
fn create_boot_entry(
    btrfs_mount: &Path,
    esp_path: &Path,
    entry_config: &BootEntryConfig,
    live_boot_subvolume: &str,
) -> Result<(), BackupError> {
    // Determine root device and subvolume
    let root_device = find_btrfs_device(btrfs_mount)?;
    let options = build_entry_options(entry_config, &root_device, live_boot_subvolume);
    let entry_content = build_entry_content(entry_config, &options);

    // Write entry file
    let entries_dir = esp_path.join("loader/entries");
    std::fs::create_dir_all(&entries_dir)?;

    let entry_file = entries_dir.join(format!("{}.conf", entry_config.title.replace(' ', "_")));
    std::fs::write(&entry_file, entry_content)?;

    Ok(())
}

/// Find the UUID of the btrfs filesystem mounted at the given path
fn find_btrfs_device(mount_point: &Path) -> Result<String, BackupError> {
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
        return Err(BackupError::Mount(format!(
            "Failed to find UUID for mount point {}",
            mount_point.display()
        )));
    }

    let uuid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if uuid.is_empty() {
        return Err(BackupError::Mount(format!(
            "Empty UUID for mount point {}",
            mount_point.display()
        )));
    }

    Ok(uuid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BootloaderType, LiveBootConfig, TargetLocation};
    use crate::test_util::{HookCommandRunner, scoped_hook_command_runner};
    use std::path::PathBuf;

    #[test]
    fn test_build_entry_options_filters_root_related_options() {
        let entry_config = BootEntryConfig {
            title: "Backup".to_string(),
            kernel: PathBuf::from("/boot/vmlinuz-linux"),
            initramfs: PathBuf::from("/boot/initramfs-linux.img"),
            microcode: None,
            options: vec![
                "quiet".to_string(),
                "root=UUID=old".to_string(),
                "rootflags=subvol=old".to_string(),
                "rw".to_string(),
                "loglevel=3".to_string(),
            ],
        };

        let options = build_entry_options(&entry_config, "new-uuid", "@");
        assert_eq!(
            options,
            vec![
                "quiet".to_string(),
                "loglevel=3".to_string(),
                "root=UUID=new-uuid".to_string(),
                "rootflags=subvol=@/root_vol".to_string(),
                "rw".to_string(),
            ]
        );
    }

    #[test]
    fn test_build_entry_content_puts_microcode_before_initramfs() {
        let entry_config = BootEntryConfig {
            title: "Backup".to_string(),
            kernel: PathBuf::from("/boot/vmlinuz-linux"),
            initramfs: PathBuf::from("/boot/initramfs-linux.img"),
            microcode: Some(PathBuf::from("/boot/amd-ucode.img")),
            options: vec![],
        };
        let options = vec!["quiet".to_string(), "rw".to_string()];

        let content = build_entry_content(&entry_config, &options);
        let micro_idx = content.find("initrd  /boot/amd-ucode.img").unwrap();
        let initramfs_idx = content.find("initrd  /boot/initramfs-linux.img").unwrap();

        assert!(micro_idx < initramfs_idx);
        assert!(content.contains("options quiet rw"));
    }

    mod root_required_tests_show_ops {
        use super::*;
        use crate::test_util::require_btrfs_test_dir;

        #[test]
        fn test_prepare_live_boot_creates_subvolumes_without_esp() {
            let td = require_btrfs_test_dir!("liveboot_prepare_create");

            let config = LiveBootConfig {
                esp_location: TargetLocation::MountedPath(td.path.join("esp-unused")),
                esp_path: PathBuf::from("/efi"),
                bootloader: BootloaderType::SystemdBoot,
                boot_entry: BootEntryConfig::default(),
            };

            let esp_mount = td.path.join("esp-missing");
            prepare_live_boot(&td.path, &config, &esp_mount, "@", "@snapshots").unwrap();

            assert!(btrfs::is_subvolume(&td.path.join("@")).unwrap());
            assert!(btrfs::is_subvolume(&td.path.join("@snapshots")).unwrap());
        }

        #[test]
        fn test_prepare_live_boot_is_idempotent_when_subvolumes_exist() {
            let td = require_btrfs_test_dir!("liveboot_prepare_idempotent");

            let root = td.path.join("@");
            let snaps = td.path.join("@snapshots");
            btrfs::create_subvolume(&root).unwrap();
            btrfs::create_subvolume(&snaps).unwrap();

            let root_id_before = btrfs::get_subvolume_id(&root).unwrap();
            let snaps_id_before = btrfs::get_subvolume_id(&snaps).unwrap();

            let config = LiveBootConfig {
                esp_location: TargetLocation::MountedPath(td.path.join("esp-unused")),
                esp_path: PathBuf::from("/efi"),
                bootloader: BootloaderType::SystemdBoot,
                boot_entry: BootEntryConfig::default(),
            };

            let esp_mount = td.path.join("esp-missing");
            prepare_live_boot(&td.path, &config, &esp_mount, "@", "@snapshots").unwrap();

            let root_id_after = btrfs::get_subvolume_id(&root).unwrap();
            let snaps_id_after = btrfs::get_subvolume_id(&snaps).unwrap();

            assert_eq!(root_id_before, root_id_after);
            assert_eq!(snaps_id_before, snaps_id_after);
        }
    }

    #[test]
    fn test_find_btrfs_device_injected_findmnt_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let _runner =
            scoped_hook_command_runner(HookCommandRunner::new().with_output_hook(|cmd| {
                if crate::test_util::command_program(cmd) == "findmnt" {
                    return Some(Ok(crate::test_util::mock_output(
                        5,
                        "",
                        "injected findmnt failure\n",
                    )));
                }
                None
            }));

        let result = find_btrfs_device(tmp.path());
        assert!(result.is_err());
        assert!(format!("{}", result.err().unwrap()).contains("Failed to find UUID"));
    }

    #[test]
    fn test_find_btrfs_device_injected_empty_uuid() {
        let tmp = tempfile::tempdir().unwrap();
        let _runner =
            scoped_hook_command_runner(HookCommandRunner::new().with_output_hook(|cmd| {
                if crate::test_util::command_program(cmd) == "findmnt" {
                    return Some(Ok(crate::test_util::mock_output(0, "\n", "")));
                }
                None
            }));

        let result = find_btrfs_device(tmp.path());
        assert!(result.is_err());
        assert!(format!("{}", result.err().unwrap()).contains("Empty UUID"));
    }

    #[test]
    fn test_prepare_live_boot_injected_bootctl_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path();
        std::fs::create_dir_all(mount.join("@")).unwrap();
        std::fs::create_dir_all(mount.join("@snapshots")).unwrap();

        let esp = mount.join("esp");
        std::fs::create_dir_all(&esp).unwrap();

        let config = LiveBootConfig {
            esp_location: TargetLocation::MountedPath(esp.clone()),
            esp_path: PathBuf::from("/efi"),
            bootloader: BootloaderType::SystemdBoot,
            boot_entry: BootEntryConfig::default(),
        };

        let _runner =
            scoped_hook_command_runner(HookCommandRunner::new().with_output_hook(|cmd| {
                if crate::test_util::command_program(cmd) == "bootctl" {
                    return Some(Ok(crate::test_util::mock_output(
                        9,
                        "",
                        "injected bootctl failure\n",
                    )));
                }
                None
            }));

        let result = prepare_live_boot(mount, &config, &esp, "@", "@snapshots");
        assert!(result.is_err());
        assert!(format!("{}", result.err().unwrap()).contains("Failed to install systemd-boot"));
    }
}
