use crate::error::BackupError;
use crate::ui;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Statistics from a single send/receive transfer.
pub struct TransferStats {
    pub bytes: u64,
    pub elapsed_secs: f64,
}

impl TransferStats {
    pub fn speed(&self) -> u64 {
        if self.elapsed_secs > 0.0 {
            (self.bytes as f64 / self.elapsed_secs) as u64
        } else {
            0
        }
    }
}

/// Check if a path is a btrfs subvolume
pub fn is_subvolume(path: &Path) -> Result<bool, BackupError> {
    let mut cmd = Command::new("btrfs");
    cmd.arg("subvolume").arg("show").arg(path);
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = cmd.output()?;
    Ok(output.status.success())
}

/// Create a read-only snapshot of a subvolume
pub fn create_snapshot(source: &Path, dest: &Path) -> Result<(), BackupError> {
    let mut cmd = Command::new("btrfs");
    cmd.arg("subvolume").arg("snapshot").arg("-r").arg(source).arg(dest);
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Btrfs(format!(
            "Failed to create snapshot from {} to {}: {}",
            source.display(),
            dest.display(),
            stderr
        )));
    }

    Ok(())
}

/// Create a read-write snapshot of a subvolume
pub fn create_snapshot_rw(source: &Path, dest: &Path) -> Result<(), BackupError> {
    let mut cmd = Command::new("btrfs");
    cmd.arg("subvolume").arg("snapshot").arg(source).arg(dest);
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Btrfs(format!(
            "Failed to create read-write snapshot from {} to {}: {}",
            source.display(),
            dest.display(),
            stderr
        )));
    }

    Ok(())
}

/// Create a new btrfs subvolume
pub fn create_subvolume(path: &Path) -> Result<(), BackupError> {
    let mut cmd = Command::new("btrfs");
    cmd.arg("subvolume").arg("create").arg(path);
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Btrfs(format!(
            "Failed to create subvolume {}: {}",
            path.display(),
            stderr
        )));
    }

    Ok(())
}

/// Delete a subvolume (snapshot)
pub fn delete_subvolume(path: &Path) -> Result<(), BackupError> {
    let mut cmd = Command::new("btrfs");
    cmd.arg("subvolume").arg("delete").arg(path);
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Btrfs(format!(
            "Failed to delete subvolume {}: {}",
            path.display(),
            stderr
        )));
    }

    Ok(())
}

/// Send a subvolume to stdout (for backup) - returns process handle
pub fn send_subvolume_process(
    source: &Path,
    parent: Option<&Path>,
) -> Result<std::process::Child, BackupError> {
    let mut cmd = Command::new("btrfs");
    cmd.arg("send");

    if let Some(parent) = parent {
        cmd.arg("-p").arg(parent);
    }

    cmd.arg(source)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd.spawn()?;
    Ok(child)
}

/// Receive a subvolume from stdin - returns process handle
pub fn receive_subvolume_process(dest_dir: &Path) -> Result<std::process::Child, BackupError> {
    let child = Command::new("btrfs")
        .arg("receive")
        .arg(dest_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    Ok(child)
}

/// Best-effort enlargement of a pipe buffer via `fcntl(F_SETPIPE_SZ)`.
/// Silently ignores errors (e.g. unprivileged limit below requested size).
fn try_set_pipe_size(fd: std::os::unix::io::RawFd, size: i32) {
    unsafe {
        libc::fcntl(fd, libc::F_SETPIPE_SZ, size);
    }
}

const PIPE_BUF_SIZE: usize = 1024 * 1024; // 1 MiB

/// Zero-copy transfer between two pipe fds using `splice(2)`.
/// Calls `cb(total_bytes)` after each successful splice.
/// Returns total bytes transferred, or an error.
fn splice_transfer(
    stdout: &std::process::ChildStdout,
    stdin: &std::process::ChildStdin,
    mut cb: impl FnMut(u64),
) -> std::io::Result<u64> {
    let fd_in = stdout.as_raw_fd();
    let fd_out = stdin.as_raw_fd();

    try_set_pipe_size(fd_in, PIPE_BUF_SIZE as i32);
    try_set_pipe_size(fd_out, PIPE_BUF_SIZE as i32);

    let mut total: u64 = 0;
    loop {
        let n = unsafe {
            libc::splice(
                fd_in,
                std::ptr::null_mut(),
                fd_out,
                std::ptr::null_mut(),
                PIPE_BUF_SIZE,
                libc::SPLICE_F_MOVE,
            )
        };
        if n > 0 {
            total += n as u64;
            cb(total);
        } else if n == 0 {
            // EOF
            break;
        } else {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EINTR) | Some(libc::EAGAIN) => continue,
                _ => return Err(err),
            }
        }
    }
    Ok(total)
}

