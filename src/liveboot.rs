use crate::btrfs;
use crate::command_runner;
use crate::config::{BootEntryConfig, LiveBootConfig, TargetConfig, TargetLocation};
use crate::error::BackupError;
use crate::ui;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_RD_LUKS_OPTIONS: &str =
    "keyfile-timeout=3s,timeout=0,discard,password-echo=masked,tries=0";

#[derive(Debug, Clone)]
struct LuksBootConfig {
    luks_uuid: String,
    mapping_name: String,
}

/// Prepare live boot environment on a mounted btrfs filesystem
pub fn prepare_live_boot(
    btrfs_mount: &Path,
    config: &LiveBootConfig,
    target_config: &TargetConfig,
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
            target_config,
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
    luks_boot_config: Option<&LuksBootConfig>,
) -> Vec<String> {
    let mut options: Vec<String> = entry_config
        .options
        .iter()
        .filter(|opt| !is_managed_option(opt))
        .cloned()
        .collect();

    if let Some(luks) = luks_boot_config {
        options.push(format!(
            "rd.luks.name={}={}",
            luks.luks_uuid, luks.mapping_name
        ));
        options.push(format!(
            "rd.luks.options={}={}",
            luks.luks_uuid, DEFAULT_RD_LUKS_OPTIONS
        ));
        options.push("rootflags=x-systemd.device-timeout=0".to_string());
    }

    options.push(format!("root={}", root_device));
    options.push("rw".to_string());
    options.push(format!("rootflags=subvol={}/root_vol", live_boot_subvolume));
    options
}

fn build_entry_content(entry_config: &BootEntryConfig, options: &[String]) -> String {
    fn esp_asset_path(path: &Path) -> PathBuf {
        match path.file_name() {
            Some(file_name) => Path::new("/").join(file_name),
            None => path.to_path_buf(),
        }
    }

    let kernel_path = esp_asset_path(&entry_config.kernel);
    let initramfs_path = esp_asset_path(&entry_config.initramfs);
    let mut entry_lines = vec![
        format!("title   {}", entry_config.title),
        format!("linux   {}", kernel_path.display()),
    ];

    // Add microcode initrd before main initramfs (required for CPU microcode loading).
    if let Some(microcode) = &entry_config.microcode {
        let microcode_path = esp_asset_path(microcode);
        entry_lines.push(format!("initrd  {}", microcode_path.display()));
    }

    entry_lines.push(format!("initrd  {}", initramfs_path.display()));
    entry_lines.push(format!("options {}", options.join(" ")));
    format!("{}\n", entry_lines.join("\n"))
}

/// Create a bootloader entry for the live environment
fn create_boot_entry(
    btrfs_mount: &Path,
    esp_path: &Path,
    entry_config: &BootEntryConfig,
    target_config: &TargetConfig,
    live_boot_subvolume: &str,
) -> Result<(), BackupError> {
    // Determine root device and subvolume
    let root_uuid = find_btrfs_device(btrfs_mount)?;
    let root_device = resolve_root_device(target_config, &root_uuid);
    let luks_boot_config = resolve_luks_boot_config(target_config)?;
    let options = build_entry_options(
        entry_config,
        &root_device,
        live_boot_subvolume,
        luks_boot_config.as_ref(),
    );
    let entry_content = build_entry_content(entry_config, &options);

    // Write entry file
    let entries_dir = esp_path.join("loader/entries");
    std::fs::create_dir_all(&entries_dir)?;

    let entry_file = entries_dir.join(format!("{}.conf", entry_config.title.replace(' ', "_")));
    std::fs::write(&entry_file, entry_content)?;

    Ok(())
}

fn is_managed_option(opt: &str) -> bool {
    opt == "rw"
        || opt.starts_with("root=")
        || opt.starts_with("rootflags=")
        || opt.starts_with("rd.luks.name=")
        || opt.starts_with("rd.luks.options=")
}

fn resolve_root_device(target_config: &TargetConfig, root_uuid: &str) -> String {
    if target_config.encryption.is_none()
        && let TargetLocation::Device(device) = &target_config.location
        && (device.starts_with("UUID=")
            || device.starts_with("LABEL=")
            || device.starts_with("PARTUUID="))
    {
        return device.clone();
    }

    format!("UUID={}", root_uuid)
}

