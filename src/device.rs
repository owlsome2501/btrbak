use crate::command_runner;
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

trait DeviceBackend {
    fn mount_managed(&self, device: &str) -> Result<ManagedMount, BackupError>;

    fn unmount_managed(&self, mount: &ManagedMount) -> Result<(), BackupError>;

    fn is_luks_device(&self, device: &str) -> Result<bool, BackupError>;

    fn open_luks_device(
        &self,
        device: &str,
        mapping_name: &str,
        keyfile: Option<&Path>,
        passphrase_env: Option<&str>,
    ) -> Result<String, BackupError>;

    fn close_luks_device(&self, mapping_name: &str) -> Result<(), BackupError>;
}

struct UserSpaceBackend;
struct PrivilegedBackend;

struct ManagedMount {
    mount_point: PathBuf,
    // Keep tempdir alive while mounted in privileged mode.
    temp_dir: Option<tempfile::TempDir>,
    // Device identifier required by the backend for unmount.
    managed_device: Option<String>,
}

impl DeviceBackend for UserSpaceBackend {
    fn mount_managed(&self, device: &str) -> Result<ManagedMount, BackupError> {
        let resolved = DevicePathResolver::resolve_block_device(device)?;
        let mount_point = mount_device_userspace_impl(&resolved)?;
        Ok(ManagedMount {
            mount_point,
            temp_dir: None,
            managed_device: Some(resolved),
        })
    }

    fn unmount_managed(&self, mount: &ManagedMount) -> Result<(), BackupError> {
        let managed_device = mount.managed_device.as_deref().ok_or_else(|| {
            BackupError::Mount("Missing managed device context for user-space unmount".to_string())
        })?;
        unmount_device_userspace_impl(managed_device)
    }

    fn is_luks_device(&self, device: &str) -> Result<bool, BackupError> {
        is_luks_device_userspace_impl(device)
    }

    fn open_luks_device(
        &self,
        device: &str,
        mapping_name: &str,
        keyfile: Option<&Path>,
        passphrase_env: Option<&str>,
    ) -> Result<String, BackupError> {
        open_luks_device_userspace_impl(device, mapping_name, keyfile, passphrase_env)
    }

    fn close_luks_device(&self, mapping_name: &str) -> Result<(), BackupError> {
        close_luks_device_userspace_impl(mapping_name)
    }
}

impl DeviceBackend for PrivilegedBackend {
    fn mount_managed(&self, device: &str) -> Result<ManagedMount, BackupError> {
        let temp_dir = tempfile::tempdir()?;
        let mount_point = temp_dir.path().to_path_buf();

        mount_device_privileged_impl(device, &mount_point)?;
        Ok(ManagedMount {
            mount_point,
            temp_dir: Some(temp_dir),
            managed_device: None,
        })
    }

    fn unmount_managed(&self, mount: &ManagedMount) -> Result<(), BackupError> {
        if mount.temp_dir.is_some() {
            unmount_privileged_impl(&mount.mount_point)
        } else {
            Ok(())
        }
    }

    fn is_luks_device(&self, device: &str) -> Result<bool, BackupError> {
        is_luks_device_privileged_impl(device)
    }

    fn open_luks_device(
        &self,
        device: &str,
        mapping_name: &str,
        keyfile: Option<&Path>,
        passphrase_env: Option<&str>,
    ) -> Result<String, BackupError> {
        open_luks_device_privileged_impl(device, mapping_name, keyfile, passphrase_env)
    }

    fn close_luks_device(&self, mapping_name: &str) -> Result<(), BackupError> {
        close_luks_device_privileged_impl(mapping_name)
    }
}

fn backend_for_mode(mode: DeviceAccessMode) -> &'static dyn DeviceBackend {
    static USERSPACE_BACKEND: UserSpaceBackend = UserSpaceBackend;
    static PRIVILEGED_BACKEND: PrivilegedBackend = PrivilegedBackend;

    match mode {
        DeviceAccessMode::UserSpace => &USERSPACE_BACKEND,
        DeviceAccessMode::Privileged => &PRIVILEGED_BACKEND,
    }
}

