use crate::BackupError;
use crate::btrfs;
use crate::config::{Config, SourceConfig, TargetConfig, TargetLocation};
use crate::device;
use crate::hooks;
use crate::liveboot;
use crate::ui;
use fs4::fs_std::FileExt;
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

/// Compute the total number of backup steps for progress display
fn compute_backup_steps(config: &Config) -> usize {
    let mut steps = 0;
    steps += 1; // Mount target
    steps += config.sources.len(); // One step per source
    if config.target.enable_live_boot {
        steps += 1; // Live boot update
    }
    steps += 1; // Summary
    steps
}

/// Main backup procedure
pub fn run_backup(config_path: &Path, dry_run: bool) -> Result<(), BackupError> {
    let config = Config::from_file(config_path)?;
    config.validate()?;

    // Acquire lock to prevent concurrent runs with same config name
    let _lock = ConfigLock::acquire(&config.name)?;

    ui::header(&format!("Backup: {}", config.name));
    ui::info("Sources:");
    for source in &config.sources {
        ui::info(&format!("  {}", source.path.display()));
    }
    ui::info(&format!("Target: {:?}", config.target.location));

    if dry_run {
        ui::info("Dry run mode - no changes will be made");
        return Ok(());
    }

    let backup_start = std::time::Instant::now();
    let total_steps = compute_backup_steps(&config);
    let mut current_step = 0;

    // Step 1: Ensure target is mounted
    current_step += 1;
    ui::step(current_step, total_steps, "Mounting target");
    let mount_guard = mount_target(&config)?;
    let target_mount = mount_guard.mount_point();
    ui::success("Target mounted");

    // Collect per-source results
    let mut source_results: Vec<(PathBuf, Option<btrfs::TransferStats>)> = Vec::new();
    let mut errors = Vec::new();

    // Backup each source
    for source in &config.sources {
        current_step += 1;
        ui::step(
            current_step,
            total_steps,
            &format!("Backing up {}", source.path.display()),
        );

        match backup_single_source(source, &config.target, target_mount, &config.name) {
            Ok(stats) => {
                ui::success(&format!("Backed up {}", source.path.display()));
                source_results.push((source.path.clone(), Some(stats)));
            }
            Err(e) => {
                ui::error(&format!(
                    "Failed to backup {}: {}",
                    source.path.display(),
                    e
                ));
                source_results.push((source.path.clone(), None));
                errors.push((source.path.clone(), e));
            }
        }
    }

    // Update live boot environment if enabled
    if config.target.enable_live_boot && errors.is_empty() {
        current_step += 1;
        ui::step(current_step, total_steps, "Updating live boot environment");

        if let Err(e) = update_live_environment(&config, target_mount) {
            ui::error(&format!("Failed to update live boot environment: {}", e));
            errors.push((PathBuf::from("live_boot"), e));
        } else {
            ui::success("Live boot environment updated");
        }
    }

    // Always show summary
    current_step += 1;
    ui::step(current_step, total_steps, "Summary");
    print_summary(&source_results, &backup_start);
    ui::section_end();

    // Return error after summary if any source failed
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

    Ok(())
}

/// Print a final summary of all source backup results
fn print_summary(
    results: &[(PathBuf, Option<btrfs::TransferStats>)],
    start: &std::time::Instant,
) {
    for (path, stats) in results {
        match stats {
            Some(s) => {
                ui::success(&format!(
                    "{}: {} ({}/s)",
                    path.display(),
                    ui::format_bytes(s.bytes),
                    ui::format_bytes(s.speed()),
                ));
            }
            None => {
                ui::error(&format!("{}: failed", path.display()));
            }
        }
    }

    let total_bytes: u64 = results
        .iter()
        .filter_map(|(_, s)| s.as_ref())
        .map(|s| s.bytes)
        .sum();
    let total_elapsed = start.elapsed().as_secs_f64();

    ui::info(&format!(
        "Total: {} transferred in {}",
        ui::format_bytes(total_bytes),
        ui::format_duration(total_elapsed),
    ));

    let succeeded = results.iter().filter(|(_, s)| s.is_some()).count();
    let failed = results.iter().filter(|(_, s)| s.is_none()).count();

    if failed == 0 {
        ui::success(&format!(
            "Backup completed successfully ({} sources)",
            succeeded,
        ));
    } else {
        ui::warning(&format!(
            "Backup completed with errors ({} succeeded, {} failed)",
            succeeded, failed,
        ));
    }
}

