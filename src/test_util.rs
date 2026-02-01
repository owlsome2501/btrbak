use crate::config::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// RAII guard that creates a unique subdirectory under a btrfs test filesystem
/// and recursively cleans up all btrfs subvolumes + files on drop.
pub struct BtrfsTestDir {
    pub path: PathBuf,
}

impl BtrfsTestDir {
    /// Create a test directory on the **source** filesystem (`BTRBAK_TEST_BTRFS_DIR`).
    /// Returns `None` if the env var is not set (caller should skip).
    pub fn new(test_name: &str) -> Option<Self> {
        Self::from_env("BTRBAK_TEST_BTRFS_DIR", test_name)
    }

    /// Create a test directory on the **receive / target** filesystem
    /// (`BTRBAK_TEST_BTRFS_RECV_DIR`).  Must be a *different* btrfs filesystem
    /// from `BTRBAK_TEST_BTRFS_DIR` so that send/receive crosses filesystem
    /// boundaries, matching real backup scenarios.
    pub fn new_recv(test_name: &str) -> Option<Self> {
        Self::from_env("BTRBAK_TEST_BTRFS_RECV_DIR", test_name)
    }

    fn from_env(env_var: &str, test_name: &str) -> Option<Self> {
        let base = match std::env::var(env_var) {
            Ok(v) if !v.is_empty() => PathBuf::from(v),
            _ => return None,
        };

        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = base.join(format!("{}_{}_{}", test_name, pid, seq));
        std::fs::create_dir_all(&dir).expect("failed to create test directory");
        Some(Self { path: dir })
    }
}

impl Drop for BtrfsTestDir {
    fn drop(&mut self) {
        // Best-effort deep cleanup: delete subvolumes depth-first, then remove_dir_all.
        let _ = cleanup_recursive(&self.path);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Depth-first traversal: delete all btrfs subvolumes under `dir`, then
/// remove regular files/dirs.  Uses the `crate::btrfs` wrappers instead of
/// shelling out directly.
fn cleanup_recursive(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    let entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();

    for entry in entries {
        if entry.is_dir() {
            // Recurse first (depth-first)
            let _ = cleanup_recursive(&entry);

            // Try to delete as subvolume via the btrfs module
            if crate::btrfs::is_subvolume(&entry).unwrap_or(false)
                && crate::btrfs::delete_subvolume(&entry).is_ok()
            {
                continue;
            }
            // Fall back to plain directory removal
            let _ = std::fs::remove_dir_all(&entry);
        } else {
            let _ = std::fs::remove_file(&entry);
        }
    }
    Ok(())
}

/// Skip a test when the **source** btrfs test directory is not configured.
macro_rules! require_btrfs_test_dir {
    ($name:expr) => {
        match crate::test_util::BtrfsTestDir::new($name) {
            Some(td) => td,
            None => {
                eprintln!("Skipped: BTRBAK_TEST_BTRFS_DIR not set");
                return;
            }
        }
    };
}
pub(crate) use require_btrfs_test_dir;

/// Skip a test when the **receive / target** btrfs test directory is not
/// configured.  This must point to a *different* btrfs filesystem from
/// `BTRBAK_TEST_BTRFS_DIR`.
macro_rules! require_btrfs_recv_dir {
    ($name:expr) => {
        match crate::test_util::BtrfsTestDir::new_recv($name) {
            Some(td) => td,
            None => {
                eprintln!("Skipped: BTRBAK_TEST_BTRFS_RECV_DIR not set");
                return;
            }
        }
    };
}
pub(crate) use require_btrfs_recv_dir;

/// Build a `SourceConfig` suitable for tests.
pub fn make_source_config(path: &Path, snapshot_dir: &Path) -> SourceConfig {
    SourceConfig {
        path: path.to_path_buf(),
        snapshot_dir: snapshot_dir.to_path_buf(),
        use_snapper: false,
        snapshot_name: "btrbak".to_string(),
        snapper_config: None,
    }
}

/// Build a `TargetConfig` with `MountedPath` location.
pub fn make_target_config(path: &Path) -> TargetConfig {
    TargetConfig {
        location: TargetLocation::MountedPath(path.to_path_buf()),
        enable_live_boot: false,
        snapshot_subvolume: None,
        live_root_subvolume: None,
        encryption: None,
    }
}

/// Write a small file inside `dir`.
pub fn write_test_file(dir: &Path, name: &str, content: &str) {
    let p = dir.join(name);
    std::fs::write(&p, content).expect("failed to write test file");
}
