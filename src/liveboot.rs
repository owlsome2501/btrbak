use crate::btrfs;
use crate::config::{BootEntryConfig, LiveBootConfig};
use crate::error::BackupError;
use crate::ui;
use std::path::Path;
use std::process::Command;

/// Prepare live boot environment on a mounted btrfs filesystem
pub fn prepare_live_boot(
    btrfs_mount: &Path,
    config: &LiveBootConfig,
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

    // Initialize bootloader if ESP path provided
    if config.esp_path.exists() {
        ui::substep("Initializing systemd-boot");
        init_systemd_boot(&config.esp_path)?;
        ui::substep("Creating boot entry");
        create_boot_entry(
            btrfs_mount,
            &config.esp_path,
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

    let output = cmd.output()?;

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

/// Create a bootloader entry for the live environment
fn create_boot_entry(
    btrfs_mount: &Path,
    esp_path: &Path,
    entry_config: &BootEntryConfig,
    live_boot_subvolume: &str,
) -> Result<(), BackupError> {
    // Determine root device and subvolume
    let root_device = find_btrfs_device(btrfs_mount)?;
    // The root filesystem is at <live_boot_subvolume>/root_vol (e.g. @/root_vol)
    let subvolume_path = format!("{}/root_vol", live_boot_subvolume);

    // Filter user options to avoid duplicating root=, rootflags=, and rw
    let mut options: Vec<String> = entry_config
        .options
        .iter()
        .filter(|opt| !opt.starts_with("root=") && !opt.starts_with("rootflags=") && *opt != "rw")
        .cloned()
        .collect();
    options.push(format!("root=UUID={}", root_device));
    options.push(format!("rootflags=subvol={}", subvolume_path));
    options.push("rw".to_string());

    // Build entry file content
    let mut entry_lines = vec![
        format!("title   {}", entry_config.title),
        format!("linux   {}", entry_config.kernel.display()),
    ];

    // Add microcode initrd before main initramfs (required for CPU microcode loading)
    if let Some(microcode) = &entry_config.microcode {
        entry_lines.push(format!("initrd  {}", microcode.display()));
    }

    entry_lines.push(format!("initrd  {}", entry_config.initramfs.display()));
    entry_lines.push(format!("options {}", options.join(" ")));

    let entry_content = format!("{}\n", entry_lines.join("\n"));

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

    let output = cmd.output()?;

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