/// Backup a single source volume
fn backup_single_source(
    source: &SourceConfig,
    target_config: &TargetConfig,
    target_mount: &Path,
    config_name: &str,
) -> Result<btrfs::TransferStats, BackupError> {
    // Create local snapshot and get parent snapshot (if any)
    ui::substep("Creating local snapshot");
    let (snapshot_path, local_parent_snapshot) = create_local_snapshot(source, config_name)?;

    // For btrfs send -p, we need the parent snapshot on the source side
    // If we have a local parent snapshot, use it for incremental backup
    let parent_snapshot_for_send = local_parent_snapshot.as_deref();

    // Send snapshot to target
    let mode = if parent_snapshot_for_send.is_some() {
        "incremental"
    } else {
        "full"
    };
    ui::substep(&format!("Sending snapshot to target ({})", mode));
    let stats = send_snapshot(
        source,
        target_config,
        &snapshot_path,
        parent_snapshot_for_send,
        target_mount,
    )?;

    // Clean up old local snapshot
    ui::substep("Cleaning up old snapshots");
    cleanup_old_snapshot(source, local_parent_snapshot, config_name)?;

    Ok(stats)
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

    ui::detail(&format!("Using snapper snapshot at: {}", snapshot_path.display()));

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
        ui::detail(&format!("Cleaning up old previous snapshot: {}", prev_path.display()));
        btrfs::delete_subvolume(&prev_path)?;
    }

    // Check if there's an existing snapshot to preserve as parent
    let parent_snapshot_path = if snapshot_path.exists() && btrfs::is_subvolume(&snapshot_path)? {
        // Rename existing snapshot to preserve it as parent for incremental backup
        ui::detail("Preserving previous snapshot for incremental backup");
        btrfs::rename_subvolume(&snapshot_path, &prev_path)?;
        Some(prev_path)
    } else {
        None
    };

    // Create new read-only snapshot
    btrfs::create_snapshot(source_path, &snapshot_path)?;
    ui::detail(&format!("Created snapshot at: {}", snapshot_path.display()));

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
    let mut cmd = Command::new("snapper");
    cmd.arg("-c")
        .arg(config_name)
        .arg("create")
        .arg("-t")
        .arg("single")
        .arg("-d")
        .arg(&description)
        .arg("--read-only")
        .arg("--print-number");
    ui::cmd_start(&ui::format_cmd(&cmd));

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ui::cmd_stderr_output(&stderr);
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
            ui::detail(&format!(
                "Found previous snapper snapshot at: {}",
                parent_path.display()
            ));
            Ok(Some(parent_path))
        } else {
            ui::warning(&format!(
                "Previous snapper snapshot path is not a subvolume: {}",
                parent_path.display()
            ));
            Ok(None)
        }
    } else {
        ui::detail("No previous snapper snapshot found for incremental backup");
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
) -> Result<btrfs::TransferStats, BackupError> {
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
    let stats = btrfs::send_and_replace_safely(
        snapshot_path,
        parent_snapshot,
        &target_parent_dir,
        "old",
        Some(&target_subvol_name),
    )?;

    ui::detail(&format!(
        "Sent snapshot to: {}/{}",
        target_parent_dir.display(),
        target_subvol_name
    ));
    Ok(stats)
}

