use anyhow;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use toml;

use crate::btrfs;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Source subvolume configurations
    #[serde(alias = "source")]
    pub sources: Vec<SourceConfig>,
    /// Target backup location configuration
    pub target: TargetConfig,
    /// Live boot environment configuration (optional)
    #[serde(default)]
    pub live_boot: Option<LiveBootConfig>,
    /// Hook configuration
    #[serde(default)]
    pub hooks: HookConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceConfig {
    /// Path to the source subvolume (must be a btrfs subvolume)
    pub path: PathBuf,
    /// Path to the snapshot directory (defaults to .snapshots in the source)
    #[serde(default = "default_snapshot_dir")]
    pub snapshot_dir: PathBuf,
    /// Whether to use snapper for creating snapshots
    #[serde(default)]
    pub use_snapper: bool,
    /// Name of the snapshot subvolume (if not using snapper)
    #[serde(default = "default_snapshot_name")]
    pub snapshot_name: String,
    /// Snapper configuration name (required if use_snapper is true)
    #[serde(default)]
    pub snapper_config: Option<String>,
}

fn default_snapshot_dir() -> PathBuf {
    PathBuf::from(".snapshots")
}

fn default_snapshot_name() -> String {
    "btrbak".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TargetConfig {
    /// Either a mounted path or device identifier
    pub location: TargetLocation,
    /// Whether to enable live boot environment
    #[serde(default)]
    pub enable_live_boot: bool,
    /// Subvolume name for snapshots (default: "@snapshots" if live boot enabled, else ".")
    #[serde(default)]
    pub snapshot_subvolume: Option<String>,
    /// Subvolume name for live boot root (default: "@")
    #[serde(default)]
    pub live_root_subvolume: Option<String>,
    /// Encryption configuration for target device (optional)
    #[serde(default)]
    pub encryption: Option<EncryptionConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EncryptionConfig {
    /// Path to keyfile for LUKS encryption (optional, if not provided, will prompt for passphrase)
    pub keyfile: Option<PathBuf>,
    /// Name of environment variable containing passphrase (optional)
    pub passphrase_env: Option<String>,
    /// Custom name for LUKS mapping (defaults to "backup_target")
    #[serde(default = "default_luks_mapping_name")]
    pub mapping_name: String,
}

fn default_luks_mapping_name() -> String {
    "backup_target".to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum TargetLocation {
    /// Already mounted filesystem path
    MountedPath(PathBuf),
    /// Device identifier (by UUID, LABEL, or path)
    Device(String),
}

impl<'de> serde::Deserialize<'de> for TargetLocation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        // Heuristic to determine if string is a device identifier
        if s.starts_with("/dev/")
            || s.starts_with("UUID=")
            || s.starts_with("LABEL=")
            || s.starts_with("PARTUUID=")
        {
            Ok(TargetLocation::Device(s))
        } else {
            // Assume it's a mounted path
            Ok(TargetLocation::MountedPath(PathBuf::from(s)))
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LiveBootConfig {
    /// Path to ESP (EFI System Partition)
    pub esp_path: PathBuf,
    /// Bootloader type (currently only systemd-boot supported)
    #[serde(default = "default_bootloader")]
    pub bootloader: BootloaderType,
    /// Bootloader entry configuration
    #[serde(default)]
    pub boot_entry: BootEntryConfig,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub enum BootloaderType {
    #[default]
    SystemdBoot,
}

fn default_bootloader() -> BootloaderType {
    BootloaderType::SystemdBoot
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BootEntryConfig {
    /// Title for boot menu
    #[serde(default = "default_title")]
    pub title: String,
    /// Kernel path (relative to live boot root)
    #[serde(default = "default_kernel")]
    pub kernel: PathBuf,
    /// Initramfs path (relative to live boot root)
    #[serde(default = "default_initramfs")]
    pub initramfs: PathBuf,
    /// Additional kernel command line options
    #[serde(default)]
    pub options: Vec<String>,
}

fn default_title() -> String {
    "Backup Environment".to_string()
}

fn default_kernel() -> PathBuf {
    PathBuf::from("/boot/vmlinuz-linux")
}

fn default_initramfs() -> PathBuf {
    PathBuf::from("/boot/initramfs-linux.img")
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HookConfig {
    /// Copy kernel and initramfs to ESP after backup
    #[serde(default = "default_true")]
    pub copy_kernel: bool,
    /// Regenerate fstab in live boot environment
    #[serde(default = "default_true")]
    pub regenerate_fstab: bool,
    /// Remove snapper configuration from live boot environment
    #[serde(default = "default_true")]
    pub remove_snapper_config: bool,
}

fn default_true() -> bool {
    true
}

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file(path: &PathBuf) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Validate configuration consistency
    pub fn validate(&self) -> anyhow::Result<()> {
        // Ensure we have at least one source
        if self.sources.is_empty() {
            anyhow::bail!("No source configurations provided");
        }

        // Validate each source
        for source in &self.sources {
            // Ensure source path exists and is a btrfs subvolume
            if !source.path.exists() {
                anyhow::bail!("Source path does not exist: {:?}", source.path);
            }

            // Check if source path is a btrfs subvolume
            match btrfs::is_subvolume(&source.path) {
                Ok(true) => {} // Success
                Ok(false) => {
                    anyhow::bail!("Source path is not a btrfs subvolume: {:?}", source.path);
                }
                Err(e) => {
                    anyhow::bail!(
                        "Failed to check btrfs subvolume status: {}. Ensure btrfs tools are installed.",
                        e
                    );
                }
            }

            // If using snapper, snapper_config must be set
            if source.use_snapper && source.snapper_config.is_none() {
                anyhow::bail!(
                    "snapper_config must be set when use_snapper is true for source: {:?}",
                    source.path
                );
            }
        }

        // If target is a mounted path, ensure it exists
        match &self.target.location {
            TargetLocation::MountedPath(p) => {
                if !p.exists() {
                    anyhow::bail!("Target mounted path does not exist: {:?}", p);
                }
                // Check if mounted path is a btrfs filesystem
                match btrfs::is_btrfs_filesystem(p) {
                    Ok(true) => {} // Success
                    Ok(false) => {
                        anyhow::bail!("Target mounted path is not a btrfs filesystem: {:?}", p);
                    }
                    Err(e) => {
                        anyhow::bail!(
                            "Failed to check btrfs filesystem status: {}. Ensure btrfs tools are installed.",
                            e
                        );
                    }
                }
            }
            TargetLocation::Device(_) => {
                // Device will be mounted later
            }
        }

        // If live boot enabled, ensure configuration is provided
        if self.target.enable_live_boot && self.live_boot.is_none() {
            anyhow::bail!("Live boot enabled but live_boot configuration missing");
        }

        // If live boot configuration exists, validate ESP path
        if let Some(live_boot) = &self.live_boot
            && !live_boot.esp_path.exists()
        {
            anyhow::bail!("ESP path does not exist: {:?}", live_boot.esp_path);
        }

        // Validate encryption configuration if present
        if let Some(encryption) = &self.target.encryption {
            if let Some(keyfile) = &encryption.keyfile
                && !keyfile.exists()
            {
                anyhow::bail!("Encryption keyfile does not exist: {:?}", keyfile);
            }
            // If neither keyfile nor passphrase_env is provided, that's an error
            if encryption.keyfile.is_none() && encryption.passphrase_env.is_none() {
                anyhow::bail!(
                    "Encryption configured but neither keyfile nor passphrase_env provided"
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_config_defaults() {
        let config = Config {
            sources: vec![SourceConfig {
                path: PathBuf::from("/test"),
                snapshot_dir: PathBuf::from(".snapshots"),
                use_snapper: false,
                snapshot_name: "btrbak".to_string(),
                snapper_config: None,
            }],
            target: TargetConfig {
                location: TargetLocation::MountedPath(PathBuf::from("/mnt")),
                enable_live_boot: false,
                snapshot_subvolume: None,
                live_root_subvolume: None,
                encryption: None,
            },
            live_boot: None,
            hooks: HookConfig::default(),
        };

        assert_eq!(config.sources[0].snapshot_dir, PathBuf::from(".snapshots"));
        assert_eq!(config.sources[0].snapshot_name, "btrbak");
        assert!(!config.sources[0].use_snapper);
    }

    #[test]
    fn test_config_deserialize() {
        let toml_content = r#"
            [[sources]]
            path = "/home"
            use_snapper = false
            snapshot_name = "test_snapshot"

            [target]
            location = "/mnt/backup"
            enable_live_boot = false
        "#;

        let config: Result<Config, _> = toml::from_str(toml_content);
        assert!(config.is_ok());
        let config = config.unwrap();
        assert_eq!(config.sources.len(), 1);
        assert_eq!(config.sources[0].path, PathBuf::from("/home"));
        assert_eq!(config.sources[0].snapshot_name, "test_snapshot");
        assert!(!config.sources[0].use_snapper);
        match config.target.location {
            TargetLocation::MountedPath(ref p) => assert_eq!(p, &PathBuf::from("/mnt/backup")),
            TargetLocation::Device(_) => panic!("Expected MountedPath"),
        }
    }

    #[test]
    fn test_encryption_config_deserialize() {
        let toml_content = r#"
            [[sources]]
            path = "/home"

            [target]
            location = "/dev/sda1"
            enable_live_boot = false
            
            [target.encryption]
            keyfile = "/path/to/keyfile"
            passphrase_env = "BACKUP_PASSPHRASE"
            mapping_name = "custom_mapping"
        "#;

        let config: Result<Config, _> = toml::from_str(toml_content);
        assert!(config.is_ok());
        let config = config.unwrap();
        let encryption = config.target.encryption.unwrap();
        assert_eq!(encryption.keyfile, Some(PathBuf::from("/path/to/keyfile")));
        assert_eq!(
            encryption.passphrase_env,
            Some("BACKUP_PASSPHRASE".to_string())
        );
        assert_eq!(encryption.mapping_name, "custom_mapping");
    }

    #[test]
    fn test_config_from_file() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let toml_content = r#"
            [[sources]]
            path = "/test"
            use_snapper = false

            [target]
            location = "/mnt"
            enable_live_boot = false
        "#;
        temp_file.write_all(toml_content.as_bytes()).unwrap();

        let config = Config::from_file(&temp_file.path().to_path_buf());
        assert!(config.is_ok());
    }
}