fn resolve_luks_boot_config(
    target_config: &TargetConfig,
) -> Result<Option<LuksBootConfig>, BackupError> {
    let Some(encryption) = &target_config.encryption else {
        return Ok(None);
    };

    let target_device = match &target_config.location {
        TargetLocation::Device(device) => device.as_str(),
        TargetLocation::MountedPath(_) => return Ok(None),
    };

    let resolved = resolve_block_device(target_device)?;
    let luks_uuid = find_luks_uuid(&resolved)?;

    Ok(Some(LuksBootConfig {
        luks_uuid,
        mapping_name: encryption.mapping_name.clone(),
    }))
}

fn resolve_block_device(device: &str) -> Result<String, BackupError> {
    if device.starts_with("/dev/") {
        return Ok(device.to_string());
    }

    if device.starts_with("UUID=")
        || device.starts_with("LABEL=")
        || device.starts_with("PARTUUID=")
    {
        let mut cmd = Command::new("findfs");
        cmd.arg(device);
        ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

        let output = command_runner::output(&mut cmd)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BackupError::Bootloader(format!(
                "Failed to resolve target device identifier {} for LUKS boot options: {}",
                device, stderr
            )));
        }

        let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if resolved.starts_with("/dev/") {
            return Ok(resolved);
        }

        return Err(BackupError::Bootloader(format!(
            "Resolved target device identifier {} to invalid block device path: {}",
            device, resolved
        )));
    }

    Ok(device.to_string())
}