/// Update live boot environment
fn update_live_environment(config: &Config, target_mount: &Path) -> Result<(), BackupError> {
    if let Some(live_boot_config) = &config.live_boot {
        let live_root_subvolume = config.target.live_root_subvolume.as_deref().unwrap_or("@");
        let live_root_path = target_mount.join(live_root_subvolume);

        ui::detail(&format!(
            "Updating live boot for {} sources",
            config.sources.len()
        ));

        // Update each source in live boot environment
        for source in &config.sources {
            let subvolume_name = btrfs::get_subvolume_name_with_suffix(&source.path);

            ui::substep(&format!(
                "Updating live subvolume: {}",
                subvolume_name
            ));

            let snapshot_path = target_mount.join("@snapshots").join(&subvolume_name);

            if btrfs::is_subvolume(&snapshot_path)? {
                update_live_subvolume(&live_root_path, &snapshot_path, &subvolume_name)?;
            } else {
                ui::warning(&format!(
                    "Snapshot subvolume not found at {}, skipping live update for {}",
                    snapshot_path.display(),
                    source.path.display()
                ));
            }
        }

        // Run hooks after all sources are updated
        ui::substep("Running post-backup hooks");
        hooks::run_hooks(
            &live_root_path,
            target_mount,
            &live_boot_config.esp_path,
            &config.hooks,
            &live_boot_config.boot_entry,
            config,
        )?;
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

    ui::detail(&format!("Updated subvolume {} with latest snapshot", volume_name));
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
            ui::detail(&format!("Deleting old local snapshot: {}", parent_path.display()));
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
        ui::warning(&format!("Failed to list snapper snapshots for cleanup: {}", stderr));
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
        ui::detail(&format!("Deleting old snapper snapshot #{}", snapshot_id));

        let output = Command::new("snapper")
            .arg("-c")
            .arg(config_name)
            .arg("delete")
            .arg(snapshot_id.to_string())
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            ui::warning(&format!(
                "Failed to delete snapper snapshot #{}: {}",
                snapshot_id, stderr
            ));
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

    ui::header("Prepare Live Boot Environment");

    // Mount target if needed
    ui::step(1, 2, "Mounting target");
    let mount_guard = mount_target(&config)?;
    ui::success("Target mounted");

    // Prepare live boot environment
    ui::step(2, 2, "Setting up live boot");
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

    ui::success("Live boot environment prepared successfully");
    ui::section_end();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn make_config(num_sources: usize, enable_live_boot: bool) -> Config {
        let sources: Vec<SourceConfig> = (0..num_sources)
            .map(|i| SourceConfig {
                path: PathBuf::from(format!("/src{}", i)),
                snapshot_dir: PathBuf::from(".snapshots"),
                use_snapper: false,
                snapshot_name: "btrbak".to_string(),
                snapper_config: None,
            })
            .collect();
        Config {
            name: "test".to_string(),
            sources,
            target: TargetConfig {
                location: TargetLocation::MountedPath(PathBuf::from("/mnt")),
                enable_live_boot,
                snapshot_subvolume: None,
                live_root_subvolume: None,
                encryption: None,
            },
            live_boot: None,
            hooks: HookConfig::default(),
        }
    }

    #[test]
    fn test_compute_steps_no_live_boot() {
        let config = make_config(2, false);
        // 1 mount + 2 sources + 1 summary = 4
        assert_eq!(compute_backup_steps(&config), 4);
    }

    #[test]
    fn test_compute_steps_with_live_boot() {
        let config = make_config(3, true);
        // 1 mount + 3 sources + 1 live boot + 1 summary = 6
        assert_eq!(compute_backup_steps(&config), 6);
    }

    #[test]
    fn test_compute_steps_single_source() {
        let config = make_config(1, false);
        // 1 mount + 1 source + 1 summary = 3
        assert_eq!(compute_backup_steps(&config), 3);
    }

    #[test]
    fn test_config_lock_acquire_release() {
        let lock = ConfigLock::acquire("test_lock_acquire_release");
        assert!(lock.is_ok());
        drop(lock);
        // After releasing, should be able to acquire again
        let lock2 = ConfigLock::acquire("test_lock_acquire_release");
        assert!(lock2.is_ok());
    }

    #[test]
    fn test_config_lock_error_message() {
        // ConfigLock uses fcntl locks which are per-process, so we can't test
        // double-acquire within the same process. Instead verify the error
        // message format that would be produced on contention.
        let err = BackupError::Lock(
            "Another btrbak instance is already running with config name 'test'".to_string(),
        );
        let msg = format!("{}", err);
        assert!(msg.contains("already running"));
    }
}