/// Fallback userspace copy between pipe fds.
/// Uses a 1 MiB buffer with direct read/write_all (no BufReader/BufWriter).
fn copy_transfer(
    stdout: &mut std::process::ChildStdout,
    stdin: &mut std::process::ChildStdin,
    mut cb: impl FnMut(u64),
) -> std::io::Result<u64> {
    use std::io::{Read, Write};

    let mut buf = vec![0u8; PIPE_BUF_SIZE];
    let mut total: u64 = 0;
    loop {
        let n = stdout.read(&mut buf)?;
        if n == 0 {
            break;
        }
        stdin.write_all(&buf[..n])?;
        total += n as u64;
        cb(total);
    }
    Ok(total)
}

/// Send a subvolume directly to a receive process via pipe.
/// Uses `splice(2)` for zero-copy kernel-space transfer with fallback to
/// userspace copy if splice returns `EINVAL`.
pub fn send_and_receive_piped(
    source: &Path,
    parent: Option<&Path>,
    dest_dir: &Path,
) -> Result<TransferStats, BackupError> {
    // Display as a single logical piped command
    let send_part = if let Some(p) = parent {
        format!("btrfs send -p {} {}", p.display(), source.display())
    } else {
        format!("btrfs send {}", source.display())
    };
    ui::cmd_start(&format!("{} | btrfs receive {}", send_part, dest_dir.display()));

    let mut send_child = send_subvolume_process(source, parent)?;
    let mut recv_child = receive_subvolume_process(dest_dir)?;

    // Get stdout from send and stdin to receive
    let mut send_stdout = send_child
        .stdout
        .take()
        .ok_or_else(|| BackupError::Btrfs("Failed to get stdout from send process".to_string()))?;

    let mut recv_stdin = recv_child
        .stdin
        .take()
        .ok_or_else(|| BackupError::Btrfs("Failed to get stdin for receive process".to_string()))?;

    let tp = ui::start_transfer();
    let start = std::time::Instant::now();

    // Try splice (zero-copy), fall back to userspace copy on EINVAL
    let total_bytes = match splice_transfer(&send_stdout, &recv_stdin, |total| {
        tp.update(total, &start);
    }) {
        Ok(total) => total,
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
            copy_transfer(&mut send_stdout, &mut recv_stdin, |total| {
                tp.update(total, &start);
            })?
        }
        Err(e) => return Err(BackupError::Io(e)),
    };

    let elapsed = start.elapsed().as_secs_f64();
    tp.finish(total_bytes, elapsed);

    // Explicitly close stdin so receive process sees EOF
    drop(recv_stdin);
    drop(send_stdout);

    // Wait for both processes
    let send_output = send_child.wait_with_output()?;
    let recv_output = recv_child.wait_with_output()?;

    if !send_output.status.success() {
        let stderr = String::from_utf8_lossy(&send_output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Btrfs(format!(
            "Failed to send subvolume {}: {}",
            source.display(),
            stderr
        )));
    }

    if !recv_output.status.success() {
        let stderr = String::from_utf8_lossy(&recv_output.stderr);
        ui::cmd_stderr_output(&stderr);
        return Err(BackupError::Btrfs(format!(
            "Failed to receive subvolume to {}: {}",
            dest_dir.display(),
            stderr
        )));
    }

    Ok(TransferStats {
        bytes: total_bytes,
        elapsed_secs: elapsed,
    })
}

