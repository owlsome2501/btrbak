use crate::error::BackupError;
use crate::ui;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use tempfile::{self, NamedTempFile};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAccessMode {
    UserSpace,
    Privileged,
}

impl DeviceAccessMode {
    pub fn from_privileged_flag(privileged_mode: bool) -> Self {
        if privileged_mode {
            Self::Privileged
        } else {
            Self::UserSpace
        }
    }
}

#[derive(Default)]
struct UserspaceLuksState {
    mapping_to_source: HashMap<String, String>,
    source_refcount: HashMap<String, usize>,
}

fn userspace_luks_state() -> &'static Mutex<UserspaceLuksState> {
    static STATE: OnceLock<Mutex<UserspaceLuksState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(UserspaceLuksState::default()))
}

fn register_userspace_luks_mapping(mapping_name: &str, source_device: &str) {
    if let Ok(mut state) = userspace_luks_state().lock() {
        if let Some(prev_source) = state
            .mapping_to_source
            .insert(mapping_name.to_string(), source_device.to_string())
            && let Some(prev_count) = state.source_refcount.get_mut(&prev_source)
        {
            *prev_count = prev_count.saturating_sub(1);
            if *prev_count == 0 {
                state.source_refcount.remove(&prev_source);
            }
        }

        *state
            .source_refcount
            .entry(source_device.to_string())
            .or_insert(0) += 1;
    }
}

fn remove_userspace_luks_mapping(mapping_name: &str) -> Option<(String, bool)> {
    let mut state = userspace_luks_state().lock().ok()?;
    let source_device = state.mapping_to_source.remove(mapping_name)?;

    let should_lock = if let Some(count) = state.source_refcount.get_mut(&source_device) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            state.source_refcount.remove(&source_device);
            true
        } else {
            false
        }
    } else {
        true
    };

    Some((source_device, should_lock))
}

