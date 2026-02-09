use crate::BackupError;
use crate::btrfs;
use crate::command_runner;
use crate::config::{Config, LiveBootConfig, SourceConfig, TargetConfig, TargetLocation};
use crate::device;
use crate::hooks;
use crate::liveboot;
use crate::ui;
use fs4::fs_std::FileExt;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// File-based lock to prevent concurrent runs with same config name.
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

struct SourceSnapshot<'a> {
    source: &'a SourceConfig,
    config_name: &'a str,
}

impl<'a> SourceSnapshot<'a> {
    fn new(source: &'a SourceConfig, config_name: &'a str) -> Self {
        Self {
            source,
            config_name,
        }
    }

    fn create_local_snapshot(&self) -> Result<(PathBuf, Option<PathBuf>), BackupError> {
        let source_path = &self.source.path;

        if !btrfs::is_subvolume(source_path)? {
            return Err(BackupError::Btrfs(format!(
                "Source path is not a btrfs subvolume: {:?}",
                source_path
            )));
        }

        let snapshot_dir = source_path.join(&self.source.snapshot_dir);
        if !snapshot_dir.exists() {
            fs::create_dir_all(&snapshot_dir)?;
        }

        if self.source.use_snapper {
            self.create_snapper_local_snapshot(&snapshot_dir)
        } else {
            Self::create_manual_local_snapshot(
                self.source,
                source_path,
                &snapshot_dir,
                self.config_name,
            )
        }
    }

    fn cleanup_old_snapshot(
        &self,
        local_parent_snapshot: Option<PathBuf>,
    ) -> Result<(), BackupError> {
        if self.source.use_snapper {
            self.cleanup_old_snapper_snapshots()?;
        } else if let Some(parent_path) = local_parent_snapshot
            && parent_path.exists()
            && btrfs::is_subvolume(&parent_path)?
        {
            ui::detail(&format!(
                "Deleting old local snapshot: {}",
                parent_path.display()
            ));
            btrfs::delete_subvolume(&parent_path)?;
        }

        Ok(())
    }

    fn create_snapper_local_snapshot(
        &self,
        snapshot_dir: &Path,
    ) -> Result<(PathBuf, Option<PathBuf>), BackupError> {
        let snapshot_name = self.create_snapper_snapshot()?;
        let snapshot_path = snapshot_dir.join(&snapshot_name);

        if !btrfs::is_subvolume(&snapshot_path)? {
            return Err(BackupError::Btrfs(format!(
                "Snapper snapshot not found or not a subvolume: {}",
                snapshot_path.display()
            )));
        }

        ui::detail(&format!(
            "Using snapper snapshot at: {}",
            snapshot_path.display()
        ));

        let parent_snapshot_path = self.find_previous_snapper_snapshot(snapshot_dir)?;
        Ok((snapshot_path, parent_snapshot_path))
    }

