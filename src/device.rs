use crate::command_runner;
use crate::error::BackupError;
use crate::ui;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAccessMode {
    Privileged,
}

impl DeviceAccessMode {
    pub fn from_privileged_flag(_privileged_mode: bool) -> Self {
        Self::Privileged
    }
}

struct DevicePathResolver;

impl DevicePathResolver {
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
                ui::cmd_stderr_output(&stderr);
                return Err(BackupError::Mount(format!(
                    "Failed to resolve device identifier {}: {}",
                    device, stderr
                )));
            }

            let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if resolved.starts_with("/dev/") {
                return Ok(resolved);
            }

            return Err(BackupError::Mount(format!(
                "Resolved device identifier {} to invalid block device path: {}",
                device, resolved
            )));
        }

        Ok(device.to_string())
    }

    fn is_mounted(path: &Path) -> Result<bool, BackupError> {
        let mut cmd = Command::new("findmnt");
        cmd.arg("--mountpoint").arg(path).arg("--noheadings");
        ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

        let output = command_runner::output(&mut cmd)?;
        Ok(output.status.success())
    }
}

fn mount_device_privileged_impl(device: &str, mount_point: &Path) -> Result<(), BackupError> {
    if !mount_point.exists() {
        std::fs::create_dir_all(mount_point)?;
    }

    if DevicePathResolver::is_mounted(mount_point)? {
        return Ok(());
    }

    let mut cmd = Command::new("mount");
    cmd.arg(device).arg(mount_point);
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = command_runner::output(&mut cmd)?;

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

fn unmount_privileged_impl(mount_point: &Path) -> Result<(), BackupError> {
    let mut cmd = Command::new("umount");
    cmd.arg(mount_point);
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = command_runner::output(&mut cmd)?;

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

fn is_luks_device_privileged_impl(device: &str) -> Result<bool, BackupError> {
    let mut cmd = Command::new("cryptsetup");
    cmd.arg("isLuks").arg(device);
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = command_runner::output(&mut cmd)?;
    Ok(output.status.success())
}

/// Check if a device is a LUKS encrypted device.
///
/// Uses privileged system tools (`cryptsetup`).
pub fn is_luks_device(device: &str) -> Result<bool, BackupError> {
    is_luks_device_with_mode(device, DeviceAccessMode::Privileged)
}

pub fn is_luks_device_with_mode(
    device: &str,
    _mode: DeviceAccessMode,
) -> Result<bool, BackupError> {
    let resolved = DevicePathResolver::resolve_block_device(device)?;
    is_luks_device_privileged_impl(&resolved)
}

fn open_luks_device_privileged_impl(
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

        let output = command_runner::output(&mut cmd)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            ui::cmd_stderr_output(&stderr);
            return Err(BackupError::Mount(format!(
                "Failed to open LUKS device {}: {}",
                device, stderr
            )));
        }
    } else if let Some(env_var) = passphrase_env {
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

        let mut cmd = Command::new("cryptsetup");
        cmd.arg("open")
            .arg("--key-file")
            .arg("-")
            .arg(device)
            .arg(mapping_name)
            .stdin(Stdio::piped());

        let mut child = command_runner::spawn(&mut cmd)?;
        if let Some(mut stdin) = child.stdin.take() {
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

/// Open a LUKS encrypted device.
///
/// Uses privileged system tools (`cryptsetup`).
pub fn open_luks_device(
    device: &str,
    mapping_name: &str,
    keyfile: Option<&Path>,
    passphrase_env: Option<&str>,
) -> Result<String, BackupError> {
    open_luks_device_with_mode(
        device,
        mapping_name,
        keyfile,
        passphrase_env,
        DeviceAccessMode::Privileged,
    )
}

pub fn open_luks_device_with_mode(
    device: &str,
    mapping_name: &str,
    keyfile: Option<&Path>,
    passphrase_env: Option<&str>,
    _mode: DeviceAccessMode,
) -> Result<String, BackupError> {
    let resolved = DevicePathResolver::resolve_block_device(device)?;
    open_luks_device_privileged_impl(&resolved, mapping_name, keyfile, passphrase_env)
}

fn close_luks_device_privileged_impl(mapping_name: &str) -> Result<(), BackupError> {
    let mut cmd = Command::new("cryptsetup");
    cmd.arg("close").arg(mapping_name);
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = command_runner::output(&mut cmd)?;

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

/// Close a LUKS mapping.
///
/// Uses privileged system tools (`cryptsetup`).
pub fn close_luks_device(mapping_name: &str) -> Result<(), BackupError> {
    close_luks_device_with_mode(mapping_name, DeviceAccessMode::Privileged)
}

pub fn close_luks_device_with_mode(
    mapping_name: &str,
    _mode: DeviceAccessMode,
) -> Result<(), BackupError> {
    close_luks_device_privileged_impl(mapping_name)
}

struct ManagedMount {
    mount_point: PathBuf,
    // Keep tempdir alive while mounted.
    _temp_dir: tempfile::TempDir,
}

fn mount_managed(device: &str) -> Result<ManagedMount, BackupError> {
    let resolved = DevicePathResolver::resolve_block_device(device)?;
    let temp_dir = tempfile::tempdir()?;
    let mount_point = temp_dir.path().to_path_buf();

    mount_device_privileged_impl(&resolved, &mount_point)?;

    Ok(ManagedMount {
        mount_point,
        _temp_dir: temp_dir,
    })
}

/// Mount a device and return the guard that unmounts on drop.
pub struct MountGuard {
    mount_point: PathBuf,
    /// LUKS mapping name if device is encrypted (None for non-encrypted).
    luks_mapping: Option<String>,
    managed_mount: Option<ManagedMount>,
}

impl MountGuard {
    pub fn new(device: &str) -> Result<Self, BackupError> {
        Self::new_with_mode(device, DeviceAccessMode::Privileged)
    }

    pub fn new_with_mode(device: &str, _mode: DeviceAccessMode) -> Result<Self, BackupError> {
        let managed_mount = mount_managed(device)?;

        Ok(Self {
            mount_point: managed_mount.mount_point.clone(),
            luks_mapping: None,
            managed_mount: Some(managed_mount),
        })
    }

    /// Create a MountGuard for an encrypted device.
    pub fn new_encrypted(
        device: &str,
        encryption: &crate::config::EncryptionConfig,
    ) -> Result<Self, BackupError> {
        Self::new_encrypted_with_mode(device, encryption, DeviceAccessMode::Privileged)
    }

    pub fn new_encrypted_with_mode(
        device: &str,
        encryption: &crate::config::EncryptionConfig,
        _mode: DeviceAccessMode,
    ) -> Result<Self, BackupError> {
        let resolved_device = DevicePathResolver::resolve_block_device(device)?;

        if !is_luks_device_privileged_impl(&resolved_device)? {
            return Err(BackupError::Mount(format!(
                "Device {} is not a LUKS encrypted device",
                device
            )));
        }

        let mapped_device = open_luks_device_privileged_impl(
            &resolved_device,
            &encryption.mapping_name,
            encryption.keyfile.as_deref(),
            encryption.passphrase_env.as_deref(),
        )?;

        let managed_mount = match mount_managed(&mapped_device) {
            Ok(mount) => mount,
            Err(e) => {
                if let Err(close_err) = close_luks_device_privileged_impl(&encryption.mapping_name)
                {
                    ui::warning(&format!(
                        "Failed to close LUKS mapping {} after mount failure: {}",
                        encryption.mapping_name, close_err
                    ));
                }
                return Err(e);
            }
        };

        Ok(Self {
            mount_point: managed_mount.mount_point.clone(),
            luks_mapping: Some(encryption.mapping_name.clone()),
            managed_mount: Some(managed_mount),
        })
    }

    /// Create a MountGuard for an already mounted path (won't unmount on drop).
    pub fn for_mounted_path(path: &Path) -> Self {
        Self {
            mount_point: path.to_path_buf(),
            luks_mapping: None,
            managed_mount: None,
        }
    }

    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        if let Some(managed_mount) = &self.managed_mount
            && let Err(e) = unmount_privileged_impl(&managed_mount.mount_point)
        {
            ui::warning(&format!(
                "Failed to unmount {}: {}",
                self.mount_point.display(),
                e
            ));
        }

        if let Some(mapping_name) = &self.luks_mapping
            && let Err(e) = close_luks_device_privileged_impl(mapping_name)
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

    // ── No-env/no-root tests ────────────────────────────────────────────

    #[test]
    fn test_device_access_mode_from_flag_always_privileged() {
        assert_eq!(
            DeviceAccessMode::from_privileged_flag(false),
            DeviceAccessMode::Privileged
        );
        assert_eq!(
            DeviceAccessMode::from_privileged_flag(true),
            DeviceAccessMode::Privileged
        );
    }

    #[test]
    fn test_mount_guard_for_mounted_path() {
        let path = Path::new("/tmp");
        let guard = MountGuard::for_mounted_path(path);
        assert_eq!(guard.mount_point(), path);
        drop(guard);
    }

    #[test]
    fn test_mount_guard_mount_point_accessor() {
        let path = Path::new("/some/path");
        let guard = MountGuard::for_mounted_path(path);
        assert_eq!(guard.mount_point(), Path::new("/some/path"));
    }

    #[test]
    fn test_resolve_block_device_injected_findfs_failure() {
        let _runner = crate::test_util::scoped_hook_command_runner(
            crate::test_util::HookCommandRunner::new().with_output_hook(|cmd| {
                if crate::test_util::command_program(cmd) == "findfs" {
                    return Some(Ok(crate::test_util::mock_output(
                        3,
                        "",
                        "injected findfs failure\n",
                    )));
                }
                None
            }),
        );

        let result = DevicePathResolver::resolve_block_device("UUID=deadbeef");
        assert!(result.is_err());
        assert!(
            format!("{}", result.err().unwrap()).contains("Failed to resolve device identifier")
        );
    }

    #[test]
    fn test_resolve_block_device_injected_findfs_invalid_output() {
        let _runner = crate::test_util::scoped_hook_command_runner(
            crate::test_util::HookCommandRunner::new().with_output_hook(|cmd| {
                if crate::test_util::command_program(cmd) == "findfs"
                    && crate::test_util::command_args(cmd) == vec!["UUID=deadbeef".to_string()]
                {
                    return Some(Ok(crate::test_util::mock_output(
                        0,
                        "/tmp/not-a-device\n",
                        "",
                    )));
                }
                None
            }),
        );

        let result = DevicePathResolver::resolve_block_device("UUID=deadbeef");
        assert!(result.is_err());
        assert!(
            format!("{}", result.err().unwrap())
                .contains("Resolved device identifier UUID=deadbeef to invalid block device path")
        );
    }

    // ── Root-required mount/LUKS tests ─────────────────────────────────

    mod root_required_tests {
        use super::*;
        use crate::test_util::require_luks_test_device;
        use std::path::Path;
        use std::process::Command;

        fn ensure_root_for_root_required_tests() -> bool {
            let output = match Command::new("id").arg("-u").output() {
                Ok(output) => output,
                Err(_) => {
                    eprintln!("Skipped: failed to determine current uid");
                    return false;
                }
            };

            if !output.status.success() {
                eprintln!("Skipped: failed to determine current uid");
                return false;
            }

            if String::from_utf8_lossy(&output.stdout).trim() == "0" {
                true
            } else {
                eprintln!("Skipped: root_required_tests require root");
                false
            }
        }

        #[test]
        fn test_root_is_luks_device_positive() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let dev = require_luks_test_device!("is_luks_pos");
            assert!(is_luks_device(&dev.loop_device).unwrap());
            assert!(
                is_luks_device_with_mode(&dev.loop_device, DeviceAccessMode::Privileged).unwrap()
            );
        }

        #[test]
        fn test_root_is_luks_device_negative() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let _dev = require_luks_test_device!("is_luks_neg");
            assert!(!is_luks_device("/dev/null").unwrap());
        }

        #[test]
        fn test_root_open_close_luks_keyfile() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let dev = require_luks_test_device!("open_close_kf");
            let enc = dev.encryption_config();

            let mapped = open_luks_device(
                &dev.loop_device,
                &enc.mapping_name,
                enc.keyfile.as_deref(),
                enc.passphrase_env.as_deref(),
            )
            .expect("open_luks_device with keyfile should succeed");

            assert!(mapped.starts_with("/dev/mapper/"));
            assert!(Path::new(&mapped).exists());

            close_luks_device(&enc.mapping_name).expect("close should succeed");
            assert!(!Path::new(&mapped).exists());

            let mapped_with_mode = open_luks_device_with_mode(
                &dev.loop_device,
                &enc.mapping_name,
                enc.keyfile.as_deref(),
                enc.passphrase_env.as_deref(),
                DeviceAccessMode::Privileged,
            )
            .expect("open_luks_device_with_mode should succeed");
            assert!(Path::new(&mapped_with_mode).exists());
            close_luks_device_with_mode(&enc.mapping_name, DeviceAccessMode::Privileged)
                .expect("close_luks_device_with_mode should succeed");
        }

        #[test]
        fn test_root_open_luks_passphrase_env() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let dev = require_luks_test_device!("open_pass_env");

            if std::env::var("BTRBAK_TEST_LUKS_PASSPHRASE").is_err() {
                crate::test_util::skip_or_fail_test("Skipped: BTRBAK_TEST_LUKS_PASSPHRASE not set");
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

            assert!(mapped.starts_with("/dev/mapper/"));
            close_luks_device(&enc.mapping_name).expect("close should succeed");
        }

        #[test]
        fn test_root_open_luks_no_credentials() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let dev = require_luks_test_device!("no_creds");
            let result = open_luks_device(&dev.loop_device, &dev.mapping_name, None, None);
            assert!(result.is_err());
        }

        #[test]
        fn test_root_open_luks_wrong_keyfile() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let dev = require_luks_test_device!("wrong_kf");
            let result = open_luks_device(
                &dev.loop_device,
                &dev.mapping_name,
                Some(Path::new("/dev/null")),
                None,
            );
            assert!(result.is_err());
        }

        #[test]
        fn test_root_close_luks_not_open() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let _dev = require_luks_test_device!("close_not_open");
            let result = close_luks_device("btrbak_test_nonexistent_mapping");
            assert!(result.is_err());
        }

        #[test]
        fn test_root_mount_guard_encrypted_lifecycle() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let dev = require_luks_test_device!("enc_lifecycle");
            let enc = dev.encryption_config();

            let guard = MountGuard::new_encrypted(&dev.loop_device, &enc)
                .expect("new_encrypted should succeed");

            let mp = guard.mount_point().to_path_buf();
            assert!(mp.exists());

            drop(guard);
            assert!(!mp.exists());
            assert!(close_luks_device(&enc.mapping_name).is_err());
        }

        #[test]
        fn test_root_mount_guard_encrypted_lifecycle_with_mode() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let dev = require_luks_test_device!("enc_lifecycle_mode");
            let enc = dev.encryption_config();

            let guard = MountGuard::new_encrypted_with_mode(
                &dev.loop_device,
                &enc,
                DeviceAccessMode::Privileged,
            )
            .expect("new_encrypted_with_mode should succeed");

            let mp = guard.mount_point().to_path_buf();
            assert!(mp.exists());

            drop(guard);
            assert!(!mp.exists());
            assert!(close_luks_device(&enc.mapping_name).is_err());
        }

        #[test]
        fn test_root_mount_guard_encrypted_not_luks() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let _dev = require_luks_test_device!("enc_not_luks");
            let enc = _dev.encryption_config();
            let result = MountGuard::new_encrypted("/dev/null", &enc);
            assert!(result.is_err());
        }
    }
}