fn normalize_device_token(token: &str) -> Option<String> {
    let trimmed = token.trim().trim_end_matches('.');
    if trimmed.starts_with("/dev/") {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn parse_last_device_path(output: &str) -> Option<String> {
    let mut last = None;
    for token in output.split_whitespace() {
        if let Some(device) = normalize_device_token(token) {
            last = Some(device);
        }
    }
    last
}

fn parse_already_unlocked_mapping(stderr: &str) -> Option<String> {
    if stderr.contains("already unlocked as") {
        return parse_last_device_path(stderr);
    }
    None
}

fn parse_mount_path(output: &str) -> Option<PathBuf> {
    for line in output.lines() {
        if let Some((_, right)) = line.split_once(" at ") {
            let mount_path = right.trim().trim_end_matches('.');
            if !mount_path.is_empty() {
                return Some(PathBuf::from(mount_path));
            }
        }
    }
    None
}

fn find_executable(candidates: &[&str]) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for candidate in candidates {
            let full = dir.join(candidate);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    None
}

fn udisksctl_bin() -> Result<PathBuf, BackupError> {
    find_executable(&["udisksctl"]).ok_or_else(|| {
        BackupError::Mount(
            "User-space mode requires 'udisksctl'. Use --privileged-mode to fall back to mount/cryptsetup."
                .to_string(),
        )
    })
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

        let output = cmd.output()?;
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

/// Mount a device by identifier (UUID, LABEL, or path) to a mount point
pub fn mount_device(device: &str, mount_point: &Path) -> Result<(), BackupError> {
    mount_device_privileged(device, mount_point)
}

fn mount_device_privileged(device: &str, mount_point: &Path) -> Result<(), BackupError> {
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

fn mount_device_userspace(device: &str) -> Result<PathBuf, BackupError> {
    let resolved = resolve_block_device(device)?;
    let udisksctl = udisksctl_bin()?;

    let mut cmd = Command::new(&udisksctl);
    cmd.arg("mount")
        .arg("--block-device")
        .arg(&resolved)
        .arg("--no-user-interaction");
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Mount(format!(
            "Failed to mount {} with udisksctl: {}",
            resolved, stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(mount_point) = parse_mount_path(&stdout) {
        return Ok(mount_point);
    }

    if let Some(mount_point) = find_mount_point(&resolved)? {
        return Ok(mount_point);
    }

    Err(BackupError::Mount(format!(
        "Failed to determine mount point from udisksctl output: {}",
        stdout.trim()
    )))
}

/// Unmount a mount point
pub fn unmount(mount_point: &Path) -> Result<(), BackupError> {
    unmount_privileged(mount_point)
}

fn unmount_privileged(mount_point: &Path) -> Result<(), BackupError> {
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

fn unmount_device_userspace(device: &str) -> Result<(), BackupError> {
    let resolved = resolve_block_device(device)?;
    let udisksctl = udisksctl_bin()?;

    let mut cmd = Command::new(&udisksctl);
    cmd.arg("unmount")
        .arg("--block-device")
        .arg(&resolved)
        .arg("--no-user-interaction");
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Mount(format!(
            "Failed to unmount {} with udisksctl: {}",
            resolved, stderr
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

/// Check if a device is a LUKS encrypted device.
///
/// Default behavior uses user-space tools.
pub fn is_luks_device(device: &str) -> Result<bool, BackupError> {
    is_luks_device_with_mode(device, DeviceAccessMode::UserSpace)
}

pub fn is_luks_device_with_mode(device: &str, mode: DeviceAccessMode) -> Result<bool, BackupError> {
    match mode {
        DeviceAccessMode::UserSpace => is_luks_device_userspace(device),
        DeviceAccessMode::Privileged => is_luks_device_privileged(device),
    }
}

fn is_luks_device_privileged(device: &str) -> Result<bool, BackupError> {
    let mut cmd = Command::new("cryptsetup");
    cmd.arg("isLuks").arg(device);
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = cmd.output()?;
    Ok(output.status.success())
}

fn is_luks_device_userspace(device: &str) -> Result<bool, BackupError> {
    let resolved = resolve_block_device(device)?;
    let udisksctl = udisksctl_bin()?;

    let mut cmd = Command::new(&udisksctl);
    cmd.arg("info").arg("--block-device").arg(&resolved);
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = cmd.output()?;
    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(str::trim)
        .any(|line| line.starts_with("IdType:") && line.contains("crypto_LUKS")))
}

fn write_passphrase_keyfile(passphrase_env: &str) -> Result<NamedTempFile, BackupError> {
    let passphrase = std::env::var(passphrase_env).map_err(|e| {
        BackupError::Mount(format!(
            "Failed to get passphrase from environment variable {}: {}",
            passphrase_env, e
        ))
    })?;

    let mut temp_key = NamedTempFile::new()?;
    temp_key.write_all(passphrase.as_bytes())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = temp_key.as_file().metadata()?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(temp_key.path(), perms)?;
    }

    Ok(temp_key)
}

/// Open a LUKS encrypted device.
///
/// Default behavior uses user-space tools.
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
        DeviceAccessMode::UserSpace,
    )
}

pub fn open_luks_device_with_mode(
    device: &str,
    mapping_name: &str,
    keyfile: Option<&Path>,
    passphrase_env: Option<&str>,
    mode: DeviceAccessMode,
) -> Result<String, BackupError> {
    match mode {
        DeviceAccessMode::UserSpace => {
            open_luks_device_userspace(device, mapping_name, keyfile, passphrase_env)
        }
        DeviceAccessMode::Privileged => {
            open_luks_device_privileged(device, mapping_name, keyfile, passphrase_env)
        }
    }
}

fn open_luks_device_privileged(
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

        let mut child = Command::new("cryptsetup")
            .arg("open")
            .arg("--key-file")
            .arg("-")
            .arg(device)
            .arg(mapping_name)
            .stdin(Stdio::piped())
            .spawn()?;
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

fn open_luks_device_userspace(
    device: &str,
    mapping_name: &str,
    keyfile: Option<&Path>,
    passphrase_env: Option<&str>,
) -> Result<String, BackupError> {
    let resolved = resolve_block_device(device)?;
    let udisksctl = udisksctl_bin()?;

    let mut temp_key = None;
    let keyfile_path = if let Some(path) = keyfile {
        path.to_path_buf()
    } else if let Some(env_var) = passphrase_env {
        let file = write_passphrase_keyfile(env_var)?;
        let path = file.path().to_path_buf();
        temp_key = Some(file);
        path
    } else {
        return Err(BackupError::Mount(
            "No keyfile or passphrase environment variable provided for LUKS device".to_string(),
        ));
    };

    let mut cmd = Command::new(&udisksctl);
    cmd.arg("unlock")
        .arg("--block-device")
        .arg(&resolved)
        .arg("--key-file")
        .arg(&keyfile_path)
        .arg("--no-user-interaction");
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;
    drop(temp_key);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mapped = if output.status.success() {
        parse_last_device_path(&stdout)
    } else {
        parse_already_unlocked_mapping(&stderr)
    }
    .ok_or_else(|| {
        if !output.status.success() {
            ui::cmd_stderr_output(&stderr);
            BackupError::Mount(format!(
                "Failed to open LUKS device {} with udisksctl: {}",
                resolved, stderr
            ))
        } else {
            BackupError::Mount(format!(
                "Failed to parse unlocked mapping path from udisksctl output: {}",
                stdout.trim()
            ))
        }
    })?;

    register_userspace_luks_mapping(mapping_name, &resolved);
    Ok(mapped)
}

/// Close a LUKS mapping.
///
/// Default behavior uses user-space tools.
pub fn close_luks_device(mapping_name: &str) -> Result<(), BackupError> {
    close_luks_device_with_mode(mapping_name, DeviceAccessMode::UserSpace)
}

pub fn close_luks_device_with_mode(
    mapping_name: &str,
    mode: DeviceAccessMode,
) -> Result<(), BackupError> {
    match mode {
        DeviceAccessMode::UserSpace => close_luks_device_userspace(mapping_name),
        DeviceAccessMode::Privileged => close_luks_device_privileged(mapping_name),
    }
}

fn close_luks_device_privileged(mapping_name: &str) -> Result<(), BackupError> {
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

fn close_luks_device_userspace(mapping_name: &str) -> Result<(), BackupError> {
    let (source_device, should_lock) =
        remove_userspace_luks_mapping(mapping_name).ok_or_else(|| {
            BackupError::Mount(format!(
                "Failed to close LUKS mapping {}: mapping is not tracked in user-space mode",
                mapping_name
            ))
        })?;

    if !should_lock {
        return Ok(());
    }

    let udisksctl = udisksctl_bin()?;
    let mut cmd = Command::new(&udisksctl);
    cmd.arg("lock")
        .arg("--block-device")
        .arg(&source_device)
        .arg("--no-user-interaction");
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;
    if !output.status.success() {
        register_userspace_luks_mapping(mapping_name, &source_device);
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Mount(format!(
            "Failed to close LUKS mapping {} with udisksctl: {}",
            mapping_name, stderr
        )));
    }

    Ok(())
}

/// Mount a device and return the guard that unmounts on drop
pub struct MountGuard {
    mount_point: PathBuf,
    #[allow(unused)]
    device: String,
    /// LUKS mapping name if device is encrypted (None for non-encrypted)
    luks_mapping: Option<String>,
    /// Source LUKS device used for user-space lock/unlock
    #[allow(unused)]
    luks_source_device: Option<String>,
    /// Temporary directory for privileged mount point (if created by us)
    temp_dir: Option<tempfile::TempDir>,
    /// Block device mounted via user-space backend (for udisksctl unmount)
    managed_mount_device: Option<String>,
    mode: DeviceAccessMode,
}

impl MountGuard {
    pub fn new(device: &str) -> Result<Self, BackupError> {
        Self::new_with_mode(device, DeviceAccessMode::UserSpace)
    }

    pub fn new_with_mode(device: &str, mode: DeviceAccessMode) -> Result<Self, BackupError> {
        match mode {
            DeviceAccessMode::Privileged => {
                let temp_dir = tempfile::tempdir()?;
                let mount_point = temp_dir.path().to_path_buf();

                mount_device_privileged(device, &mount_point)?;
                Ok(Self {
                    mount_point,
                    device: device.to_string(),
                    luks_mapping: None,
                    luks_source_device: None,
                    temp_dir: Some(temp_dir),
                    managed_mount_device: None,
                    mode,
                })
            }
            DeviceAccessMode::UserSpace => {
                let resolved = resolve_block_device(device)?;
                let mount_point = mount_device_userspace(&resolved)?;
                Ok(Self {
                    mount_point,
                    device: resolved.clone(),
                    luks_mapping: None,
                    luks_source_device: None,
                    temp_dir: None,
                    managed_mount_device: Some(resolved),
                    mode,
                })
            }
        }
    }

    /// Create a MountGuard for an encrypted device
    pub fn new_encrypted(
        device: &str,
        encryption: &crate::config::EncryptionConfig,
    ) -> Result<Self, BackupError> {
        Self::new_encrypted_with_mode(device, encryption, DeviceAccessMode::UserSpace)
    }

    pub fn new_encrypted_with_mode(
        device: &str,
        encryption: &crate::config::EncryptionConfig,
        mode: DeviceAccessMode,
    ) -> Result<Self, BackupError> {
        let resolved_device = resolve_block_device(device)?;

        if !is_luks_device_with_mode(&resolved_device, mode)? {
            return Err(BackupError::Mount(format!(
                "Device {} is not a LUKS encrypted device",
                device
            )));
        }

        match mode {
            DeviceAccessMode::Privileged => {
                let temp_dir = tempfile::tempdir()?;
                let mount_point = temp_dir.path().to_path_buf();

                let mapped_device = open_luks_device_with_mode(
                    &resolved_device,
                    &encryption.mapping_name,
                    encryption.keyfile.as_deref(),
                    encryption.passphrase_env.as_deref(),
                    mode,
                )?;

                if let Err(e) = mount_device_privileged(&mapped_device, &mount_point) {
                    if let Err(close_err) =
                        close_luks_device_with_mode(&encryption.mapping_name, mode)
                    {
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
                    luks_source_device: Some(resolved_device),
                    temp_dir: Some(temp_dir),
                    managed_mount_device: None,
                    mode,
                })
            }
            DeviceAccessMode::UserSpace => {
                let mapped_device = open_luks_device_with_mode(
                    &resolved_device,
                    &encryption.mapping_name,
                    encryption.keyfile.as_deref(),
                    encryption.passphrase_env.as_deref(),
                    mode,
                )?;

                let mount_point = match mount_device_userspace(&mapped_device) {
                    Ok(mp) => mp,
                    Err(e) => {
                        if let Err(close_err) =
                            close_luks_device_with_mode(&encryption.mapping_name, mode)
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
                    mount_point,
                    device: mapped_device.clone(),
                    luks_mapping: Some(encryption.mapping_name.clone()),
                    luks_source_device: Some(resolved_device),
                    temp_dir: None,
                    managed_mount_device: Some(mapped_device),
                    mode,
                })
            }
        }
    }

    /// Create a MountGuard for an already mounted path (won't unmount on drop)
    pub fn for_mounted_path(path: &Path) -> Self {
        Self {
            mount_point: path.to_path_buf(),
            device: String::new(),
            luks_mapping: None,
            luks_source_device: None,
            temp_dir: None,
            managed_mount_device: None,
            mode: DeviceAccessMode::UserSpace,
        }
    }

    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        if let Some(block_device) = &self.managed_mount_device {
            if let Err(e) = unmount_device_userspace(block_device) {
                ui::warning(&format!(
                    "Failed to unmount {}: {}",
                    self.mount_point.display(),
                    e
                ));
            }
        } else if self.temp_dir.is_some()
            && let Err(e) = unmount(&self.mount_point)
        {
            ui::warning(&format!(
                "Failed to unmount {}: {}",
                self.mount_point.display(),
                e
            ));
        }

        if let Some(mapping_name) = &self.luks_mapping
            && let Err(e) = close_luks_device_with_mode(mapping_name, self.mode)
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
    fn test_device_access_mode_from_flag() {
        assert_eq!(
            DeviceAccessMode::from_privileged_flag(false),
            DeviceAccessMode::UserSpace
        );
        assert_eq!(
            DeviceAccessMode::from_privileged_flag(true),
            DeviceAccessMode::Privileged
        );
    }

    #[test]
    fn test_parse_mount_path_with_period() {
        let output = "Mounted /dev/loop0 at /run/media/user/abc.";
        assert_eq!(
            parse_mount_path(output),
            Some(PathBuf::from("/run/media/user/abc"))
        );
    }

    #[test]
    fn test_parse_mount_path_without_period() {
        let output = "Mounted /dev/loop0 at /run/media/user/abc";
        assert_eq!(
            parse_mount_path(output),
            Some(PathBuf::from("/run/media/user/abc"))
        );
    }

    #[test]
    fn test_parse_device_paths() {
        let output = "Unlocked /dev/loop7 as /dev/dm-1.";
        assert_eq!(parse_last_device_path(output).as_deref(), Some("/dev/dm-1"));
    }

    #[test]
    fn test_parse_already_unlocked_mapping() {
        let stderr =
            "Error unlocking /dev/loop9: Device /dev/loop9 is already unlocked as /dev/dm-1";
        assert_eq!(
            parse_already_unlocked_mapping(stderr).as_deref(),
            Some("/dev/dm-1")
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
    fn test_is_mounted_true() {
        assert!(is_mounted(Path::new("/")).unwrap());
    }

    #[test]
    fn test_is_mounted_false() {
        assert!(!is_mounted(Path::new("/nonexistent_mount_point_btrbak_test")).unwrap());
    }

    #[test]
    fn test_find_mount_point_none() {
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

        assert!(mapped.starts_with("/dev/"));
        assert!(Path::new(&mapped).exists());

        close_luks_device(&enc.mapping_name).expect("close should succeed");
        assert!(!Path::new(&mapped).exists());
    }

    #[test]
    fn test_open_luks_passphrase_env() {
        let dev = require_luks_test_device!("open_pass_env");

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

        assert!(mapped.starts_with("/dev/"));
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
        assert!(!mp.exists());
        assert!(close_luks_device(&enc.mapping_name).is_err());
    }

    #[test]
    fn test_mount_guard_encrypted_not_luks() {
        let _dev = require_luks_test_device!("enc_not_luks");
        let enc = _dev.encryption_config();
        let result = MountGuard::new_encrypted("/dev/null", &enc);
        assert!(result.is_err());
    }
}
