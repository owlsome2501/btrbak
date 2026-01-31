use crate::BackupError;
use crate::btrfs;
use crate::config::{Config, SourceConfig, TargetConfig, TargetLocation};
use crate::device;
use crate::hooks;
use crate::liveboot;
use fs4::fs_std::FileExt;
use log;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

/// File-based lock to prevent concurrent runs with same config name
struct ConfigLock {
    _lock_file: File,
}

impl ConfigLock {
    fn acquire(config_name: &str) -> Result<Self, BackupError> {
        let lock_parent = std::env::temp_dir().join("btrbak_locks");
        fs::create_dir_all(&lock_parent).map_err(|e| {
            BackupError::Lock(format!("Failed to create lock parent directory: {}", e))
        })?;

        let lock_path = lock_parent.join(format!("{}.lock", config_name));
        let lock_file = File::create(&lock_path)
            .map_err(|e| BackupError::Lock(format!("Failed to create lock file: {}", e)))?;

        // Try to acquire exclusive lock (non-blocking)
        match lock_file.try_lock_exclusive() {
            Ok(_) => Ok(Self {
                _lock_file: lock_file,
            }),
            Err(_) => Err(BackupError::Lock(format!(
                "Another btrbak instance is already running with config name '{}'",
                config_name
            ))),
        }
    }
}

/// Main backup procedure
pub fn run_backup(config_path: &Path, dry_run: bool) -> Result<(), BackupError> {
    let config = Config::from_file(config_path)?;
    config.validate()?;

    // Acquire lock to prevent concurrent runs with same config name
    let _lock = ConfigLock::acquire(&config.name)?;

    log::info!("Starting backup with configuration:");
    log::info!("  Sources:");
    for source in &config.sources {
        log::info!("    - {}", source.path.display());
    }
    log::info!("  Target: {:?}", config.target.location);

    if dry_run {
        log::info!("Dry run mode - no changes will be made");
        return Ok(());
    }

    // Step 1: Ensure target is mounted
    let mount_guard = mount_target(&config)?;
    let target_mount = mount_guard.mount_point();

    // Collect errors from each source backup
    let mut errors = Vec::new();

    // Backup each source
    for source in &config.sources {
        log::info!("Backing up source: {}", source.path.display());

        match backup_single_source(source, &config.target, target_mount, &config.name) {
            Ok(()) => {
                log::info!("Successfully backed up source: {}", source.path.display());
            }
            Err(e) => {
                log::error!("Failed to backup source {}: {}", source.path.display(), e);
                errors.push((source.path.clone(), e));
            }
        }
    }

    // Step 5: Update live boot environment if enabled
    if config.target.enable_live_boot && errors.is_empty() {
        // Only update live boot environment if all backups succeeded
        if let Err(e) = update_live_environment(&config, target_mount) {
            log::error!("Failed to update live boot environment: {}", e);
            errors.push((PathBuf::from("live_boot"), e));
        }
    }

    // Report any errors that occurred
    if !errors.is_empty() {
        let error_msg = errors
            .iter()
            .map(|(path, err)| format!("{}: {}", path.display(), err))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(BackupError::Btrfs(format!(
            "Backup completed with errors: {}",
            error_msg
        )));
    }

    log::info!("Backup completed successfully");
    Ok(())
}

/// Backup a single source volume
fn backup_single_source(
    source: &SourceConfig,
    target_config: &TargetConfig,
    target_mount: &Path,
    config_name: &str,
) -> Result<(), BackupError> {
    // Create local snapshot and get parent snapshot (if any)
    let (snapshot_path, local_parent_snapshot) = create_local_snapshot(source, config_name)?;

    // For btrfs send -p, we need the parent snapshot on the source side
    // If we have a local parent snapshot, use it for incremental backup
    let parent_snapshot_for_send = local_parent_snapshot.as_deref();

    // Send snapshot to target
    send_snapshot(
        source,
        target_config,
        &snapshot_path,
        parent_snapshot_for_send,
        target_mount,
    )?;

    // Clean up old local snapshot
    cleanup_old_snapshot(source, local_parent_snapshot, config_name)?;

    Ok(())
}

/// Mount target device if needed
/// Returns a MountGuard that will unmount the device when dropped
fn mount_target(config: &Config) -> Result<device::MountGuard, BackupError> {
    match &config.target.location {
        TargetLocation::MountedPath(path) => {
            // Already mounted, just verify it's accessible
            if !path.exists() {
                return Err(BackupError::Mount(format!(
                    "Target mounted path does not exist: {:?}",
                    path
                )));
            }
            Ok(device::MountGuard::for_mounted_path(path))
        }
        TargetLocation::Device(device) => {
            // Check if encryption is configured
            if let Some(encryption) = &config.target.encryption {
                device::MountGuard::new_encrypted(device, encryption)
            } else {
                device::MountGuard::new(device)
            }
        }
    }
}