#[derive(Default)]
struct UserspaceLuksState {
    mapping_to_source: HashMap<String, String>,
    source_refcount: HashMap<String, usize>,
}

struct UserspaceLuksRegistry;

impl UserspaceLuksRegistry {
    fn state() -> &'static Mutex<UserspaceLuksState> {
        static STATE: OnceLock<Mutex<UserspaceLuksState>> = OnceLock::new();
        STATE.get_or_init(|| Mutex::new(UserspaceLuksState::default()))
    }

    fn register(mapping_name: &str, source_device: &str) {
        if let Ok(mut state) = Self::state().lock() {
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

    fn remove(mapping_name: &str) -> Option<(String, bool)> {
        let mut state = Self::state().lock().ok()?;
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
}

struct UdisksOutputParser;

impl UdisksOutputParser {
    fn normalize_device_token(token: &str) -> Option<String> {
        let trimmed = token.trim().trim_end_matches('.');
        if trimmed.starts_with("/dev/") {
            Some(trimmed.to_string())
        } else {
            None
        }
    }

    fn last_device_path(output: &str) -> Option<String> {
        let mut last = None;
        for token in output.split_whitespace() {
            if let Some(device) = Self::normalize_device_token(token) {
                last = Some(device);
            }
        }
        last
    }

    fn already_unlocked_mapping(stderr: &str) -> Option<String> {
        if stderr.contains("already unlocked as") {
            return Self::last_device_path(stderr);
        }
        None
    }

    fn mount_path(output: &str) -> Option<PathBuf> {
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
}

struct DevicePathResolver;

impl DevicePathResolver {
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
        Self::find_executable(&["udisksctl"]).ok_or_else(|| {
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

    fn find_mount_point(device: &str) -> Result<Option<PathBuf>, BackupError> {
        let mut cmd = Command::new("findmnt");
        cmd.arg("--source")
            .arg(device)
            .arg("--output")
            .arg("TARGET")
            .arg("--noheadings")
            .arg("--first-only");
        ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

        let output = command_runner::output(&mut cmd)?;

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

fn mount_device_userspace_impl(device: &str) -> Result<PathBuf, BackupError> {
    let resolved = DevicePathResolver::resolve_block_device(device)?;
    let udisksctl = DevicePathResolver::udisksctl_bin()?;

    let mut cmd = Command::new(&udisksctl);
    cmd.arg("mount")
        .arg("--block-device")
        .arg(&resolved)
        .arg("--no-user-interaction");
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = command_runner::output(&mut cmd)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Mount(format!(
            "Failed to mount {} with udisksctl: {}",
            resolved, stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(mount_point) = UdisksOutputParser::mount_path(&stdout) {
        return Ok(mount_point);
    }

    if let Some(mount_point) = DevicePathResolver::find_mount_point(&resolved)? {
        return Ok(mount_point);
    }

    Err(BackupError::Mount(format!(
        "Failed to determine mount point from udisksctl output: {}",
        stdout.trim()
    )))
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

fn unmount_device_userspace_impl(device: &str) -> Result<(), BackupError> {
    let resolved = DevicePathResolver::resolve_block_device(device)?;
    let udisksctl = DevicePathResolver::udisksctl_bin()?;

    let mut cmd = Command::new(&udisksctl);
    cmd.arg("unmount")
        .arg("--block-device")
        .arg(&resolved)
        .arg("--no-user-interaction");
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = command_runner::output(&mut cmd)?;

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

/// Check if a device is a LUKS encrypted device.
///
/// Default behavior uses user-space tools.
pub fn is_luks_device(device: &str) -> Result<bool, BackupError> {
    is_luks_device_with_mode(device, DeviceAccessMode::UserSpace)
}

pub fn is_luks_device_with_mode(device: &str, mode: DeviceAccessMode) -> Result<bool, BackupError> {
    backend_for_mode(mode).is_luks_device(device)
}

fn is_luks_device_privileged_impl(device: &str) -> Result<bool, BackupError> {
    let mut cmd = Command::new("cryptsetup");
    cmd.arg("isLuks").arg(device);
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = command_runner::output(&mut cmd)?;
    Ok(output.status.success())
}

fn is_luks_device_userspace_impl(device: &str) -> Result<bool, BackupError> {
    let resolved = DevicePathResolver::resolve_block_device(device)?;
    let udisksctl = DevicePathResolver::udisksctl_bin()?;

    let mut cmd = Command::new(&udisksctl);
    cmd.arg("info").arg("--block-device").arg(&resolved);
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = command_runner::output(&mut cmd)?;
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
    backend_for_mode(mode).open_luks_device(device, mapping_name, keyfile, passphrase_env)
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

fn open_luks_device_userspace_impl(
    device: &str,
    mapping_name: &str,
    keyfile: Option<&Path>,
    passphrase_env: Option<&str>,
) -> Result<String, BackupError> {
    let resolved = DevicePathResolver::resolve_block_device(device)?;
    let udisksctl = DevicePathResolver::udisksctl_bin()?;

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

    let output = command_runner::output(&mut cmd)?;
    drop(temp_key);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mapped = if output.status.success() {
        UdisksOutputParser::last_device_path(&stdout)
    } else {
        UdisksOutputParser::already_unlocked_mapping(&stderr)
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

    UserspaceLuksRegistry::register(mapping_name, &resolved);
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
    backend_for_mode(mode).close_luks_device(mapping_name)
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

fn close_luks_device_userspace_impl(mapping_name: &str) -> Result<(), BackupError> {
    let (source_device, should_lock) =
        UserspaceLuksRegistry::remove(mapping_name).ok_or_else(|| {
            BackupError::Mount(format!(
                "Failed to close LUKS mapping {}: mapping is not tracked in user-space mode",
                mapping_name
            ))
        })?;

    if !should_lock {
        return Ok(());
    }

    let udisksctl = DevicePathResolver::udisksctl_bin()?;
    let mut cmd = Command::new(&udisksctl);
    cmd.arg("lock")
        .arg("--block-device")
        .arg(&source_device)
        .arg("--no-user-interaction");
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = command_runner::output(&mut cmd)?;
    if !output.status.success() {
        UserspaceLuksRegistry::register(mapping_name, &source_device);
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Mount(format!(
            "Failed to close LUKS mapping {} with udisksctl: {}",
            mapping_name, stderr
        )));
    }

    Ok(())
}

/// Mount a device and return the guard that unmounts on drop.
pub struct MountGuard {
    mount_point: PathBuf,
    /// LUKS mapping name if device is encrypted (None for non-encrypted).
    luks_mapping: Option<String>,
    managed_mount: Option<ManagedMount>,
    mode: DeviceAccessMode,
}

impl MountGuard {
    pub fn new(device: &str) -> Result<Self, BackupError> {
        Self::new_with_mode(device, DeviceAccessMode::UserSpace)
    }

    pub fn new_with_mode(device: &str, mode: DeviceAccessMode) -> Result<Self, BackupError> {
        let managed_mount = backend_for_mode(mode).mount_managed(device)?;
        Ok(Self {
            mount_point: managed_mount.mount_point.clone(),
            luks_mapping: None,
            managed_mount: Some(managed_mount),
            mode,
        })
    }

    /// Create a MountGuard for an encrypted device.
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
        let backend = backend_for_mode(mode);
        let resolved_device = DevicePathResolver::resolve_block_device(device)?;

        if !backend.is_luks_device(&resolved_device)? {
            return Err(BackupError::Mount(format!(
                "Device {} is not a LUKS encrypted device",
                device
            )));
        }

        let mapped_device = backend.open_luks_device(
            &resolved_device,
            &encryption.mapping_name,
            encryption.keyfile.as_deref(),
            encryption.passphrase_env.as_deref(),
        )?;

        let managed_mount = match backend.mount_managed(&mapped_device) {
            Ok(mount) => mount,
            Err(e) => {
                if let Err(close_err) = backend.close_luks_device(&encryption.mapping_name) {
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
            mode,
        })
    }

    /// Create a MountGuard for an already mounted path (won't unmount on drop).
    pub fn for_mounted_path(path: &Path) -> Self {
        Self {
            mount_point: path.to_path_buf(),
            luks_mapping: None,
            managed_mount: None,
            mode: DeviceAccessMode::UserSpace,
        }
    }

    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        if let Some(managed_mount) = &self.managed_mount
            && let Err(e) = backend_for_mode(self.mode).unmount_managed(managed_mount)
        {
            ui::warning(&format!(
                "Failed to unmount {}: {}",
                self.mount_point.display(),
                e
            ));
        }

        if let Some(mapping_name) = &self.luks_mapping
            && let Err(e) = backend_for_mode(self.mode).close_luks_device(mapping_name)
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
    use std::process::Command;

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

    // ── LUKS tests (require integration env) ────────────────────────────

    use crate::test_util::require_luks_test_device;

    fn ensure_root_for_privileged_mode_tests() -> bool {
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
            eprintln!("Skipped: privileged mode tests require root");
            false
        }
    }

    #[test]
    fn test_is_luks_device_positive() {
        let dev = require_luks_test_device!("is_luks_pos");
        assert!(is_luks_device(&dev.loop_device).unwrap());
    }

    #[test]
    fn test_is_luks_device_positive_privileged() {
        if !ensure_root_for_privileged_mode_tests() {
            return;
        }

        let dev = require_luks_test_device!("is_luks_pos_priv");
        assert!(is_luks_device_with_mode(&dev.loop_device, DeviceAccessMode::Privileged).unwrap());
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
    fn test_open_close_luks_keyfile_privileged() {
        if !ensure_root_for_privileged_mode_tests() {
            return;
        }

        let dev = require_luks_test_device!("open_close_kf_priv");
        let enc = dev.encryption_config();

        let mapped = open_luks_device_with_mode(
            &dev.loop_device,
            &enc.mapping_name,
            enc.keyfile.as_deref(),
            enc.passphrase_env.as_deref(),
            DeviceAccessMode::Privileged,
        )
        .expect("open_luks_device_with_mode privileged with keyfile should succeed");

        assert!(mapped.starts_with("/dev/mapper/"));
        assert!(Path::new(&mapped).exists());

        close_luks_device_with_mode(&enc.mapping_name, DeviceAccessMode::Privileged)
            .expect("close privileged should succeed");
        assert!(!Path::new(&mapped).exists());
    }

    #[test]
    fn test_open_luks_passphrase_env() {
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
    fn test_mount_guard_encrypted_lifecycle_privileged() {
        if !ensure_root_for_privileged_mode_tests() {
            return;
        }

        let dev = require_luks_test_device!("enc_lifecycle_priv");
        let enc = dev.encryption_config();

        let guard = MountGuard::new_encrypted_with_mode(
            &dev.loop_device,
            &enc,
            DeviceAccessMode::Privileged,
        )
        .expect("new_encrypted_with_mode privileged should succeed");

        let mp = guard.mount_point().to_path_buf();
        assert!(mp.exists());

        drop(guard);
        assert!(!mp.exists());
        assert!(
            close_luks_device_with_mode(&enc.mapping_name, DeviceAccessMode::Privileged).is_err()
        );
    }

    #[test]
    fn test_mount_guard_encrypted_not_luks() {
        let _dev = require_luks_test_device!("enc_not_luks");
        let enc = _dev.encryption_config();
        let result = MountGuard::new_encrypted("/dev/null", &enc);
        assert!(result.is_err());
    }
}
