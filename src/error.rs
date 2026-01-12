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
    }

    #[test]
    fn test_error_from_io() {
        let io_error = std::io::Error::new(std::io::ErrorKind::Other, "test");
        let backup_error: BackupError = io_error.into();
        match backup_error {
            BackupError::Io(_) => (),
            _ => panic!("Expected Io error"),
        }
    }
}