/// Create a local snapshot of the source subvolume
/// Returns (new_snapshot_path, parent_snapshot_path) where parent_snapshot_path is Some
/// if there was a previous local snapshot that can be used for incremental backup
fn create_local_snapshot(
    source: &SourceConfig,
    config_name: &str,
) -> Result<(PathBuf, Option<PathBuf>), BackupError> {
    let source_path = &source.path;

    if !btrfs::is_subvolume(source_path)? {
        return Err(BackupError::Btrfs(format!(
            "Source path is not a btrfs subvolume: {:?}",
            source_path
        )));
    }

    // Determine snapshot directory
    let snapshot_dir = source_path.join(&source.snapshot_dir);
    if !snapshot_dir.exists() {
        fs::create_dir_all(&snapshot_dir)?;
    }

    // Dispatch to appropriate handler based on snapshot method
    if source.use_snapper {
        create_snapper_local_snapshot(source, source_path, &snapshot_dir, config_name)
    } else {
        create_manual_local_snapshot(source, source_path, &snapshot_dir, config_name)
    }
}

/// Create local snapshot using snapper
fn create_snapper_local_snapshot(
    source: &SourceConfig,
    _source_path: &Path,
    snapshot_dir: &Path,
    config_name: &str,
) -> Result<(PathBuf, Option<PathBuf>), BackupError> {
    // Use snapper to create snapshot with single type and btrbak description
    let snapshot_name = create_snapper_snapshot(source, config_name)?;
    let snapshot_path = snapshot_dir.join(&snapshot_name);

    // Verify snapper created the snapshot and it's a valid subvolume
    if !btrfs::is_subvolume(&snapshot_path)? {
        return Err(BackupError::Btrfs(format!(
            "Snapper snapshot not found or not a subvolume: {}",
            snapshot_path.display()
        )));
    }

    log::info!("Using snapper snapshot at: {}", snapshot_path.display());

    // Find previous snapper snapshot with btrbak tag for incremental backup
    let parent_snapshot_path = find_previous_snapper_snapshot(source, snapshot_dir, config_name)?;

    Ok((snapshot_path, parent_snapshot_path))
}

/// Create local snapshot manually (without snapper)
fn create_manual_local_snapshot(
    source: &SourceConfig,
    source_path: &Path,
    snapshot_dir: &Path,
    config_name: &str,
) -> Result<(PathBuf, Option<PathBuf>), BackupError> {
    let base_name = format!("{}_{}", source.snapshot_name, config_name);
    let snapshot_path = snapshot_dir.join(&base_name);

    // Prepare previous snapshot path for preserving old snapshot
    let prev_name = format!("{}_prev", base_name);
    let prev_path = snapshot_dir.join(&prev_name);

    // Clean up old _prev snapshot if it exists
    if prev_path.exists() && btrfs::is_subvolume(&prev_path)? {
        log::info!("Cleaning up old previous snapshot: {}", prev_path.display());
        btrfs::delete_subvolume(&prev_path)?;
    }

    // Check if there's an existing snapshot to preserve as parent
    let parent_snapshot_path = if snapshot_path.exists() && btrfs::is_subvolume(&snapshot_path)? {
        // Rename existing snapshot to preserve it as parent for incremental backup
        log::info!("Renaming existing snapshot to: {}", prev_path.display());
        btrfs::rename_subvolume(&snapshot_path, &prev_path)?;
        Some(prev_path)
    } else {
        None
    };

    // Create new read-only snapshot
    btrfs::create_snapshot(source_path, &snapshot_path)?;
    log::info!("Created local snapshot at: {}", snapshot_path.display());

    Ok((snapshot_path, parent_snapshot_path))
}

/// Create snapshot using snapper
fn create_snapper_snapshot(
    source: &SourceConfig,
    target_config_name: &str,
) -> Result<String, BackupError> {
    // snapper_config must be set when use_snapper is true (validated in config)
    let config_name = source.snapper_config.as_ref().ok_or_else(|| {
        BackupError::Config(anyhow::anyhow!(
            "snapper_config must be set when use_snapper is true for source: {}",
            source.path.display()
        ))
    })?;

    // Run snapper create command with single type and btrbak description
    let description = format!("btrbak_{}", target_config_name);
    let output = Command::new("snapper")
        .arg("-c")
        .arg(config_name)
        .arg("create")
        .arg("-t")
        .arg("single")
        .arg("-d")
        .arg(&description)
        .arg("--read-only")
        .arg("--print-number")
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BackupError::Btrfs(format!(
            "Failed to create snapper snapshot for config '{}': {}",
            config_name, stderr
        )));
    }

    // Parse output to get snapshot ID (--print-number outputs just the number)
    let stdout = String::from_utf8_lossy(&output.stdout);
    let snapshot_id = stdout.trim().parse::<u64>().map_err(|e| {
        BackupError::Btrfs(format!(
            "Failed to parse snapshot ID from snapper output '{}': {}",
            stdout, e
        ))
    })?;

    // Snapper creates snapshot at .snapshots/<id>/snapshot
    Ok(format!("{}/snapshot", snapshot_id))
}

