use crate::config::*;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Probe whether we can perform btrfs subvolume operations on `base`.
/// Tries to create and delete a temporary subvolume; returns `false` if
/// the process lacks the required privileges (typically `CAP_SYS_ADMIN`).
fn can_manage_btrfs(base: &Path) -> bool {
    let probe = base.join(".btrbak_probe");
    if crate::btrfs::create_subvolume(&probe).is_err() {
        return false;
    }
    let _ = crate::btrfs::delete_subvolume(&probe);
    true
}

/// Cached privilege check for the source and receive filesystems.
/// The probe runs at most once per env var for the entire test run.
fn can_manage_btrfs_cached(env_var: &str, base: &Path) -> bool {
    static CACHE_SRC: OnceLock<bool> = OnceLock::new();
    static CACHE_RECV: OnceLock<bool> = OnceLock::new();

    let cache = if env_var == "BTRBAK_TEST_BTRFS_DIR" {
        &CACHE_SRC
    } else {
        &CACHE_RECV
    };
    *cache.get_or_init(|| can_manage_btrfs(base))
}

/// RAII guard that creates a unique subdirectory under a btrfs test filesystem
/// and recursively cleans up all btrfs subvolumes + files on drop.
pub struct BtrfsTestDir {
    pub path: PathBuf,
}

impl BtrfsTestDir {
    /// Create a test directory on the **source** filesystem (`BTRBAK_TEST_BTRFS_DIR`).
    /// Returns `None` if the env var is not set or privileges are insufficient
    /// (caller should skip).
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

        if !can_manage_btrfs_cached(env_var, &base) {
            eprintln!(
                "Skipped: insufficient privileges for btrfs operations on {}",
                base.display()
            );
            return None;
        }

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

/// Skip a test when the **source** btrfs test directory is not available
/// (env var unset or insufficient privileges).
macro_rules! require_btrfs_test_dir {
    ($name:expr) => {
        match crate::test_util::BtrfsTestDir::new($name) {
            Some(td) => td,
            None => {
                eprintln!("Skipped: BTRBAK_TEST_BTRFS_DIR not set or insufficient privileges");
                return;
            }
        }
    };
}
pub(crate) use require_btrfs_test_dir;

/// Skip a test when the **receive / target** btrfs test directory is not
/// available (env var unset or insufficient privileges).
/// This must point to a *different* btrfs filesystem from
/// `BTRBAK_TEST_BTRFS_DIR`.
macro_rules! require_btrfs_recv_dir {
    ($name:expr) => {
        match crate::test_util::BtrfsTestDir::new_recv($name) {
            Some(td) => td,
            None => {
                eprintln!("Skipped: BTRBAK_TEST_BTRFS_RECV_DIR not set or insufficient privileges");
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
        live_boot_subvolume: None,
        encryption: None,
    }
}

/// Write a small file inside `dir`.
pub fn write_test_file(dir: &Path, name: &str, content: &str) {
    let p = dir.join(name);
    std::fs::write(&p, content).expect("failed to write test file");
}

// ── LUKS / encryption test helpers ──────────────────────────────────────

/// Check whether `cryptsetup --version` succeeds.  Cached for the process.
pub fn can_run_cryptsetup() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        std::process::Command::new("cryptsetup")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// RAII wrapper around a LUKS-backed loop device used by integration tests.
///
/// On `Drop` the helper attempts to close the dm-crypt mapping (best-effort).
pub struct LuksTestDevice {
    pub loop_device: String,
    pub keyfile: PathBuf,
    pub mapping_name: String,
}

impl LuksTestDevice {
    /// Build a `LuksTestDevice` from environment variables set by the
    /// integration test script.  Returns `None` (→ skip) when the env vars
    /// are unset or `cryptsetup` is unavailable.
    pub fn new(test_name: &str) -> Option<Self> {
        let loop_device = match std::env::var("BTRBAK_TEST_LUKS_LOOP") {
            Ok(v) if !v.is_empty() => v,
            _ => return None,
        };
        let keyfile = match std::env::var("BTRBAK_TEST_LUKS_KEYFILE") {
            Ok(v) if !v.is_empty() => PathBuf::from(v),
            _ => return None,
        };

        if !can_run_cryptsetup() {
            eprintln!("Skipped: cryptsetup not available");
            return None;
        }

        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mapping_name = format!("btrbak_test_{}_{}_{}", test_name, pid, seq);

        Some(Self {
            loop_device,
            keyfile,
            mapping_name,
        })
    }

    /// Path to the opened dm-crypt device (e.g. `/dev/mapper/btrbak_test_…`).
    pub fn mapped_device_path(&self) -> String {
        format!("/dev/mapper/{}", self.mapping_name)
    }

    /// Build an `EncryptionConfig` that uses the keyfile.
    pub fn encryption_config(&self) -> crate::config::EncryptionConfig {
        crate::config::EncryptionConfig {
            keyfile: Some(self.keyfile.clone()),
            passphrase_env: None,
            mapping_name: self.mapping_name.clone(),
        }
    }

    /// Build an `EncryptionConfig` that uses the passphrase env var.
    pub fn encryption_config_passphrase(&self) -> crate::config::EncryptionConfig {
        crate::config::EncryptionConfig {
            keyfile: None,
            passphrase_env: Some("BTRBAK_TEST_LUKS_PASSPHRASE".to_string()),
            mapping_name: self.mapping_name.clone(),
        }
    }
}

impl Drop for LuksTestDevice {
    fn drop(&mut self) {
        // Best-effort cleanup: close the LUKS mapping if it was opened.
        let _ = crate::device::close_luks_device(&self.mapping_name);
    }
}

/// Skip a test when the LUKS test device is not available.
macro_rules! require_luks_test_device {
    ($name:expr) => {
        match crate::test_util::LuksTestDevice::new($name) {
            Some(dev) => dev,
            None => {
                eprintln!("Skipped: LUKS test device not available");
                return;
            }
        }
    };
}
pub(crate) use require_luks_test_device;

/// Build a `TargetConfig` for an encrypted device.
#[allow(dead_code)]
pub fn make_encrypted_target_config(
    device: &str,
    encryption: crate::config::EncryptionConfig,
) -> crate::config::TargetConfig {
    crate::config::TargetConfig {
        location: crate::config::TargetLocation::Device(device.to_string()),
        enable_live_boot: false,
        snapshot_subvolume: None,
        live_boot_subvolume: None,
        encryption: Some(encryption),
    }
}
