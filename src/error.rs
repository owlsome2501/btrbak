use thiserror::Error;

#[derive(Error, Debug)]
pub enum BackupError {
    #[error("Configuration error: {0}")]
    Config(#[from] anyhow::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Btrfs operation failed: {0}")]
    Btrfs(String),

    #[error("Mount error: {0}")]
    Mount(String),

    #[error("Bootloader error: {0}")]
    Bootloader(String),

    #[error("Hook execution error: {0}")]
    Hook(String),

    #[error("Lock error: {0}")]
    Lock(String),
}

impl BackupError {
    pub fn hints(&self) -> Vec<&'static str> {
        match self {
            BackupError::Lock(msg) => {
                if msg.contains("already running") {
                    vec![
                        "Wait for the other btrbak instance to finish",
                        "Remove stale lock files in /tmp/btrbak_locks/ if no other instance is running",
                    ]
                } else {
                    vec!["Check that /tmp is writable"]
                }
            }
            BackupError::Btrfs(msg) => {
                let mut hints = Vec::new();
                if msg.contains("Permission denied") {
                    hints.push("Try running with sudo or as root");
                }
                if msg.contains("send") || msg.contains("Send") {
                    hints.push("Check free space on the target: 'btrfs filesystem usage <path>'");
                    hints.push("Ensure the parent snapshot still exists for incremental backup");
                }
                if msg.contains("snapshot") || msg.contains("Snapshot") {
                    hints.push("Ensure the source subvolume is accessible");
                    hints.push("Check disk space: 'btrfs filesystem usage <path>'");
                }
                if msg.contains("not a btrfs subvolume") || msg.contains("not a subvolume") {
                    hints.push("Verify the path points to a valid btrfs subvolume");
                }
                if msg.contains("receive") || msg.contains("Receive") {
                    hints.push("Check free space on the target device");
                    hints.push("Ensure the target directory exists and is on a btrfs filesystem");
                }
                if hints.is_empty() {
                    hints.push("Check that btrfs-progs is installed");
                    hints.push("Try running with sudo or as root");
                }
                hints
            }
            BackupError::Mount(msg) => {
                let mut hints = Vec::new();
                if msg.contains("LUKS") || msg.contains("luks") {
                    hints.push("Verify the device is a valid LUKS container: 'cryptsetup isLuks <device>'");
                    hints.push("Check that the keyfile exists and is readable");
                }
                if msg.contains("not a LUKS") {
                    hints.push("Ensure encryption is configured for the correct device");
                }
                if msg.contains("Failed to mount") {
                    hints.push("Check that the filesystem on the device is valid");
                    hints.push("Try mounting manually to diagnose: 'mount <device> <path>'");
                }
                if msg.contains("Failed to unmount") {
                    hints.push("Check for open files on the mount point: 'lsof <path>'");
                }
                if hints.is_empty() {
                    hints.push("Check that the device exists and is accessible");
                }
                hints
            }
            BackupError::Io(err) => {
                match err.kind() {
                    std::io::ErrorKind::PermissionDenied => {
                        vec!["Try running with sudo or as root"]
                    }
                    std::io::ErrorKind::NotFound => {
                        vec!["Check that the file or directory path is correct"]
                    }
                    std::io::ErrorKind::AlreadyExists => {
                        vec!["A file or directory already exists at the target path"]
                    }
                    _ => vec![],
                }
            }
            BackupError::Config(_) => {
                vec![
                    "Check your configuration file for syntax errors",
                    "Run 'btrbak validate -c <config>' to check the configuration",
                ]
            }
            BackupError::Bootloader(msg) => {
                let mut hints = Vec::new();
                if msg.contains("systemd-boot") || msg.contains("bootctl") {
                    hints.push("Ensure systemd-boot is installed: 'pacman -S systemd'");
                    hints.push("Check that the ESP is mounted and writable");
                }
                if hints.is_empty() {
                    hints.push("Check that the ESP path is correct and accessible");
                }
                hints
            }
            BackupError::Hook(msg) => {
                let mut hints = Vec::new();
                if msg.contains("UUID") {
                    hints.push("Ensure the target device is properly mounted");
                }
                if msg.contains("kernel") || msg.contains("initramfs") {
                    hints.push("Check that the kernel and initramfs paths in boot_entry config are correct");
                }
                if hints.is_empty() {
                    hints.push("Check the hook configuration in your config file");
                }
                hints
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let backup_error = BackupError::Io(io_error);
        assert!(backup_error.to_string().contains("IO error"));

        let btrfs_error = BackupError::Btrfs("test failure".to_string());
        assert!(btrfs_error.to_string().contains("Btrfs operation failed"));

        let mount_error = BackupError::Mount("mount failed".to_string());
        assert!(mount_error.to_string().contains("Mount error"));

        let bootloader_error = BackupError::Bootloader("bootloader failed".to_string());
        assert!(bootloader_error.to_string().contains("Bootloader error"));

        let hook_error = BackupError::Hook("hook failed".to_string());
        assert!(hook_error.to_string().contains("Hook execution error"));

        let lock_error = BackupError::Lock("lock failed".to_string());
        assert!(lock_error.to_string().contains("Lock error"));
    }

    #[test]
    fn test_error_from_io() {
        let io_error = std::io::Error::other("test");
        let backup_error: BackupError = io_error.into();
        match backup_error {
            BackupError::Io(_) => (),
            _ => panic!("Expected Io error"),
        }
    }

    #[test]
    fn test_hints_lock_already_running() {
        let err = BackupError::Lock("Another btrbak instance is already running".to_string());
        let hints = err.hints();
        assert!(!hints.is_empty());
        assert!(hints[0].contains("Wait"));
    }

    #[test]
    fn test_hints_io_permission_denied() {
        let io_error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err = BackupError::Io(io_error);
        let hints = err.hints();
        assert!(!hints.is_empty());
        assert!(hints[0].contains("sudo"));
    }

    #[test]
    fn test_hints_btrfs_send() {
        let err = BackupError::Btrfs("Failed to send subvolume".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("free space")));
    }

