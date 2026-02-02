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

        ui::cmd_start(&format!(
            "cryptsetup open --key-file - {} {}",
            device, mapping_name
        ));

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

        // Mount the mapped device; if this fails, close the LUKS mapping
        // before propagating the error.
        if let Err(e) = mount_device(&mapped_device, &mount_point) {
            if let Err(close_err) = close_luks_device(&encryption.mapping_name) {
                ui::warning(&format!(
                    "Failed to close LUKS mapping {} after mount failure: {}",
                    encryption.mapping_name, close_err
                ));
            }
            return Err(e);
        }

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
            ui::warning(&format!(
                "Failed to unmount {}: {}",
                self.mount_point.display(),
                e
            ));
        }

        // Close LUKS mapping if exists
        if let Some(mapping_name) = &self.luks_mapping
            && let Err(e) = close_luks_device(mapping_name)
        {
            ui::warning(&format!(
                "Failed to close LUKS mapping {}: {}",
                mapping_name, e
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ── No-privilege tests (always run) ─────────────────────────────────

    #[test]
    fn test_mount_guard_for_mounted_path() {
        let path = Path::new("/tmp");
        let guard = MountGuard::for_mounted_path(path);
        assert_eq!(guard.mount_point(), path);
        // Drop is a no-op – should not panic.
        drop(guard);
    }

    #[test]
    fn test_mount_guard_mount_point_accessor() {
        let path = Path::new("/some/path");
        let guard = MountGuard::for_mounted_path(path);
        assert_eq!(guard.mount_point(), Path::new("/some/path"));
    }

    #[test]
    fn test_is_mounted_true() {
        // The root filesystem is always mounted.
        assert!(is_mounted(Path::new("/")).unwrap());
    }

    #[test]
    fn test_is_mounted_false() {
        assert!(!is_mounted(Path::new("/nonexistent_mount_point_btrbak_test")).unwrap());
    }

    #[test]
    fn test_find_mount_point_none() {
        // /dev/null is never a mounted filesystem source.
        let result = find_mount_point("/dev/null").unwrap();
        assert!(result.is_none());
    }

    // ── LUKS tests (require integration env) ────────────────────────────

    use crate::test_util::require_luks_test_device;

    #[test]
    fn test_is_luks_device_positive() {
        let dev = require_luks_test_device!("is_luks_pos");
        assert!(is_luks_device(&dev.loop_device).unwrap());
    }

    #[test]
    fn test_is_luks_device_negative() {
        let _dev = require_luks_test_device!("is_luks_neg");
        assert!(!is_luks_device("/dev/null").unwrap());
    }

    #[test]
    fn test_open_close_luks_keyfile() {
        let dev = require_luks_test_device!("open_close_kf");
        let enc = dev.encryption_config();

        let mapped = open_luks_device(
            &dev.loop_device,
            &enc.mapping_name,
            enc.keyfile.as_deref(),
            enc.passphrase_env.as_deref(),
        )
        .expect("open_luks_device with keyfile should succeed");

        assert_eq!(mapped, dev.mapped_device_path());
        assert!(Path::new(&mapped).exists());

        close_luks_device(&enc.mapping_name).expect("close should succeed");
        assert!(!Path::new(&mapped).exists());
    }

    #[test]
    fn test_open_luks_passphrase_env() {
        let dev = require_luks_test_device!("open_pass_env");

        // The integration script exports BTRBAK_TEST_LUKS_PASSPHRASE.
        if std::env::var("BTRBAK_TEST_LUKS_PASSPHRASE").is_err() {
            eprintln!("Skipped: BTRBAK_TEST_LUKS_PASSPHRASE not set");
            return;
        }

        let enc = dev.encryption_config_passphrase();
        let mapped = open_luks_device(
            &dev.loop_device,
            &enc.mapping_name,
            enc.keyfile.as_deref(),
            enc.passphrase_env.as_deref(),
        )
        .expect("open_luks_device with passphrase env should succeed");

        assert_eq!(mapped, dev.mapped_device_path());
        close_luks_device(&enc.mapping_name).expect("close should succeed");
    }

    #[test]
    fn test_open_luks_no_credentials() {
        let dev = require_luks_test_device!("no_creds");
        let result = open_luks_device(&dev.loop_device, &dev.mapping_name, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_open_luks_wrong_keyfile() {
        let dev = require_luks_test_device!("wrong_kf");
        // Use /dev/null as a wrong keyfile (empty).
        let result = open_luks_device(
            &dev.loop_device,
            &dev.mapping_name,
            Some(Path::new("/dev/null")),
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_close_luks_not_open() {
        let _dev = require_luks_test_device!("close_not_open");
        let result = close_luks_device("btrbak_test_nonexistent_mapping");
        assert!(result.is_err());
    }

    #[test]
    fn test_mount_guard_encrypted_lifecycle() {
        let dev = require_luks_test_device!("enc_lifecycle");
        let enc = dev.encryption_config();

        let guard = MountGuard::new_encrypted(&dev.loop_device, &enc)
            .expect("new_encrypted should succeed");

        let mp = guard.mount_point().to_path_buf();
        assert!(mp.exists());

        drop(guard);
        // After drop, the temp dir is removed and the mapping closed.
        assert!(!mp.exists());
        assert!(!Path::new(&dev.mapped_device_path()).exists());
    }

    #[test]
    fn test_mount_guard_encrypted_not_luks() {
        let _dev = require_luks_test_device!("enc_not_luks");
        let enc = _dev.encryption_config();
        let result = MountGuard::new_encrypted("/dev/null", &enc);
        assert!(result.is_err());
    }
}
