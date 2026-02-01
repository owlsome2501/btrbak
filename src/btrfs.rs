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