    #[test]
    fn test_hints_config() {
        let err = BackupError::Config(anyhow::anyhow!("bad config"));
        let hints = err.hints();
        assert!(!hints.is_empty());
        assert!(hints.iter().any(|h| h.contains("validate")));
    }

    // Btrfs hints

    #[test]
    fn test_hints_btrfs_permission_denied() {
        let err = BackupError::Btrfs("Permission denied on /mnt".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("sudo")));
    }

    #[test]
    fn test_hints_btrfs_snapshot() {
        let err = BackupError::Btrfs("Failed to create snapshot".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("subvolume is accessible")));
    }

    #[test]
    fn test_hints_btrfs_not_subvolume() {
        let err = BackupError::Btrfs("Path is not a btrfs subvolume".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("valid btrfs subvolume")));
    }

    #[test]
    fn test_hints_btrfs_receive() {
        let err = BackupError::Btrfs("Failed to receive subvolume".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("free space on the target")));
    }

    // Mount hints

    #[test]
    fn test_hints_mount_luks() {
        let err = BackupError::Mount("Failed to open LUKS device".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("cryptsetup isLuks")));
    }

    #[test]
    fn test_hints_mount_not_luks() {
        let err = BackupError::Mount("Device is not a LUKS container".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("encryption is configured")));
    }

    #[test]
    fn test_hints_mount_failed() {
        let err = BackupError::Mount("Failed to mount /dev/sda1".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("mounting manually")));
    }

    #[test]
    fn test_hints_mount_unmount_failed() {
        let err = BackupError::Mount("Failed to unmount /mnt/backup".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("lsof")));
    }

    #[test]
    fn test_hints_mount_generic() {
        let err = BackupError::Mount("some unknown mount issue".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("device exists")));
    }

    // Bootloader hints

    #[test]
    fn test_hints_bootloader_systemd_boot() {
        let err = BackupError::Bootloader("systemd-boot configuration failed".to_string());
        let hints = err.hints();
        assert!(hints
            .iter()
            .any(|h| h.contains("systemd-boot is installed")));
    }

    #[test]
    fn test_hints_bootloader_generic() {
        let err = BackupError::Bootloader("some bootloader error".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("ESP path")));
    }

    // Hook hints and other

    #[test]
    fn test_hints_hook_uuid() {
        let err = BackupError::Hook("Failed to get UUID for mount".to_string());
        let hints = err.hints();
        assert!(hints
            .iter()
            .any(|h| h.contains("target device is properly mounted")));
    }

    #[test]
    fn test_hints_hook_kernel() {
        let err = BackupError::Hook("kernel and initramfs copy failed".to_string());
        let hints = err.hints();
        assert!(hints
            .iter()
            .any(|h| h.contains("kernel and initramfs paths")));
    }

    #[test]
    fn test_hints_lock_generic() {
        let err = BackupError::Lock("failed to create lock".to_string());
        let hints = err.hints();
        assert!(hints.iter().any(|h| h.contains("/tmp is writable")));
    }
}
