use btrbak::{backup, btrfs};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::NamedTempFile;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct MountedTestWorkspace {
    src_root: PathBuf,
    recv_root: PathBuf,
}

impl MountedTestWorkspace {
    fn new(test_name: &str) -> Option<Self> {
        let src_base = std::env::var("BTRBAK_TEST_BTRFS_DIR")
            .ok()
            .map(PathBuf::from)?;
        let recv_base = std::env::var("BTRBAK_TEST_BTRFS_RECV_DIR")
            .ok()
            .map(PathBuf::from)?;

        if !src_base.is_dir() || !recv_base.is_dir() {
            return None;
        }

        if !probe_btrfs_subvolume_ops(&src_base) || !probe_btrfs_subvolume_ops(&recv_base) {
            return None;
        }

        let id = format!(
            "{}_{}_{}",
            test_name,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        );

        let src_root = src_base.join(format!("it_src_{}", id));
        let recv_root = recv_base.join(format!("it_recv_{}", id));

        if fs::create_dir_all(&src_root).is_err() || fs::create_dir_all(&recv_root).is_err() {
            return None;
        }

        Some(Self {
            src_root,
            recv_root,
        })
    }
}

impl Drop for MountedTestWorkspace {
    fn drop(&mut self) {
        cleanup_recursive(&self.src_root);
        cleanup_recursive(&self.recv_root);
    }
}

fn probe_btrfs_subvolume_ops(base: &Path) -> bool {
    let probe = base.join(format!(
        ".btrbak_it_probe_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));

    if btrfs::create_subvolume(&probe).is_err() {
        return false;
    }

    btrfs::delete_subvolume(&probe).is_ok()
}

fn cleanup_recursive(path: &Path) {
    if !path.exists() {
        return;
    }

    let entries: Vec<PathBuf> = match fs::read_dir(path) {
        Ok(iter) => iter.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(_) => Vec::new(),
    };

    for entry in entries {
        if entry.is_dir() {
            cleanup_recursive(&entry);

            if btrfs::is_subvolume(&entry).unwrap_or(false)
                && btrfs::delete_subvolume(&entry).is_ok()
            {
                continue;
            }
            let _ = fs::remove_dir_all(&entry);
        } else {
            let _ = fs::remove_file(&entry);
        }
    }

    if btrfs::is_subvolume(path).unwrap_or(false) {
        let _ = btrfs::delete_subvolume(path);
    }
    let _ = fs::remove_dir_all(path);
}

fn write_backup_config(
    source_subvolume: &Path,
    target_dir: &Path,
    config_name: &str,
) -> NamedTempFile {
    let mut cfg_file = NamedTempFile::new().expect("failed to create temp config file");

    let escaped_src = source_subvolume
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let escaped_target = target_dir
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");

    let cfg = format!(
        r#"
name = "{config_name}"

[[sources]]
path = "{escaped_src}"
snapshot_dir = ".snapshots"
use_snapper = false
snapshot_name = "btrbak"

[target]
location = "{escaped_target}"
enable_live_boot = false
"#
    );

    cfg_file
        .write_all(cfg.as_bytes())
        .expect("failed to write config file");

    cfg_file
}

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

mod root_required_tests_show_ops {
    use super::*;

    #[test]
    fn test_integration_backup_dry_run_has_no_side_effects() {
        if !ensure_root_for_root_required_tests() {
            return;
        }

        let ws = match MountedTestWorkspace::new("dry_run") {
            Some(ws) => ws,
            None => {
                eprintln!("Skipped: mounted btrfs test environment not available");
                return;
            }
        };

        let source_subvolume = ws.src_root.join("src");
        btrfs::create_subvolume(&source_subvolume).expect("failed to create source subvolume");
        fs::write(source_subvolume.join("seed.txt"), "seed").expect("failed to write source data");

        let target_dir = ws.recv_root.join("target");
        fs::create_dir_all(&target_dir).expect("failed to create target dir");

        let cfg = write_backup_config(&source_subvolume, &target_dir, "it_dry_run");
        backup::run_backup(cfg.path(), true).expect("dry-run backup should succeed");

        assert!(!source_subvolume.join(".snapshots").exists());
        assert!(fs::read_dir(&target_dir).unwrap().next().is_none());
    }
}

mod root_required_tests {
    use super::*;

    #[test]
    fn test_integration_backup_full_to_mounted_target() {
        if !ensure_root_for_root_required_tests() {
            return;
        }

        let ws = match MountedTestWorkspace::new("full") {
            Some(ws) => ws,
            None => {
                eprintln!("Skipped: mounted btrfs test environment not available");
                return;
            }
        };

        let source_subvolume = ws.src_root.join("src");
        btrfs::create_subvolume(&source_subvolume).expect("failed to create source subvolume");
        fs::write(source_subvolume.join("file.txt"), "hello").expect("failed to write source data");

        let target_dir = ws.recv_root.join("target");
        fs::create_dir_all(&target_dir).expect("failed to create target dir");

        let cfg = write_backup_config(&source_subvolume, &target_dir, "it_full_backup");
        backup::run_backup(cfg.path(), false).expect("full backup failed");

        let target_vol = target_dir.join(btrfs::get_subvolume_name_with_suffix(&source_subvolume));
        assert!(btrfs::is_subvolume(&target_vol).unwrap());
        assert_eq!(
            fs::read_to_string(target_vol.join("file.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn test_integration_backup_incremental_to_mounted_target() {
        if !ensure_root_for_root_required_tests() {
            return;
        }

        let ws = match MountedTestWorkspace::new("incremental") {
            Some(ws) => ws,
            None => {
                eprintln!("Skipped: mounted btrfs test environment not available");
                return;
            }
        };

        let source_subvolume = ws.src_root.join("src");
        btrfs::create_subvolume(&source_subvolume).expect("failed to create source subvolume");
        fs::write(source_subvolume.join("v1.txt"), "v1").expect("failed to write source v1");

        let target_dir = ws.recv_root.join("target");
        fs::create_dir_all(&target_dir).expect("failed to create target dir");

        let cfg = write_backup_config(&source_subvolume, &target_dir, "it_incremental_backup");
        backup::run_backup(cfg.path(), false).expect("first backup failed");

        fs::write(source_subvolume.join("v2.txt"), "v2").expect("failed to write source v2");
        backup::run_backup(cfg.path(), false).expect("incremental backup failed");

        let target_vol = target_dir.join(btrfs::get_subvolume_name_with_suffix(&source_subvolume));
        assert!(btrfs::is_subvolume(&target_vol).unwrap());
        assert_eq!(fs::read_to_string(target_vol.join("v2.txt")).unwrap(), "v2");
    }
}