/// Find previous snapper snapshot with btrbak description
fn find_previous_snapper_snapshot(
    source: &SourceConfig,
    snapshot_dir: &Path,
    target_config_name: &str,
) -> Result<Option<PathBuf>, BackupError> {
    // snapper_config must be set when use_snapper is true (validated in config)
    let config_name = source.snapper_config.as_ref().ok_or_else(|| {
        BackupError::Config(anyhow::anyhow!(
            "snapper_config must be set when use_snapper is true for source: {}",
            source.path.display()
        ))
    })?;

    // Get snapper list
    let output = Command::new("snapper")
        .arg("-c")
        .arg(config_name)
        .arg("list")
        .arg("--columns")
        .arg("number,description,type")
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BackupError::Btrfs(format!(
            "Failed to list snapper snapshots for config '{}': {}",
            config_name, stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut backup_snapshots = Vec::new();

    // Determine expected description based on target config name
    let expected_description = format!("btrbak_{}", target_config_name);

    // Parse output lines
    for line in stdout.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let number = parts[0];
            let description = parts[1];
            let snapshot_type = parts[2];

            // Look for single type snapshots with matching description
            if snapshot_type == "single"
                && description == expected_description
                && let Ok(id) = number.parse::<u64>()
            {
                backup_snapshots.push(id);
            }
        }
    }

    // Sort descending (newest first)
    backup_snapshots.sort_by(|a, b| b.cmp(a));

    // We need at least 2 snapshots to have a parent
    if backup_snapshots.len() >= 2 {
        // The first one is the one we just created, the second one is the parent
        let parent_id = backup_snapshots[1];
        let parent_path = snapshot_dir.join(format!("{}/snapshot", parent_id));

        if btrfs::is_subvolume(&parent_path)? {
            log::info!(
                "Found previous snapper snapshot at: {}",
                parent_path.display()
            );
            Ok(Some(parent_path))
        } else {
            log::warn!(
                "Previous snapper snapshot path is not a subvolume: {}",
                parent_path.display()
            );
            Ok(None)
        }
    } else {
        log::info!("No previous snapper snapshot found for incremental backup");
        Ok(None)
    }
}

/// Send snapshot to target
fn send_snapshot(
    source: &SourceConfig,
    target_config: &TargetConfig,
    snapshot_path: &Path,
    parent_snapshot: Option<&Path>,
    target_mount: &Path,
) -> Result<(), BackupError> {
    // Determine target volume name and parent directory
    let subvolume_name = btrfs::get_subvolume_name_with_suffix(&source.path);

    let (target_parent_dir, target_subvol_name) = if target_config.enable_live_boot {
        let parent = target_mount.join("@snapshots");
        (parent, subvolume_name)
    } else {
        (target_mount.to_path_buf(), subvolume_name)
    };

    // Create parent directory if it doesn't exist
    if !target_parent_dir.exists() {
        fs::create_dir_all(&target_parent_dir)?;
    }

    // Send the snapshot safely with atomic replacement
    btrfs::send_and_replace_safely(
        snapshot_path,
        parent_snapshot,
        &target_parent_dir,
        "old",
        Some(&target_subvol_name),
    )?;

    log::info!(
        "Sent snapshot to: {}/{}",
        target_parent_dir.display(),
        target_subvol_name
    );
    Ok(())
}

/// Update live boot environment
fn update_live_environment(config: &Config, target_mount: &Path) -> Result<(), BackupError> {
    if let Some(live_boot_config) = &config.live_boot {
        let live_root_subvolume = config.target.live_root_subvolume.as_deref().unwrap_or("@");
        let live_root_path = target_mount.join(live_root_subvolume);

        log::info!(
            "Updating live boot environment for {} sources",
            config.sources.len()
        );

        // Update each source in live boot environment
        for source in &config.sources {
            let subvolume_name = btrfs::get_subvolume_name_with_suffix(&source.path);

            log::info!(
                "Processing source: {} (subvolume: {})",
                source.path.display(),
                subvolume_name
            );

            let snapshot_path = target_mount.join("@snapshots").join(&subvolume_name);

            if btrfs::is_subvolume(&snapshot_path)? {
                log::info!(
                    "Updating subvolume {} in live boot environment",
                    subvolume_name
                );
                update_live_subvolume(&live_root_path, &snapshot_path, &subvolume_name)?;

                log::info!(
                    "Live boot environment updated for {}",
                    source.path.display()
                );
            } else {
                log::warn!(
                    "Snapshot subvolume not found at {}, skipping live update for {}",
                    snapshot_path.display(),
                    source.path.display()
                );
            }
        }

        // Run hooks after all sources are updated
        hooks::run_hooks(
            &live_root_path,
            target_mount,
            &live_boot_config.esp_path,
            &config.hooks,
            &live_boot_config.boot_entry,
            config,
        )?;

        log::info!("Live boot environment update complete");
    }

    Ok(())
}

