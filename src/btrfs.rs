use crate::error::BackupError;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Check if a path is a btrfs subvolume
pub fn is_subvolume(path: &Path) -> Result<bool, BackupError> {
    let output = Command::new("btrfs")
        .arg("subvolume")
        .arg("show")
        .arg(path)
        .output()?;

    Ok(output.status.success())
}

/// Create a read-only snapshot of a subvolume
pub fn create_snapshot(source: &Path, dest: &Path) -> Result<(), BackupError> {
    let output = Command::new("btrfs")
        .arg("subvolume")
        .arg("snapshot")
        .arg("-r")
        .arg(source)
        .arg(dest)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
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
    let output = Command::new("btrfs")
        .arg("subvolume")
        .arg("snapshot")
        .arg(source)
        .arg(dest)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
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
    let output = Command::new("btrfs")
        .arg("subvolume")
        .arg("create")
        .arg(path)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
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
    let output = Command::new("btrfs")
        .arg("subvolume")
        .arg("delete")
        .arg(path)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
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

/// Send a subvolume directly to a receive process via pipe
pub fn send_and_receive_piped(
    source: &Path,
    parent: Option<&Path>,
    dest_dir: &Path,
) -> Result<(), BackupError> {
    let mut send_child = send_subvolume_process(source, parent)?;
    let mut recv_child = receive_subvolume_process(dest_dir)?;

    // Get stdout from send and stdin to receive
    let send_stdout = send_child
        .stdout
        .take()
        .ok_or_else(|| BackupError::Btrfs("Failed to get stdout from send process".to_string()))?;

    let recv_stdin = recv_child
        .stdin
        .take()
        .ok_or_else(|| BackupError::Btrfs("Failed to get stdin for receive process".to_string()))?;

    // Pipe the data
    use std::io::copy;
    copy(
        &mut std::io::BufReader::new(send_stdout),
        &mut std::io::BufWriter::new(recv_stdin),
    )?;

    // Wait for both processes
    let send_output = send_child.wait_with_output()?;
    let recv_output = recv_child.wait_with_output()?;

    if !send_output.status.success() {
        let stderr = String::from_utf8_lossy(&send_output.stderr);
        return Err(BackupError::Btrfs(format!(
            "Failed to send subvolume {}: {}",
            source.display(),
            stderr
        )));
    }

    if !recv_output.status.success() {
        let stderr = String::from_utf8_lossy(&recv_output.stderr);
        return Err(BackupError::Btrfs(format!(
            "Failed to receive subvolume to {}: {}",
            dest_dir.display(),
            stderr
        )));
    }

    Ok(())
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
    let output = Command::new("btrfs")
        .arg("subvolume")
        .arg("show")
        .arg(path)
        .output()?;

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
) -> Result<(), BackupError> {
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
    if let Err(e) = send_and_receive_piped(source, parent, dest_dir) {
        restore_renamed();
        return Err(e);
    }

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

    Ok(())
}

/// Safely replace a subvolume by moving another subvolume into its place
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
    let output = Command::new("btrfs")
        .arg("filesystem")
        .arg("show")
        .arg(path)
        .output()?;

    Ok(output.status.success())
}