fn find_luks_uuid(device: &str) -> Result<String, BackupError> {
    let mut cmd = Command::new("cryptsetup");
    cmd.arg("luksUUID").arg(device);
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = command_runner::output(&mut cmd)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BackupError::Bootloader(format!(
            "Failed to query LUKS UUID for {}: {}",
            device, stderr
        )));
    }

    let luks_uuid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if luks_uuid.is_empty() {
        return Err(BackupError::Bootloader(format!(
            "Empty LUKS UUID for {}",
            device
        )));
    }

    Ok(luks_uuid)
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
    use crate::config::{BootloaderType, EncryptionConfig, LiveBootConfig, TargetLocation};
    use crate::test_util::{HookCommandRunner, scoped_hook_command_runner};
    use std::path::PathBuf;

    fn mounted_target(path: PathBuf) -> TargetConfig {
        TargetConfig {
            location: TargetLocation::MountedPath(path),
            enable_live_boot: true,
            snapshot_subvolume: None,
            live_boot_subvolume: None,
            encryption: None,
        }
    }

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
                "rd.luks.name=olduuid=oldmap".to_string(),
                "rd.luks.options=olduuid=oldopts".to_string(),
                "rw".to_string(),
                "loglevel=3".to_string(),
            ],
        };

        let options = build_entry_options(&entry_config, "UUID=new-uuid", "@", None);
        assert_eq!(
            options,
            vec![
                "quiet".to_string(),
                "loglevel=3".to_string(),
                "root=UUID=new-uuid".to_string(),
                "rw".to_string(),
                "rootflags=subvol=@/root_vol".to_string(),
            ]
        );
    }

    #[test]
    fn test_build_entry_options_adds_luks_root_options() {
        let entry_config = BootEntryConfig {
            title: "Backup".to_string(),
            kernel: PathBuf::from("/boot/vmlinuz-linux"),
            initramfs: PathBuf::from("/boot/initramfs-linux.img"),
            microcode: None,
            options: vec!["quiet".to_string()],
        };
        let luks = LuksBootConfig {
            luks_uuid: "64dd0231-0074-42b9-bd1e-914201277068".to_string(),
            mapping_name: "nest".to_string(),
        };

        let options = build_entry_options(&entry_config, "LABEL=nest", "@", Some(&luks));
        assert_eq!(
            options,
            vec![
                "quiet".to_string(),
                "rd.luks.name=64dd0231-0074-42b9-bd1e-914201277068=nest".to_string(),
                "rd.luks.options=64dd0231-0074-42b9-bd1e-914201277068=keyfile-timeout=3s,timeout=0,discard,password-echo=masked,tries=0".to_string(),
                "rootflags=x-systemd.device-timeout=0".to_string(),
                "root=LABEL=nest".to_string(),
                "rw".to_string(),
                "rootflags=subvol=@/root_vol".to_string(),
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
        let micro_idx = content.find("initrd  /amd-ucode.img").unwrap();
        let initramfs_idx = content.find("initrd  /initramfs-linux.img").unwrap();

        assert!(micro_idx < initramfs_idx);
        assert!(content.contains("linux   /vmlinuz-linux"));
        assert!(content.contains("options quiet rw"));
    }

    #[test]
    fn test_resolve_root_device_prefers_stable_target_identifier_when_unencrypted() {
        let target = TargetConfig {
            location: TargetLocation::Device("LABEL=nest".to_string()),
            enable_live_boot: true,
            snapshot_subvolume: None,
            live_boot_subvolume: None,
            encryption: None,
        };

        let root = resolve_root_device(&target, "btrfs-uuid");
        assert_eq!(root, "LABEL=nest");
    }

    #[test]
    fn test_resolve_root_device_uses_btrfs_uuid_when_encrypted() {
        let target = TargetConfig {
            location: TargetLocation::Device("LABEL=nest".to_string()),
            enable_live_boot: true,
            snapshot_subvolume: None,
            live_boot_subvolume: None,
            encryption: Some(EncryptionConfig {
                keyfile: None,
                passphrase_env: Some("BTRBAK_TEST".to_string()),
                mapping_name: "nest".to_string(),
            }),
        };

        let root = resolve_root_device(&target, "btrfs-uuid");
        assert_eq!(root, "UUID=btrfs-uuid");
    }

    #[test]
    fn test_resolve_luks_boot_config_injected_success() {
        let target = TargetConfig {
            location: TargetLocation::Device("UUID=deadbeef".to_string()),
            enable_live_boot: true,
            snapshot_subvolume: None,
            live_boot_subvolume: None,
            encryption: Some(EncryptionConfig {
                keyfile: None,
                passphrase_env: Some("BTRBAK_TEST".to_string()),
                mapping_name: "nest".to_string(),
            }),
        };

        let _runner =
            scoped_hook_command_runner(HookCommandRunner::new().with_output_hook(|cmd| {
                let program = crate::test_util::command_program(cmd);
                let args = crate::test_util::command_args(cmd);

                if program == "findfs" && args == vec!["UUID=deadbeef".to_string()] {
                    return Some(Ok(crate::test_util::mock_output(0, "/dev/loop0\n", "")));
                }
                if program == "cryptsetup"
                    && args == vec!["luksUUID".to_string(), "/dev/loop0".to_string()]
                {
                    return Some(Ok(crate::test_util::mock_output(0, "luks-uuid\n", "")));
                }
                None
            }));

        let luks = resolve_luks_boot_config(&target).unwrap().unwrap();
        assert_eq!(luks.luks_uuid, "luks-uuid");
        assert_eq!(luks.mapping_name, "nest");
    }

    #[test]
    fn test_resolve_luks_boot_config_injected_findfs_failure() {
        let target = TargetConfig {
            location: TargetLocation::Device("UUID=deadbeef".to_string()),
            enable_live_boot: true,
            snapshot_subvolume: None,
            live_boot_subvolume: None,
            encryption: Some(EncryptionConfig {
                keyfile: None,
                passphrase_env: Some("BTRBAK_TEST".to_string()),
                mapping_name: "nest".to_string(),
            }),
        };

        let _runner =
            scoped_hook_command_runner(HookCommandRunner::new().with_output_hook(|cmd| {
                if crate::test_util::command_program(cmd) == "findfs" {
                    return Some(Ok(crate::test_util::mock_output(4, "", "findfs failure\n")));
                }
                None
            }));

        let result = resolve_luks_boot_config(&target);
        assert!(result.is_err());
        assert!(
            format!("{}", result.err().unwrap())
                .contains("Failed to resolve target device identifier")
        );
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
            let target = mounted_target(td.path.clone());
            prepare_live_boot(&td.path, &config, &target, &esp_mount, "@", "@snapshots").unwrap();

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
            let target = mounted_target(td.path.clone());
            prepare_live_boot(&td.path, &config, &target, &esp_mount, "@", "@snapshots").unwrap();

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

        let target = mounted_target(mount.to_path_buf());
        let result = prepare_live_boot(mount, &config, &target, &esp, "@", "@snapshots");
        assert!(result.is_err());
        assert!(format!("{}", result.err().unwrap()).contains("Failed to install systemd-boot"));
    }
}
