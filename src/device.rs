use crate::error::BackupError;
use crate::ui;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile;

/// Mount a device by identifier (UUID, LABEL, or path) to a mount point
pub fn mount_device(device: &str, mount_point: &Path) -> Result<(), BackupError> {
    // Create mount point directory if it doesn't exist
    if !mount_point.exists() {
        std::fs::create_dir_all(mount_point)?;
    }

    // Check if already mounted
    if is_mounted(mount_point)? {
        return Ok(());
    }

    let mut cmd = Command::new("mount");
    cmd.arg(device).arg(mount_point);
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Mount(format!(
            "Failed to mount {} to {}: {}",
            device,
            mount_point.display(),
            stderr
        )));
    }

    Ok(())
}

/// Unmount a mount point
pub fn unmount(mount_point: &Path) -> Result<(), BackupError> {
    let mut cmd = Command::new("umount");
    cmd.arg(mount_point);
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Mount(format!(
            "Failed to unmount {}: {}",
            mount_point.display(),
            stderr
        )));
    }

    Ok(())
}

/// Check if a path is already mounted
pub fn is_mounted(path: &Path) -> Result<bool, BackupError> {
    let mut cmd = Command::new("findmnt");
    cmd.arg("--mountpoint").arg(path).arg("--noheadings");
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = cmd.output()?;
    Ok(output.status.success())
}

/// Find the mount point of a device
pub fn find_mount_point(device: &str) -> Result<Option<PathBuf>, BackupError> {
    let mut cmd = Command::new("findmnt");
    cmd.arg("--source")
        .arg(device)
        .arg("--output")
        .arg("TARGET")
        .arg("--noheadings")
        .arg("--first-only");
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = cmd.output()?;

    if !output.status.success() {
        return Ok(None);
    }

    let target = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if target.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(target)))
    }
}

/// Check if a device is a LUKS encrypted device
pub fn is_luks_device(device: &str) -> Result<bool, BackupError> {
    let mut cmd = Command::new("cryptsetup");
    cmd.arg("isLuks").arg(device);
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = cmd.output()?;
    Ok(output.status.success())
}

/// Open a LUKS encrypted device
pub fn open_luks_device(
    device: &str,
    mapping_name: &str,
    keyfile: Option<&Path>,
    passphrase_env: Option<&str>,
) -> Result<String, BackupError> {
    if let Some(keyfile) = keyfile {
        let mut cmd = Command::new("cryptsetup");
        cmd.arg("open")
            .arg("--key-file")
            .arg(keyfile)
            .arg(device)
            .arg(mapping_name);
        ui::cmd_start(&ui::format_cmd(&cmd));

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            ui::cmd_stderr_output(&stderr);
            return Err(BackupError::Mount(format!(
                "Failed to open LUKS device {}: {}",
                device, stderr
            )));
        }
    } else if let Some(env_var) = passphrase_env {
        // Get passphrase from environment variable
        let passphrase = std::env::var(env_var).map_err(|e| {
            BackupError::Mount(format!(
                "Failed to get passphrase from environment variable {}: {}",
                env_var, e
            ))
        })?;

        ui::cmd_start(&format!("cryptsetup open --key-file - {} {}", device, mapping_name));

        // Use stdin for passphrase
        let mut child = Command::new("cryptsetup")
            .arg("open")
            .arg("--key-file")
            .arg("-")
            .arg(device)
            .arg(mapping_name)
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(passphrase.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            ui::cmd_stderr_output(&stderr);
            return Err(BackupError::Mount(format!(
                "Failed to open LUKS device {}: {}",
                device, stderr
            )));
        }
    } else {
        return Err(BackupError::Mount(
            "No keyfile or passphrase environment variable provided for LUKS device".to_string(),
        ));
    }

    Ok(format!("/dev/mapper/{}", mapping_name))
}

/// Close a LUKS mapping
pub fn close_luks_device(mapping_name: &str) -> Result<(), BackupError> {
    let mut cmd = Command::new("cryptsetup");
    cmd.arg("close").arg(mapping_name);
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Mount(format!(
            "Failed to close LUKS mapping {}: {}",
            mapping_name, stderr
        )));
    }

    Ok(())
}

/// Mount a device to a temporary directory and return the guard that unmounts on drop
pub struct MountGuard {
    mount_point: PathBuf,
    #[allow(unused)]
    device: String,
    /// LUKS mapping name if device is encrypted (None for non-encrypted)
    luks_mapping: Option<String>,
    /// Temporary directory for mount point (if created by us)
    temp_dir: Option<tempfile::TempDir>,
}

impl MountGuard {
    pub fn new(device: &str) -> Result<Self, BackupError> {
        let temp_dir = tempfile::tempdir()?;
        let mount_point = temp_dir.path().to_path_buf();

        mount_device(device, &mount_point)?;
        Ok(Self {
            mount_point,
            device: device.to_string(),
            luks_mapping: None,
            temp_dir: Some(temp_dir),
        })
    }

    /// Create a MountGuard for an encrypted device
    pub fn new_encrypted(
        device: &str,
        encryption: &crate::config::EncryptionConfig,
    ) -> Result<Self, BackupError> {
        // Check if device is LUKS
        if !is_luks_device(device)? {
            return Err(BackupError::Mount(format!(
                "Device {} is not a LUKS encrypted device",
                device
            )));
        }

        // Create temporary directory
        let temp_dir = tempfile::tempdir()?;
        let mount_point = temp_dir.path().to_path_buf();

        // Open LUKS device
        let mapped_device = open_luks_device(
            device,
            &encryption.mapping_name,
            encryption.keyfile.as_deref(),
            encryption.passphrase_env.as_deref(),
        )?;

        // Mount the mapped device
        mount_device(&mapped_device, &mount_point)?;

        Ok(Self {
            mount_point,
            device: mapped_device,
            luks_mapping: Some(encryption.mapping_name.clone()),
            temp_dir: Some(temp_dir),
        })
    }

    /// Create a MountGuard for an already mounted path (won't unmount on drop)
    pub fn for_mounted_path(path: &Path) -> Self {
        Self {
            mount_point: path.to_path_buf(),
            device: String::new(),
            luks_mapping: None,
            temp_dir: None,
        }
    }

    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        // Only unmount if we mounted it (i.e., we created a temp directory)
        if self.temp_dir.is_some()
            && let Err(e) = unmount(&self.mount_point)
        {
            ui::warning(&format!("Failed to unmount {}: {}", self.mount_point.display(), e));
        }

        // Close LUKS mapping if exists
        if let Some(mapping_name) = &self.luks_mapping
            && let Err(e) = close_luks_device(mapping_name)
        {
            ui::warning(&format!("Failed to close LUKS mapping {}: {}", mapping_name, e));
        }
    }
}