/// Find the latest snapshot in a directory
pub fn find_latest_snapshot(snapshot_dir: &Path) -> Result<Option<PathBuf>, BackupError> {
    if !snapshot_dir.exists() {
        return Ok(None);
    }

    let entries = std::fs::read_dir(snapshot_dir)?;
    let mut snapshots = Vec::new();

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if is_subvolume(&path)? {
            snapshots.push(path);
        }
    }

    // Sort by modification time (latest first)
    snapshots.sort_by(|a, b| {
        let a_meta = std::fs::metadata(a).ok();
        let b_meta = std::fs::metadata(b).ok();
        let a_time = a_meta.and_then(|m| m.modified().ok());
        let b_time = b_meta.and_then(|m| m.modified().ok());
        b_time.cmp(&a_time) // reverse order
    });

    Ok(snapshots.first().cloned())
}

/// Get the subvolume ID of a path
pub fn get_subvolume_id(path: &Path) -> Result<u64, BackupError> {
    let mut cmd = Command::new("btrfs");
    cmd.arg("subvolume").arg("show").arg(path);
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = cmd.output()?;

    if !output.status.success() {
        return Err(BackupError::Btrfs(format!(
            "Failed to get subvolume info for {}",
            path.display()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.trim().starts_with("Subvolume ID:") {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() == 2 {
                let id_str = parts[1].trim();
                if let Ok(id) = id_str.parse::<u64>() {
                    return Ok(id);
                }
            }
        }
    }

    Err(BackupError::Btrfs(format!(
        "Could not parse subvolume ID from output: {}",
        stdout
    )))
}

/// Rename a btrfs subvolume (atomic move)
pub fn rename_subvolume(old_path: &Path, new_path: &Path) -> Result<(), BackupError> {
    // btrfs subvolumes are just directories, can be renamed with fs::rename
    // but need to ensure they're in the same filesystem
    std::fs::rename(old_path, new_path)?;

    // Verify it's still a subvolume after rename
    if !is_subvolume(new_path)? {
        // Try to rollback rename
        let _ = std::fs::rename(new_path, old_path);
        return Err(BackupError::Btrfs(format!(
            "Renamed path is not a valid subvolume: {}",
            new_path.display()
        )));
    }

    Ok(())
}

/// Safely replace a subvolume by creating a read-write snapshot and using atomic rename operations
pub fn snapshot_and_replace_safely(
    target_path: &Path,
    snapshot_source: &Path,
    backup_suffix: &str,
) -> Result<(), BackupError> {
    let parent_dir = target_path
        .parent()
        .ok_or_else(|| BackupError::Btrfs("Target path has no parent directory".to_string()))?;

    let target_name = target_path
        .file_name()
        .ok_or_else(|| BackupError::Btrfs("Target path has no file name".to_string()))?
        .to_string_lossy();

    // Create temporary names
    let new_path = parent_dir.join(format!("{}.new", target_name));
    let old_backup_path = parent_dir.join(format!("{}.{}", target_name, backup_suffix));

    // Step 1: Create new read-write snapshot with temporary name
    create_snapshot_rw(snapshot_source, &new_path)?;

    // Step 2: If target exists, rename it to backup name
    let target_exists = target_path.exists();
    if target_exists {
        rename_subvolume(target_path, &old_backup_path)?;
    }

    // Step 3: Rename new snapshot to target name
    if let Err(e) = rename_subvolume(&new_path, target_path) {
        // Restore the old subvolume from backup
        if target_exists {
            let _ = rename_subvolume(&old_backup_path, target_path);
        }
        // Clean up the new snapshot
        let _ = delete_subvolume(&new_path);
        return Err(e);
    }

    // Step 4: If we had an old target and backup succeeded, delete the backup
    if target_exists {
        delete_subvolume(&old_backup_path)?;
    }

    Ok(())
}

/// Get the name of a subvolume (last component of its path)
pub fn get_subvolume_name(path: &Path) -> Result<String, BackupError> {
    path.file_name()
        .ok_or_else(|| BackupError::Btrfs(format!("Path has no file name: {}", path.display())))
        .map(|s| s.to_string_lossy().to_string())
}

/// Send a subvolume and receive it safely with atomic replacement
pub fn send_and_replace_safely(
    source: &Path,
    parent: Option<&Path>,
    dest_dir: &Path,
    backup_suffix: &str,
    target_name: Option<&str>,
) -> Result<TransferStats, BackupError> {
    // Determine target subvolume name
    let subvol_name = match target_name {
        Some(name) => name.to_string(),
        None => get_subvolume_name(source)?,
    };

    let target_path = dest_dir.join(&subvol_name);

    // Create destination directory if it doesn't exist
    if !dest_dir.exists() {
        std::fs::create_dir_all(dest_dir)?;
    }

    // Get the name of the subvolume that will be received
    let received_base_name = get_subvolume_name(source)?;
    let received_path = dest_dir.join(&received_base_name);
    let needs_rename = received_base_name != subvol_name;

    // Check for existing subvolumes that may conflict
    let target_exists = is_subvolume(&target_path)?;

    // Determine if there's a conflict at the location where the subvolume will be received
    // Only check for conflict at received_path if it's different from target_path (needs_rename).
    // If they're the same path, any conflict is already captured by target_exists.
    let received_conflict = needs_rename && is_subvolume(&received_path)?;

    // Prepare backup paths
    let target_backup_path = dest_dir.join(format!("{}.{}", subvol_name, backup_suffix));
    let received_backup_path = if received_conflict {
        Some(dest_dir.join(format!("{}.conflict", received_base_name)))
    } else {
        None
    };

    // Rename conflicting subvolumes if they exist
    if target_exists {
        rename_subvolume(&target_path, &target_backup_path)?;
    }
    if let Some(backup) = &received_backup_path {
        rename_subvolume(&received_path, backup)?;
    }

    // Helper to restore renamed subvolumes on error
    let restore_renamed = || {
        if target_exists {
            let _ = rename_subvolume(&target_backup_path, &target_path);
        }
        if let Some(backup) = &received_backup_path {
            let _ = rename_subvolume(backup, &received_path);
        }
    };

    // Receive the subvolume directly into dest_dir
    let stats = match send_and_receive_piped(source, parent, dest_dir) {
        Ok(stats) => stats,
        Err(e) => {
            restore_renamed();
            return Err(e);
        }
    };

    // Verify the subvolume was received
    if !is_subvolume(&received_path)? {
        restore_renamed();
        return Err(BackupError::Btrfs(format!(
            "Subvolume not received at expected location: {}",
            received_path.display()
        )));
    }

    // Rename received subvolume to target name if needed
    if needs_rename
        && let Err(e) = rename_subvolume(&received_path, &target_path)
    {
        let _ = delete_subvolume(&received_path);
        restore_renamed();
        return Err(e);
    }

    // Clean up backup subvolumes
    if target_exists {
        delete_subvolume(&target_backup_path)?;
    }

    // Restore conflicting subvolume that was renamed to .conflict
    if let Some(backup) = received_backup_path {
        rename_subvolume(&backup, &received_path)?;
    }

    Ok(stats)
}

/// Safely replace a subvolume by moving another subvolume into its place using atomic rename operations
/// This is used when `source_path` is already a subvolume that can be directly moved/renamed
pub fn move_and_replace_safely(
    target_path: &Path,
    source_path: &Path,
    backup_suffix: &str,
) -> Result<(), BackupError> {
    let parent_dir = target_path
        .parent()
        .ok_or_else(|| BackupError::Btrfs("Target path has no parent directory".to_string()))?;

    let target_name = target_path
        .file_name()
        .ok_or_else(|| BackupError::Btrfs("Target path has no file name".to_string()))?
        .to_string_lossy();

    // Create temporary names
    let new_path = parent_dir.join(format!("{}.new", target_name));
    let old_backup_path = parent_dir.join(format!("{}.{}", target_name, backup_suffix));

    // Step 1: Move source to temporary name
    rename_subvolume(source_path, &new_path)?;

    // Step 2: If target exists, rename it to backup name
    let target_exists = target_path.exists();
    if target_exists {
        rename_subvolume(target_path, &old_backup_path)?;
    }

    // Step 3: Rename temporary to target name
    rename_subvolume(&new_path, target_path)?;

    // Step 4: If we had an old target and move succeeded, delete the backup
    if target_exists {
        delete_subvolume(&old_backup_path)?;
    }

    Ok(())
}

/// Convert a filesystem path to a valid subvolume name
pub fn get_volume_name_from_path(path: &Path) -> String {
    let components: Vec<String> = path
        .components()
        .filter_map(|c| {
            let s = c.as_os_str().to_string_lossy();
            if s.is_empty() || s == "." || s == "/" {
                None
            } else {
                Some(s.to_string())
            }
        })
        .collect();

    if components.is_empty() {
        "root".to_string()
    } else {
        components.join("_")
    }
}

/// Get the subvolume name with `_vol` suffix from a filesystem path
pub fn get_subvolume_name_with_suffix(path: &Path) -> String {
    let volume_name = get_volume_name_from_path(path);
    if volume_name == "root" {
        "root_vol".to_string()
    } else {
        format!("{}_vol", volume_name)
    }
}

/// Check if a path is a btrfs filesystem
pub fn is_btrfs_filesystem(path: &Path) -> Result<bool, BackupError> {
    let mut cmd = Command::new("btrfs");
    cmd.arg("filesystem").arg("show").arg(path);
    ui::detail(&format!("$ {}", ui::format_cmd(&cmd)));

    let output = cmd.output()?;
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    // TransferStats::speed() tests

    #[test]
    fn test_transfer_stats_speed_basic() {
        let stats = TransferStats {
            bytes: 1_000_000,
            elapsed_secs: 2.0,
        };
        assert_eq!(stats.speed(), 500_000);
    }

    #[test]
    fn test_transfer_stats_speed_zero_elapsed() {
        let stats = TransferStats {
            bytes: 1_000_000,
            elapsed_secs: 0.0,
        };
        assert_eq!(stats.speed(), 0);
    }

    #[test]
    fn test_transfer_stats_speed_negative_elapsed() {
        let stats = TransferStats {
            bytes: 1_000_000,
            elapsed_secs: -1.0,
        };
        assert_eq!(stats.speed(), 0);
    }

    #[test]
    fn test_transfer_stats_speed_zero_bytes() {
        let stats = TransferStats {
            bytes: 0,
            elapsed_secs: 5.0,
        };
        assert_eq!(stats.speed(), 0);
    }

    // get_volume_name_from_path() tests

    #[test]
    fn test_volume_name_root() {
        assert_eq!(get_volume_name_from_path(Path::new("/")), "root");
    }

    #[test]
    fn test_volume_name_single_component() {
        assert_eq!(get_volume_name_from_path(Path::new("/home")), "home");
    }

    #[test]
    fn test_volume_name_multi_component() {
        assert_eq!(get_volume_name_from_path(Path::new("/var/log")), "var_log");
    }

    #[test]
    fn test_volume_name_deep_path() {
        assert_eq!(
            get_volume_name_from_path(Path::new("/var/lib/docker")),
            "var_lib_docker"
        );
    }

    #[test]
    fn test_volume_name_dot() {
        assert_eq!(get_volume_name_from_path(Path::new(".")), "root");
    }

    #[test]
    fn test_volume_name_relative() {
        assert_eq!(
            get_volume_name_from_path(Path::new("home/user")),
            "home_user"
        );
    }

    #[test]
    fn test_volume_name_trailing_slash() {
        assert_eq!(get_volume_name_from_path(Path::new("/boot/")), "boot");
    }

    // get_subvolume_name_with_suffix() tests

    #[test]
    fn test_subvolume_name_suffix_root() {
        assert_eq!(
            get_subvolume_name_with_suffix(Path::new("/")),
            "root_vol"
        );
    }

    #[test]
    fn test_subvolume_name_suffix_home() {
        assert_eq!(
            get_subvolume_name_with_suffix(Path::new("/home")),
            "home_vol"
        );
    }

    #[test]
    fn test_subvolume_name_suffix_deep() {
        assert_eq!(
            get_subvolume_name_with_suffix(Path::new("/var/log")),
            "var_log_vol"
        );
    }

    #[test]
    fn test_subvolume_name_suffix_dot() {
        assert_eq!(
            get_subvolume_name_with_suffix(Path::new(".")),
            "root_vol"
        );
    }

    // get_subvolume_name() tests

    #[test]
    fn test_get_subvolume_name_simple() {
        let result = get_subvolume_name(Path::new("/mnt/snapshots/root_vol"));
        assert_eq!(result.unwrap(), "root_vol");
    }

    #[test]
    fn test_get_subvolume_name_single() {
        let result = get_subvolume_name(Path::new("/home"));
        assert_eq!(result.unwrap(), "home");
    }

    #[test]
    fn test_get_subvolume_name_root_err() {
        let result = get_subvolume_name(Path::new("/"));
        assert!(result.is_err());
    }

    #[test]
    fn test_get_subvolume_name_nested() {
        let result = get_subvolume_name(Path::new("/a/b/c/d"));
        assert_eq!(result.unwrap(), "d");
    }

    // ========================
    // Integration tests (require BTRBAK_TEST_BTRFS_DIR)
    // ========================

    use crate::test_util::{require_btrfs_recv_dir, require_btrfs_test_dir, write_test_file};

    // --- Subvolume basic operations ---

    #[test]
    fn test_btrfs_create_subvolume_and_is_subvolume() {
        let td = require_btrfs_test_dir!("create_subvol");

        let sv = td.path.join("sv");
        create_subvolume(&sv).unwrap();
        assert!(is_subvolume(&sv).unwrap());

        // Plain directory should NOT be a subvolume
        let plain = td.path.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!is_subvolume(&plain).unwrap());
    }

    #[test]
    fn test_btrfs_create_subvolume_already_exists() {
        let td = require_btrfs_test_dir!("create_subvol_exists");

        let sv = td.path.join("sv");
        create_subvolume(&sv).unwrap();
        let err = create_subvolume(&sv);
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), BackupError::Btrfs(_)));
    }

    #[test]
    fn test_btrfs_delete_subvolume() {
        let td = require_btrfs_test_dir!("delete_subvol");

        let sv = td.path.join("sv");
        create_subvolume(&sv).unwrap();
        assert!(sv.exists());

        delete_subvolume(&sv).unwrap();
        assert!(!sv.exists());
    }

    #[test]
    fn test_btrfs_delete_subvolume_nonexistent() {
        let td = require_btrfs_test_dir!("delete_subvol_noent");

        let sv = td.path.join("does_not_exist");
        let err = delete_subvolume(&sv);
        assert!(err.is_err());
    }

    #[test]
    fn test_btrfs_rename_subvolume() {
        let td = require_btrfs_test_dir!("rename_subvol");

        let old = td.path.join("old_sv");
        let new = td.path.join("new_sv");
        create_subvolume(&old).unwrap();

        rename_subvolume(&old, &new).unwrap();
        assert!(!old.exists());
        assert!(is_subvolume(&new).unwrap());
    }

    #[test]
    fn test_btrfs_get_subvolume_id() {
        let td = require_btrfs_test_dir!("get_subvol_id");

        let sv = td.path.join("sv");
        create_subvolume(&sv).unwrap();

        let id = get_subvolume_id(&sv).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_btrfs_get_subvolume_id_not_subvolume() {
        let td = require_btrfs_test_dir!("get_subvol_id_plain");

        let plain = td.path.join("plain");
        std::fs::create_dir_all(&plain).unwrap();

        let err = get_subvolume_id(&plain);
        assert!(err.is_err());
    }

    #[test]
    fn test_btrfs_is_btrfs_filesystem() {
        let td = require_btrfs_test_dir!("is_btrfs_fs");
        assert!(is_btrfs_filesystem(&td.path).unwrap());
    }

    // --- Snapshot operations ---

    #[test]
    fn test_btrfs_create_snapshot_readonly() {
        let td = require_btrfs_test_dir!("snap_ro");

        let src = td.path.join("src");
        create_subvolume(&src).unwrap();
        write_test_file(&src, "hello.txt", "world");

        let snap = td.path.join("snap_ro");
        create_snapshot(&src, &snap).unwrap();
        assert!(is_subvolume(&snap).unwrap());

        // Snapshot contains the source file
        assert_eq!(
            std::fs::read_to_string(snap.join("hello.txt")).unwrap(),
            "world"
        );

        // Writing to read-only snapshot should fail
        let write_result = std::fs::write(snap.join("new.txt"), "fail");
        assert!(write_result.is_err());
    }

    #[test]
    fn test_btrfs_create_snapshot_rw() {
        let td = require_btrfs_test_dir!("snap_rw");

        let src = td.path.join("src");
        create_subvolume(&src).unwrap();
        write_test_file(&src, "hello.txt", "world");

        let snap = td.path.join("snap_rw");
        create_snapshot_rw(&src, &snap).unwrap();
        assert!(is_subvolume(&snap).unwrap());

        assert_eq!(
            std::fs::read_to_string(snap.join("hello.txt")).unwrap(),
            "world"
        );

        // Writing to read-write snapshot should succeed
        std::fs::write(snap.join("new.txt"), "ok").unwrap();
        assert_eq!(
            std::fs::read_to_string(snap.join("new.txt")).unwrap(),
            "ok"
        );
    }

    // --- find_latest_snapshot ---

    #[test]
    fn test_btrfs_find_latest_snapshot_empty() {
        let td = require_btrfs_test_dir!("find_latest_empty");

        let snap_dir = td.path.join("snapshots");
        std::fs::create_dir_all(&snap_dir).unwrap();

        let result = find_latest_snapshot(&snap_dir).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_btrfs_find_latest_snapshot_single() {
        let td = require_btrfs_test_dir!("find_latest_single");

        let snap_dir = td.path.join("snapshots");
        std::fs::create_dir_all(&snap_dir).unwrap();

        let src = td.path.join("src");
        create_subvolume(&src).unwrap();

        let snap = snap_dir.join("snap1");
        create_snapshot(&src, &snap).unwrap();

        let result = find_latest_snapshot(&snap_dir).unwrap();
        assert_eq!(result.unwrap(), snap);
    }

    #[test]
    fn test_btrfs_find_latest_snapshot_multiple() {
        let td = require_btrfs_test_dir!("find_latest_multi");

        let snap_dir = td.path.join("snapshots");
        std::fs::create_dir_all(&snap_dir).unwrap();

        let src = td.path.join("src");
        create_subvolume(&src).unwrap();

        let snap1 = snap_dir.join("snap1");
        create_snapshot(&src, &snap1).unwrap();

        // Brief pause to ensure different mtime
        std::thread::sleep(std::time::Duration::from_millis(100));

        let snap2 = snap_dir.join("snap2");
        create_snapshot(&src, &snap2).unwrap();

        let result = find_latest_snapshot(&snap_dir).unwrap();
        assert_eq!(result.unwrap(), snap2);
    }

    #[test]
    fn test_btrfs_find_latest_snapshot_nonexistent_dir() {
        let td = require_btrfs_test_dir!("find_latest_nodir");

        let result = find_latest_snapshot(&td.path.join("no_such_dir")).unwrap();
        assert!(result.is_none());
    }

    // --- Send / Receive ---

    #[test]
    fn test_btrfs_send_and_receive_piped() {
        let td = require_btrfs_test_dir!("send_recv_piped");
        let td_recv = require_btrfs_recv_dir!("send_recv_piped");

        let src = td.path.join("src");
        create_subvolume(&src).unwrap();
        write_test_file(&src, "data.txt", "payload");

        // Create read-only snapshot (required for send)
        let snap = td.path.join("snap");
        create_snapshot(&src, &snap).unwrap();

        let dest = td_recv.path.join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        let stats = send_and_receive_piped(&snap, None, &dest).unwrap();
        assert!(stats.bytes > 0);

        // The received subvolume keeps the snapshot name
        let received = dest.join("snap");
        assert!(is_subvolume(&received).unwrap());
        assert_eq!(
            std::fs::read_to_string(received.join("data.txt")).unwrap(),
            "payload"
        );
    }

    #[test]
    fn test_btrfs_send_and_receive_incremental() {
        let td = require_btrfs_test_dir!("send_recv_incr");
        let td_recv = require_btrfs_recv_dir!("send_recv_incr");

        let src = td.path.join("src");
        create_subvolume(&src).unwrap();
        write_test_file(&src, "file1.txt", "first");

        let snap1 = td.path.join("snap1");
        create_snapshot(&src, &snap1).unwrap();

        let dest = td_recv.path.join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        // Full send of snap1
        send_and_receive_piped(&snap1, None, &dest).unwrap();

        // Add more data
        write_test_file(&src, "file2.txt", "second");
        let snap2 = td.path.join("snap2");
        create_snapshot(&src, &snap2).unwrap();

        // Incremental send of snap2 with snap1 as parent
        let stats = send_and_receive_piped(&snap2, Some(&snap1), &dest).unwrap();
        assert!(stats.bytes > 0);

        let received = dest.join("snap2");
        assert!(is_subvolume(&received).unwrap());
        assert_eq!(
            std::fs::read_to_string(received.join("file1.txt")).unwrap(),
            "first"
        );
        assert_eq!(
            std::fs::read_to_string(received.join("file2.txt")).unwrap(),
            "second"
        );
    }

    // --- Atomic replace operations ---

    #[test]
    fn test_btrfs_send_and_replace_safely_new() {
        let td = require_btrfs_test_dir!("send_replace_new");
        let td_recv = require_btrfs_recv_dir!("send_replace_new");

        let src = td.path.join("src");
        create_subvolume(&src).unwrap();
        write_test_file(&src, "f.txt", "content");

        let snap = td.path.join("snap");
        create_snapshot(&src, &snap).unwrap();

        let dest = td_recv.path.join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        let stats =
            send_and_replace_safely(&snap, None, &dest, "old", Some("target")).unwrap();
        assert!(stats.bytes > 0);

        let target = dest.join("target");
        assert!(is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(target.join("f.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn test_btrfs_send_and_replace_safely_existing() {
        let td = require_btrfs_test_dir!("send_replace_exist");
        let td_recv = require_btrfs_recv_dir!("send_replace_exist");

        // Create initial target via send
        let src = td.path.join("src");
        create_subvolume(&src).unwrap();
        write_test_file(&src, "v1.txt", "v1");

        let snap1 = td.path.join("snap1");
        create_snapshot(&src, &snap1).unwrap();

        let dest = td_recv.path.join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        send_and_replace_safely(&snap1, None, &dest, "old", Some("target")).unwrap();

        // Now update and replace
        write_test_file(&src, "v2.txt", "v2");
        let snap2 = td.path.join("snap2");
        create_snapshot(&src, &snap2).unwrap();

        send_and_replace_safely(&snap2, Some(&snap1), &dest, "old", Some("target"))
            .unwrap();

        let target = dest.join("target");
        assert!(is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(target.join("v2.txt")).unwrap(),
            "v2"
        );

        // No .old residue
        assert!(!dest.join("target.old").exists());
    }

    #[test]
    fn test_btrfs_snapshot_and_replace_safely() {
        let td = require_btrfs_test_dir!("snap_replace");

        let src = td.path.join("src");
        create_subvolume(&src).unwrap();
        write_test_file(&src, "data.txt", "hello");

        let snap = td.path.join("snap");
        create_snapshot(&src, &snap).unwrap();

        let target = td.path.join("target");

        snapshot_and_replace_safely(&target, &snap, "old").unwrap();
        assert!(is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(target.join("data.txt")).unwrap(),
            "hello"
        );

        // Replace again to exercise existing-target path
        write_test_file(&src, "data2.txt", "world");
        let snap2 = td.path.join("snap2");
        create_snapshot(&src, &snap2).unwrap();

        snapshot_and_replace_safely(&target, &snap2, "old").unwrap();
        assert!(is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(target.join("data2.txt")).unwrap(),
            "world"
        );
        assert!(!td.path.join("target.old").exists());
    }

    #[test]
    fn test_btrfs_move_and_replace_safely() {
        let td = require_btrfs_test_dir!("move_replace");

        let src = td.path.join("src");
        create_subvolume(&src).unwrap();
        write_test_file(&src, "orig.txt", "orig");

        // Create initial target
        let target = td.path.join("target");
        create_subvolume(&target).unwrap();
        write_test_file(&target, "old.txt", "old");

        // Create new subvolume to replace target with
        let new_sv = td.path.join("new_sv");
        create_subvolume(&new_sv).unwrap();
        write_test_file(&new_sv, "new.txt", "new");

        move_and_replace_safely(&target, &new_sv, "old").unwrap();

        assert!(!new_sv.exists());
        assert!(is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(target.join("new.txt")).unwrap(),
            "new"
        );
        assert!(!td.path.join("target.old").exists());
    }

    #[test]
    fn test_btrfs_move_and_replace_safely_no_target() {
        let td = require_btrfs_test_dir!("move_replace_notarget");

        let target = td.path.join("target");
        let new_sv = td.path.join("new_sv");
        create_subvolume(&new_sv).unwrap();
        write_test_file(&new_sv, "file.txt", "data");

        move_and_replace_safely(&target, &new_sv, "old").unwrap();

        assert!(!new_sv.exists());
        assert!(is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(target.join("file.txt")).unwrap(),
            "data"
        );
    }
}