/// Update a subvolume in live boot environment with latest snapshot
fn update_live_subvolume(
    live_root: &Path,
    snapshot: &Path,
    volume_name: &str,
) -> Result<(), BackupError> {
    let target_subvolume = live_root.join(volume_name);

    // Create read-write snapshot and replace with atomic renames
    btrfs::snapshot_and_replace_safely(&target_subvolume, snapshot, "old")?;

    log::info!("Updated subvolume {} with latest snapshot", volume_name);
    Ok(())
}

/// Clean up old local snapshot after successful backup
fn cleanup_old_snapshot(
    source: &SourceConfig,
    local_parent_snapshot: Option<PathBuf>,
    config_name: &str,
) -> Result<(), BackupError> {
    if source.use_snapper {
        // For snapper, clean up old btrbak snapshots
        cleanup_old_snapper_snapshots(source, config_name)?;
    } else if let Some(parent_path) = local_parent_snapshot {
        // For manual snapshots, delete the renamed parent snapshot
        if parent_path.exists() && btrfs::is_subvolume(&parent_path)? {
            log::info!("Deleting old local snapshot: {}", parent_path.display());
            btrfs::delete_subvolume(&parent_path)?;
        }
    }

    Ok(())
}

/// Clean up old snapper snapshots with btrbak description
fn cleanup_old_snapper_snapshots(
    source: &SourceConfig,
    target_config_name: &str,
) -> Result<(), BackupError> {
    // snapper_config must be set when use_snapper is true (validated in config)
    let config_name = source.snapper_config.as_ref().ok_or_else(|| {
        BackupError::Config(anyhow::anyhow!(
            "snapper_config must be set when use_snapper is true for source: {}",
            source.path.display()
        ))
    })?;

    // Get snapper list
    let output = Command::new("snapper")
        .arg("-c")
        .arg(config_name)
        .arg("list")
        .arg("--columns")
        .arg("number,description,type")
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!("Failed to list snapper snapshots for cleanup: {}", stderr);
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut backup_snapshots = Vec::new();

    // Determine expected description based on target config name
    let expected_description = format!("btrbak_{}", target_config_name);

    // Parse output lines
    for line in stdout.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let number = parts[0];
            let description = parts[1];
            let snapshot_type = parts[2];

            // Look for single type snapshots with matching description
            if snapshot_type == "single"
                && description == expected_description
                && let Ok(id) = number.parse::<u64>()
            {
                backup_snapshots.push(id);
            }
        }
    }

    // Sort descending (newest first)
    backup_snapshots.sort_by(|a, b| b.cmp(a));

    // Keep the latest 2 snapshots (current and previous for next incremental)
    // Delete older ones
    for &snapshot_id in backup_snapshots.iter().skip(2) {
        log::info!("Deleting old snapper snapshot #{}", snapshot_id);

        let output = Command::new("snapper")
            .arg("-c")
            .arg(config_name)
            .arg("delete")
            .arg(snapshot_id.to_string())
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!(
                "Failed to delete snapper snapshot #{}: {}",
                snapshot_id,
                stderr
            );
        }
    }

    Ok(())
}

/// Prepare live boot environment (initial setup)
pub fn prepare_live_environment(config_path: &Path) -> Result<(), BackupError> {
    let config = Config::from_file(config_path)?;
    config.validate()?;

    if !config.target.enable_live_boot {
        return Err(BackupError::Config(anyhow::anyhow!(
            "Live boot not enabled in configuration"
        )));
    }

    let live_boot_config = config
        .live_boot
        .as_ref()
        .ok_or_else(|| BackupError::Config(anyhow::anyhow!("Live boot configuration missing")))?;

    // Mount target if needed
    let mount_guard = mount_target(&config)?;

    // Prepare live boot environment
    let live_root_subvolume = config.target.live_root_subvolume.as_deref().unwrap_or("@");
    let snapshot_subvolume = config
        .target
        .snapshot_subvolume
        .as_deref()
        .unwrap_or("@snapshots");
    liveboot::prepare_live_boot(
        mount_guard.mount_point(),
        live_boot_config,
        live_root_subvolume,
        snapshot_subvolume,
    )?;

    log::info!("Live boot environment prepared successfully");
    Ok(())
}