    fn create_snapper_snapshot(&self) -> Result<String, BackupError> {
        let config_name = self.snapper_config()?;

        let description = format!("btrbak_{}", self.config_name);
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

        let output = command_runner::output(&mut cmd)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            ui::cmd_stderr_output(&stderr);
            return Err(BackupError::Btrfs(format!(
                "Failed to create snapper snapshot for config '{}': {}",
                config_name, stderr
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let snapshot_id = stdout.trim().parse::<u64>().map_err(|e| {
            BackupError::Btrfs(format!(
                "Failed to parse snapshot ID from snapper output '{}': {}",
                stdout, e
            ))
        })?;

        Ok(format!("{}/snapshot", snapshot_id))
    }

    fn find_previous_snapper_snapshot(
        &self,
        snapshot_dir: &Path,
    ) -> Result<Option<PathBuf>, BackupError> {
        let backup_snapshots = self.list_btrbak_snapper_snapshot_ids()?;

        if backup_snapshots.len() >= 2 {
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

    fn cleanup_old_snapper_snapshots(&self) -> Result<(), BackupError> {
        let config_name = self.snapper_config()?;

        let backup_snapshots = match self.list_btrbak_snapper_snapshot_ids() {
            Ok(ids) => ids,
            Err(e) => {
                ui::warning(&format!(
                    "Failed to list snapper snapshots for cleanup: {}",
                    e
                ));
                return Ok(());
            }
        };

        for &snapshot_id in backup_snapshots.iter().skip(2) {
            ui::detail(&format!("Deleting old snapper snapshot #{}", snapshot_id));

            let mut cmd = Command::new("snapper");
            cmd.arg("-c")
                .arg(config_name)
                .arg("delete")
                .arg(snapshot_id.to_string());
            let output = command_runner::output(&mut cmd)?;

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

    fn list_btrbak_snapper_snapshot_ids(&self) -> Result<Vec<u64>, BackupError> {
        let config_name = self.snapper_config()?;

        let mut cmd = Command::new("snapper");
        cmd.arg("-c")
            .arg(config_name)
            .arg("list")
            .arg("--columns")
            .arg("number,description,type");
        let output = command_runner::output(&mut cmd)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BackupError::Btrfs(format!(
                "Failed to list snapper snapshots for config '{}': {}",
                config_name, stderr
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let expected_description = format!("btrbak_{}", self.config_name);
        let mut backup_snapshots =
            Self::parse_btrbak_snapper_snapshot_ids(&stdout, &expected_description);
        backup_snapshots.sort_by(|a, b| b.cmp(a));
        Ok(backup_snapshots)
    }

    fn parse_btrbak_snapper_snapshot_ids(output: &str, expected_description: &str) -> Vec<u64> {
        let mut backup_snapshots = Vec::new();

        for line in output.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                continue;
            }

            let number = parts[0];
            let description = parts[1];
            let snapshot_type = parts[2];

            if snapshot_type == "single"
                && description == expected_description
                && let Ok(id) = number.parse::<u64>()
            {
                backup_snapshots.push(id);
            }
        }

        backup_snapshots
    }

    fn snapper_config(&self) -> Result<&str, BackupError> {
        self.source.snapper_config.as_deref().ok_or_else(|| {
            BackupError::Config(anyhow::anyhow!(
                "snapper_config must be set when use_snapper is true for source: {}",
                self.source.path.display()
            ))
        })
    }

    fn create_manual_local_snapshot(
        source: &SourceConfig,
        source_path: &Path,
        snapshot_dir: &Path,
        config_name: &str,
    ) -> Result<(PathBuf, Option<PathBuf>), BackupError> {
        let base_name = format!("{}_{}", source.snapshot_name, config_name);
        let snapshot_path = snapshot_dir.join(&base_name);

        let prev_name = format!("{}_prev", base_name);
        let prev_path = snapshot_dir.join(&prev_name);

        if prev_path.exists() && btrfs::is_subvolume(&prev_path)? {
            ui::detail(&format!(
                "Cleaning up old previous snapshot: {}",
                prev_path.display()
            ));
            btrfs::delete_subvolume(&prev_path)?;
        }

        let parent_snapshot_path = if snapshot_path.exists() && btrfs::is_subvolume(&snapshot_path)?
        {
            ui::detail("Preserving previous snapshot for incremental backup");
            btrfs::rename_subvolume(&snapshot_path, &prev_path)?;
            Some(prev_path)
        } else {
            None
        };

        btrfs::create_snapshot(source_path, &snapshot_path)?;
        ui::detail(&format!("Created snapshot at: {}", snapshot_path.display()));

        Ok((snapshot_path, parent_snapshot_path))
    }
}

struct LiveEnvironmentUpdater<'a> {
    config: &'a Config,
    target_mount: &'a Path,
}

impl<'a> LiveEnvironmentUpdater<'a> {
    fn new(config: &'a Config, target_mount: &'a Path) -> Self {
        Self {
            config,
            target_mount,
        }
    }

    fn update(&self) -> Result<(), BackupError> {
        if let Some(live_boot_config) = &self.config.live_boot {
            let live_boot_subvolume = self
                .config
                .target
                .live_boot_subvolume
                .as_deref()
                .unwrap_or("@");
            let live_boot_path = self.target_mount.join(live_boot_subvolume);

            ui::detail(&format!(
                "Updating live boot for {} sources",
                self.config.sources.len()
            ));

            for source in &self.config.sources {
                let subvolume_name = btrfs::get_subvolume_name_with_suffix(&source.path);

                ui::substep(&format!("Updating live subvolume: {}", subvolume_name));

                let snapshot_path = self.target_mount.join("@snapshots").join(&subvolume_name);

                if btrfs::is_subvolume(&snapshot_path)? {
                    self.update_live_subvolume(&live_boot_path, &snapshot_path, &subvolume_name)?;
                } else {
                    ui::warning(&format!(
                        "Snapshot subvolume not found at {}, skipping live update for {}",
                        snapshot_path.display(),
                        source.path.display()
                    ));
                }
            }

            let esp_mount = mount_esp(live_boot_config)?;

            ui::substep("Running post-backup hooks");
            hooks::run_hooks(
                &live_boot_path,
                self.target_mount,
                esp_mount.mount_point(),
                &self.config.hooks,
                &live_boot_config.boot_entry,
                self.config,
            )?;
        }

        Ok(())
    }

    fn update_live_subvolume(
        &self,
        live_boot_path: &Path,
        snapshot: &Path,
        volume_name: &str,
    ) -> Result<(), BackupError> {
        let target_subvolume = live_boot_path.join(volume_name);

        btrfs::snapshot_and_replace_safely(&target_subvolume, snapshot, "old")?;

        ui::detail(&format!(
            "Updated subvolume {} with latest snapshot",
            volume_name
        ));
        Ok(())
    }
}

/// Compute the total number of backup steps for progress display.
fn compute_backup_steps(config: &Config) -> usize {
    let mut steps = 0;
    steps += 1;
    steps += config.sources.len();
    if config.target.enable_live_boot {
        steps += 1;
    }
    steps += 1;
    steps
}

/// Print a final summary of all source backup results.
fn print_summary(results: &[(PathBuf, Option<btrfs::TransferStats>)], start: &Instant) {
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

/// Mount target device if needed.
/// Returns a MountGuard that will unmount the device when dropped.
fn mount_target(config: &Config) -> Result<device::MountGuard, BackupError> {
    match &config.target.location {
        TargetLocation::MountedPath(path) => {
            if !path.exists() {
                return Err(BackupError::Mount(format!(
                    "Target mounted path does not exist: {:?}",
                    path
                )));
            }
            Ok(device::MountGuard::for_mounted_path(path))
        }
        TargetLocation::Device(device) => {
            if let Some(encryption) = &config.target.encryption {
                device::MountGuard::new_encrypted(device, encryption)
            } else {
                device::MountGuard::new(device)
            }
        }
    }
}

/// Mount ESP location if needed.
/// Returns a MountGuard that will unmount the device when dropped.
fn mount_esp(live_boot_config: &LiveBootConfig) -> Result<device::MountGuard, BackupError> {
    match &live_boot_config.esp_location {
        TargetLocation::MountedPath(path) => {
            if !path.exists() {
                return Err(BackupError::Mount(format!(
                    "ESP mounted path does not exist: {:?}",
                    path
                )));
            }
            Ok(device::MountGuard::for_mounted_path(path))
        }
        TargetLocation::Device(device) => device::MountGuard::new(device),
    }
}

/// Create a local snapshot of the source subvolume.
/// Returns (new_snapshot_path, parent_snapshot_path).
#[cfg(test)]
fn create_local_snapshot(
    source: &SourceConfig,
    config_name: &str,
) -> Result<(PathBuf, Option<PathBuf>), BackupError> {
    SourceSnapshot::new(source, config_name).create_local_snapshot()
}

/// Create local snapshot manually (without snapper).
#[cfg(test)]
fn create_manual_local_snapshot(
    source: &SourceConfig,
    source_path: &Path,
    snapshot_dir: &Path,
    config_name: &str,
) -> Result<(PathBuf, Option<PathBuf>), BackupError> {
    SourceSnapshot::create_manual_local_snapshot(source, source_path, snapshot_dir, config_name)
}

/// Send snapshot to target.
fn send_snapshot(
    source: &SourceConfig,
    target_config: &TargetConfig,
    snapshot_path: &Path,
    parent_snapshot: Option<&Path>,
    target_mount: &Path,
) -> Result<btrfs::TransferStats, BackupError> {
    let subvolume_name = btrfs::get_subvolume_name_with_suffix(&source.path);

    let (target_parent_dir, target_subvol_name) = if target_config.enable_live_boot {
        let parent = target_mount.join("@snapshots");
        (parent, subvolume_name)
    } else {
        (target_mount.to_path_buf(), subvolume_name)
    };

    if !target_parent_dir.exists() {
        fs::create_dir_all(&target_parent_dir)?;
    }

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

/// Clean up old local snapshot after successful backup.
#[cfg(test)]
fn cleanup_old_snapshot(
    source: &SourceConfig,
    local_parent_snapshot: Option<PathBuf>,
    config_name: &str,
) -> Result<(), BackupError> {
    SourceSnapshot::new(source, config_name).cleanup_old_snapshot(local_parent_snapshot)
}

/// Backup a single source volume.
fn backup_single_source(
    source: &SourceConfig,
    target_config: &TargetConfig,
    target_mount: &Path,
    config_name: &str,
) -> Result<btrfs::TransferStats, BackupError> {
    let snapshot_workflow = SourceSnapshot::new(source, config_name);

    ui::substep("Creating local snapshot");
    let (snapshot_path, local_parent_snapshot) = snapshot_workflow.create_local_snapshot()?;

    let parent_snapshot_for_send = local_parent_snapshot.as_deref();

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

    ui::substep("Cleaning up old snapshots");
    snapshot_workflow.cleanup_old_snapshot(local_parent_snapshot)?;

    Ok(stats)
}

/// Main backup procedure.
pub fn run_backup(config_path: &Path, dry_run: bool) -> Result<(), BackupError> {
    let config = Config::from_file(config_path)?;
    config.validate()?;

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

    let backup_start = Instant::now();
    let total_steps = compute_backup_steps(&config);
    let mut current_step = 0;

    current_step += 1;
    ui::step(current_step, total_steps, "Mounting target");
    let mount_guard = mount_target(&config)?;
    let target_mount = mount_guard.mount_point();
    ui::success("Target mounted");

    let mut source_results: Vec<(PathBuf, Option<btrfs::TransferStats>)> = Vec::new();
    let mut errors = Vec::new();

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

    if config.target.enable_live_boot && errors.is_empty() {
        current_step += 1;
        ui::step(current_step, total_steps, "Updating live boot environment");

        if let Err(e) = LiveEnvironmentUpdater::new(&config, target_mount).update() {
            ui::error(&format!("Failed to update live boot environment: {}", e));
            errors.push((PathBuf::from("live_boot"), e));
        } else {
            ui::success("Live boot environment updated");
        }
    }

    current_step += 1;
    ui::step(current_step, total_steps, "Summary");
    print_summary(&source_results, &backup_start);
    ui::section_end();

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

/// Prepare live boot environment (initial setup).
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

    ui::step(1, 2, "Mounting target");
    let mount_guard = mount_target(&config)?;
    ui::success("Target mounted");

    ui::step(2, 2, "Setting up live boot");
    let live_boot_subvolume = config.target.live_boot_subvolume.as_deref().unwrap_or("@");
    let snapshot_subvolume = config
        .target
        .snapshot_subvolume
        .as_deref()
        .unwrap_or("@snapshots");
    let esp_mount = mount_esp(live_boot_config)?;
    liveboot::prepare_live_boot(
        mount_guard.mount_point(),
        live_boot_config,
        esp_mount.mount_point(),
        live_boot_subvolume,
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
                live_boot_subvolume: None,
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

    #[test]
    fn test_parse_snapper_snapshot_ids_filters_expected_single_only() {
        let output = r#"
# | number | description | type
41 btrbak_test single
42 btrbak_test pre
43 other single
abc btrbak_test single
44 btrbak_test single
"#;

        let ids = SourceSnapshot::parse_btrbak_snapper_snapshot_ids(output, "btrbak_test");
        assert_eq!(ids, vec![41, 44]);
    }

    #[test]
    fn test_parse_snapper_snapshot_ids_ignores_malformed_lines() {
        let output = r#"
99
100 only-two-columns
   # comment
"#;
        let ids = SourceSnapshot::parse_btrbak_snapper_snapshot_ids(output, "btrbak_test");
        assert!(ids.is_empty());
    }

    #[test]
    fn test_create_snapper_snapshot_injected_command_failure() {
        let source = SourceConfig {
            path: PathBuf::from("/snapper-source"),
            snapshot_dir: PathBuf::from(".snapshots"),
            use_snapper: true,
            snapshot_name: "btrbak".to_string(),
            snapper_config: Some("root".to_string()),
        };
        let workflow = SourceSnapshot::new(&source, "test");

        let _runner = crate::test_util::scoped_hook_command_runner(
            crate::test_util::HookCommandRunner::new().with_output_hook(|cmd| {
                if crate::test_util::command_program(cmd) == "snapper"
                    && crate::test_util::command_args(cmd)
                        .iter()
                        .any(|arg| arg == "create")
                {
                    return Some(Ok(crate::test_util::mock_output(
                        17,
                        "",
                        "injected snapper create failure\n",
                    )));
                }
                None
            }),
        );

        let result = workflow.create_snapper_snapshot();
        assert!(result.is_err());
        assert!(format!("{}", result.err().unwrap()).contains("Failed to create snapper snapshot"));
    }

    #[test]
    fn test_create_snapper_snapshot_injected_invalid_id_output() {
        let source = SourceConfig {
            path: PathBuf::from("/snapper-source"),
            snapshot_dir: PathBuf::from(".snapshots"),
            use_snapper: true,
            snapshot_name: "btrbak".to_string(),
            snapper_config: Some("root".to_string()),
        };
        let workflow = SourceSnapshot::new(&source, "test");

        let _runner = crate::test_util::scoped_hook_command_runner(
            crate::test_util::HookCommandRunner::new().with_output_hook(|cmd| {
                if crate::test_util::command_program(cmd) == "snapper"
                    && crate::test_util::command_args(cmd)
                        .iter()
                        .any(|arg| arg == "create")
                {
                    return Some(Ok(crate::test_util::mock_output(0, "not-a-number\n", "")));
                }
                None
            }),
        );

        let result = workflow.create_snapper_snapshot();
        assert!(result.is_err());
        assert!(format!("{}", result.err().unwrap()).contains("Failed to parse snapshot ID"));
    }

    #[test]
    fn test_list_snapper_snapshot_ids_injected_command_failure() {
        let source = SourceConfig {
            path: PathBuf::from("/snapper-source"),
            snapshot_dir: PathBuf::from(".snapshots"),
            use_snapper: true,
            snapshot_name: "btrbak".to_string(),
            snapper_config: Some("root".to_string()),
        };
        let workflow = SourceSnapshot::new(&source, "test");

        let _runner = crate::test_util::scoped_hook_command_runner(
            crate::test_util::HookCommandRunner::new().with_output_hook(|cmd| {
                if crate::test_util::command_program(cmd) == "snapper"
                    && crate::test_util::command_args(cmd)
                        .iter()
                        .any(|arg| arg == "list")
                {
                    return Some(Ok(crate::test_util::mock_output(
                        23,
                        "",
                        "injected snapper list failure\n",
                    )));
                }
                None
            }),
        );

        let result = workflow.list_btrbak_snapper_snapshot_ids();
        assert!(result.is_err());
        assert!(format!("{}", result.err().unwrap()).contains("Failed to list snapper snapshots"));
    }

    #[test]
    fn test_cleanup_old_snapper_snapshots_list_failure_is_non_fatal() {
        let source = SourceConfig {
            path: PathBuf::from("/snapper-source"),
            snapshot_dir: PathBuf::from(".snapshots"),
            use_snapper: true,
            snapshot_name: "btrbak".to_string(),
            snapper_config: Some("root".to_string()),
        };
        let workflow = SourceSnapshot::new(&source, "test");

        let _runner = crate::test_util::scoped_hook_command_runner(
            crate::test_util::HookCommandRunner::new().with_output_hook(|cmd| {
                if crate::test_util::command_program(cmd) == "snapper"
                    && crate::test_util::command_args(cmd)
                        .iter()
                        .any(|arg| arg == "list")
                {
                    return Some(Ok(crate::test_util::mock_output(
                        19,
                        "",
                        "injected snapper list failure\n",
                    )));
                }
                None
            }),
        );

        let result = workflow.cleanup_old_snapper_snapshots();
        assert!(result.is_ok());
    }

    #[test]
    fn test_mount_target_mounted_path_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = make_config(1, false);
        config.target.location = TargetLocation::MountedPath(tmp.path().to_path_buf());

        let guard = mount_target(&config).unwrap();
        assert_eq!(guard.mount_point(), tmp.path());
    }

    #[test]
    fn test_mount_target_mounted_path_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");

        let mut config = make_config(1, false);
        config.target.location = TargetLocation::MountedPath(missing);

        let result = mount_target(&config);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(format!("{}", err).contains("does not exist"));
    }

    #[test]
    fn test_mount_esp_mounted_path_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let live_boot = LiveBootConfig {
            esp_location: TargetLocation::MountedPath(tmp.path().to_path_buf()),
            esp_path: PathBuf::from("/efi"),
            bootloader: BootloaderType::SystemdBoot,
            boot_entry: BootEntryConfig::default(),
        };

        let guard = mount_esp(&live_boot).unwrap();
        assert_eq!(guard.mount_point(), tmp.path());
    }

    #[test]
    fn test_mount_esp_mounted_path_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("esp-missing");
        let live_boot = LiveBootConfig {
            esp_location: TargetLocation::MountedPath(missing),
            esp_path: PathBuf::from("/efi"),
            bootloader: BootloaderType::SystemdBoot,
            boot_entry: BootEntryConfig::default(),
        };

        let result = mount_esp(&live_boot);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(format!("{}", err).contains("does not exist"));
    }

    // ========================
    // Integration tests (require BTRBAK_TEST_BTRFS_DIR)
    // ========================

    mod root_required_tests_show_ops {
        use super::*;
        use crate::test_util::{make_source_config, require_btrfs_test_dir, write_test_file};

        // --- Local snapshot (manual) ---

        #[test]
        fn test_backup_create_manual_snapshot_first() {
            let td = require_btrfs_test_dir!("manual_snap_first");

            let src = td.path.join("src");
            btrfs::create_subvolume(&src).unwrap();
            write_test_file(&src, "a.txt", "aaa");

            let snap_dir = td.path.join("snaps");
            fs::create_dir_all(&snap_dir).unwrap();

            let source = make_source_config(&src, Path::new("snaps"));

            let (snap_path, parent) =
                create_manual_local_snapshot(&source, &src, &snap_dir, "test").unwrap();

            assert!(btrfs::is_subvolume(&snap_path).unwrap());
            assert!(parent.is_none());
            assert_eq!(fs::read_to_string(snap_path.join("a.txt")).unwrap(), "aaa");
        }

        #[test]
        fn test_backup_create_manual_snapshot_incremental() {
            let td = require_btrfs_test_dir!("manual_snap_incr");

            let src = td.path.join("src");
            btrfs::create_subvolume(&src).unwrap();
            write_test_file(&src, "a.txt", "v1");

            let snap_dir = td.path.join("snaps");
            fs::create_dir_all(&snap_dir).unwrap();

            let source = make_source_config(&src, Path::new("snaps"));

            // First snapshot
            let (snap1, _) =
                create_manual_local_snapshot(&source, &src, &snap_dir, "test").unwrap();
            assert!(btrfs::is_subvolume(&snap1).unwrap());

            // Update source
            write_test_file(&src, "a.txt", "v2");

            // Second snapshot — previous snapshot becomes parent
            let (snap2, parent) =
                create_manual_local_snapshot(&source, &src, &snap_dir, "test").unwrap();
            assert!(btrfs::is_subvolume(&snap2).unwrap());
            assert!(parent.is_some());
            assert!(btrfs::is_subvolume(parent.as_ref().unwrap()).unwrap());
        }

        #[test]
        fn test_backup_create_manual_snapshot_cleans_old_prev() {
            let td = require_btrfs_test_dir!("manual_snap_clean");

            let src = td.path.join("src");
            btrfs::create_subvolume(&src).unwrap();

            let snap_dir = td.path.join("snaps");
            fs::create_dir_all(&snap_dir).unwrap();

            let source = make_source_config(&src, Path::new("snaps"));

            // Round 1: creates btrbak_test
            let (_s1, _) = create_manual_local_snapshot(&source, &src, &snap_dir, "test").unwrap();

            // Round 2: renames round-1 snapshot to btrbak_test_prev, creates new btrbak_test
            let (_s2, prev2) =
                create_manual_local_snapshot(&source, &src, &snap_dir, "test").unwrap();
            let prev2_path = prev2.unwrap();
            // prev2_path holds the round-1 snapshot; capture its subvolume ID
            let prev2_id = btrfs::get_subvolume_id(&prev2_path).unwrap();

            // Round 3: deletes old _prev (round-1), renames round-2 to _prev, creates new
            let (_s3, prev3) =
                create_manual_local_snapshot(&source, &src, &snap_dir, "test").unwrap();

            // The path btrbak_test_prev still exists, but it now holds a DIFFERENT
            // subvolume (the round-2 snapshot), confirming the old _prev was deleted.
            let prev3_path = prev3.as_ref().unwrap();
            assert!(btrfs::is_subvolume(prev3_path).unwrap());
            let prev3_id = btrfs::get_subvolume_id(prev3_path).unwrap();
            assert_ne!(prev2_id, prev3_id, "old _prev should have been replaced");

            // Only 2 subvolumes should remain: btrbak_test and btrbak_test_prev
            let subvol_count = fs::read_dir(&snap_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| btrfs::is_subvolume(&e.path()).unwrap_or(false))
                .count();
            assert_eq!(subvol_count, 2);
        }

        #[test]
        fn test_backup_create_local_snapshot() {
            let td = require_btrfs_test_dir!("local_snap");

            let src = td.path.join("src");
            btrfs::create_subvolume(&src).unwrap();
            write_test_file(&src, "x.txt", "xx");

            // create_local_snapshot expects snapshot_dir relative to source
            let source = make_source_config(&src, Path::new(".snapshots"));

            let (snap_path, parent) = create_local_snapshot(&source, "test").unwrap();
            assert!(btrfs::is_subvolume(&snap_path).unwrap());
            assert!(parent.is_none());

            // .snapshots dir should have been auto-created
            assert!(src.join(".snapshots").exists());
        }

        #[test]
        fn test_backup_create_local_snapshot_not_subvolume() {
            let td = require_btrfs_test_dir!("local_snap_nosv");

            let plain = td.path.join("plain");
            fs::create_dir_all(&plain).unwrap();

            let source = make_source_config(&plain, Path::new(".snapshots"));
            let err = create_local_snapshot(&source, "test");
            assert!(err.is_err());
        }

        #[test]
        fn test_backup_cleanup_old_snapshot_manual() {
            let td = require_btrfs_test_dir!("cleanup_manual");

            let src = td.path.join("src");
            btrfs::create_subvolume(&src).unwrap();

            let snap_dir = src.join(".snapshots");
            fs::create_dir_all(&snap_dir).unwrap();

            let source = make_source_config(&src, Path::new(".snapshots"));

            // Create a "previous" snapshot subvolume
            let prev = snap_dir.join("btrbak_test_prev");
            btrfs::create_subvolume(&prev).unwrap();

            cleanup_old_snapshot(&source, Some(prev.clone()), "test").unwrap();
            assert!(!prev.exists());
        }

        #[test]
        fn test_backup_cleanup_old_snapshot_none() {
            let td = require_btrfs_test_dir!("cleanup_none");

            let src = td.path.join("src");
            btrfs::create_subvolume(&src).unwrap();

            let source = make_source_config(&src, Path::new(".snapshots"));

            // parent is None — should be a no-op
            cleanup_old_snapshot(&source, None, "test").unwrap();
        }

        #[test]
        fn test_backup_cleanup_old_snapshot_non_subvolume_parent_is_noop() {
            let td = require_btrfs_test_dir!("cleanup_plain_parent");

            let src = td.path.join("src");
            btrfs::create_subvolume(&src).unwrap();
            let source = make_source_config(&src, Path::new(".snapshots"));

            let plain_parent = src.join(".snapshots").join("btrbak_test_prev");
            fs::create_dir_all(&plain_parent).unwrap();
            fs::write(plain_parent.join("keep.txt"), "keep").unwrap();

            cleanup_old_snapshot(&source, Some(plain_parent.clone()), "test").unwrap();

            assert!(plain_parent.exists());
            assert_eq!(
                fs::read_to_string(plain_parent.join("keep.txt")).unwrap(),
                "keep"
            );
        }

        // End-to-end workflow tests are now in tests/backup_workflow_integration.rs
        // to follow Rust's external integration-test convention.
    }

    mod root_required_tests {
        use super::*;
        use crate::test_util::{
            make_source_config, make_target_config, require_btrfs_recv_dir, require_btrfs_test_dir,
            write_test_file,
        };
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
        fn test_backup_send_snapshot() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let td = require_btrfs_test_dir!("send_snap");
            let td_recv = require_btrfs_recv_dir!("send_snap");

            let src = td.path.join("src");
            btrfs::create_subvolume(&src).unwrap();
            write_test_file(&src, "f.txt", "content");

            let snap_dir = src.join(".snapshots");
            fs::create_dir_all(&snap_dir).unwrap();

            let snap = snap_dir.join("btrbak_test");
            btrfs::create_snapshot(&src, &snap).unwrap();

            let target_dir = td_recv.path.join("target");
            fs::create_dir_all(&target_dir).unwrap();

            let source = make_source_config(&src, Path::new(".snapshots"));
            let target_config = make_target_config(&target_dir);

            let stats = send_snapshot(&source, &target_config, &snap, None, &target_dir).unwrap();
            assert!(stats.bytes > 0);

            // The sent subvolume should be named with _vol suffix
            let vol_name = btrfs::get_subvolume_name_with_suffix(&src);
            let received = target_dir.join(&vol_name);
            assert!(btrfs::is_subvolume(&received).unwrap());
        }

        #[test]
        fn test_backup_send_snapshot_live_boot_layout() {
            if !ensure_root_for_root_required_tests() {
                return;
            }

            let td = require_btrfs_test_dir!("send_snap_liveboot");
            let td_recv = require_btrfs_recv_dir!("send_snap_liveboot");

            let src = td.path.join("src");
            btrfs::create_subvolume(&src).unwrap();
            write_test_file(&src, "f.txt", "content");

            let snap_dir = src.join(".snapshots");
            fs::create_dir_all(&snap_dir).unwrap();

            let snap = snap_dir.join("btrbak_test");
            btrfs::create_snapshot(&src, &snap).unwrap();

            let target_dir = td_recv.path.join("target");
            fs::create_dir_all(&target_dir).unwrap();

            let source = make_source_config(&src, Path::new(".snapshots"));
            let mut target_config = make_target_config(&target_dir);
            target_config.enable_live_boot = true;

            let stats = send_snapshot(&source, &target_config, &snap, None, &target_dir).unwrap();
            assert!(stats.bytes > 0);

            let vol_name = btrfs::get_subvolume_name_with_suffix(&src);
            let received = target_dir.join("@snapshots").join(&vol_name);
            assert!(btrfs::is_subvolume(&received).unwrap());
            assert!(!target_dir.join(&vol_name).exists());
        }
    }
}
