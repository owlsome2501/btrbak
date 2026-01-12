use crate::error::BackupError;
use log;
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

/// Safely replace a subvolume with a new one using atomic rename operations
pub fn replace_subvolume_safely(
    target_path: &Path,
    new_source: &Path,
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

    // Step 1: Create new snapshot with temporary name
    create_snapshot_rw(new_source, &new_path)?;

    // Step 2: If target exists, rename it to backup name
    let target_exists = target_path.exists();
    if target_exists {
        rename_subvolume(target_path, &old_backup_path)?;
    }

    // Step 3: Rename new snapshot to target name
    rename_subvolume(&new_path, target_path)?;

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

    // Create a temporary directory for receiving
    let temp_parent = dest_dir.join(".receive-temp");
    if !temp_parent.exists() {
        std::fs::create_dir(&temp_parent)?;
    }

    // Step 1: Receive the subvolume to temporary location
    send_and_receive_piped(source, parent, &temp_parent)?;

    // The received subvolume will have the name from source path
    let received_base_name = get_subvolume_name(source)?;
    let received_path = temp_parent.join(&received_base_name);
    if !received_path.exists() {
        return Err(BackupError::Btrfs(format!(
            "Subvolume not received at expected location: {}",
            received_path.display()
        )));
    }

    // Step 2: Safely replace the target with the received subvolume
    // If target name is different from received name, we need to rename
    if subvol_name == received_base_name {
        move_and_replace_safely(&target_path, &received_path, backup_suffix)?;
    } else {
        // First move received subvolume to target name in temp location
        let renamed_in_temp = temp_parent.join(&subvol_name);
        rename_subvolume(&received_path, &renamed_in_temp)?;
        move_and_replace_safely(&target_path, &renamed_in_temp, backup_suffix)?;
    }

    // Step 3: Clean up temporary directory
    if let Err(e) = std::fs::remove_dir(&temp_parent) {
        log::warn!(
            "Failed to clean up temporary directory {}: {}",
            temp_parent.display(),
            e
        );
    }

    Ok(())
}

/// Safely replace a subvolume by moving another subvolume into its place
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
