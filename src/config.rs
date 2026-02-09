use anyhow;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use toml;

use crate::btrfs;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Configuration name (used to distinguish between different external storage targets)
    pub name: String,
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
    /// Subvolume name for live boot (default: "@")
    #[serde(default)]
    pub live_boot_subvolume: Option<String>,
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
    /// ESP location (mounted path or device identifier)
    pub esp_location: TargetLocation,
    /// Mount point path inside live boot root volume (e.g. "/efi")
    #[serde(default = "default_esp_path")]
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

fn default_esp_path() -> PathBuf {
    PathBuf::from("/efi")
}

impl LiveBootConfig {
    /// ESP mount directory relative to live root volume (no leading slash).
    pub fn esp_path_relative(&self) -> PathBuf {
        self.esp_path
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part),
                _ => None,
            })
            .collect()
    }

    /// ESP mount point in fstab format (always starts with '/').
    pub fn esp_mount_point(&self) -> String {
        let rel = self.esp_path_relative();
        if rel.as_os_str().is_empty() {
            "/".to_string()
        } else {
            format!("/{}", rel.display())
        }
    }

    /// Validate esp_path syntax for a mount point within root_vol.
    pub fn validate_esp_path(&self) -> anyhow::Result<()> {
        if self.esp_path.as_os_str().is_empty() {
            anyhow::bail!("live_boot.esp_path must be non-empty");
        }

        if self
            .esp_path
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            anyhow::bail!(
                "live_boot.esp_path must not contain '..': {:?}",
                self.esp_path
            );
        }

        let rel = self.esp_path_relative();
        if rel.as_os_str().is_empty() {
            anyhow::bail!(
                "live_boot.esp_path must contain at least one path segment: {:?}",
                self.esp_path
            );
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    /// Microcode image path (e.g. /boot/amd-ucode.img or /boot/intel-ucode.img)
    #[serde(default)]
    pub microcode: Option<PathBuf>,
    /// Additional kernel command line options
    #[serde(default)]
    pub options: Vec<String>,
}

impl Default for BootEntryConfig {
    fn default() -> Self {
        Self {
            title: default_title(),
            kernel: default_kernel(),
            initramfs: default_initramfs(),
            microcode: None,
            options: Vec::new(),
        }
    }
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
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Validate configuration consistency
    pub fn validate(&self) -> anyhow::Result<()> {
        // Ensure configuration name is provided and not empty
        if self.name.trim().is_empty() {
            anyhow::bail!("Configuration name must be non-empty");
        }

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

        // If live boot configuration exists, validate ESP location/path
        if let Some(live_boot) = &self.live_boot {
            live_boot.validate_esp_path()?;

            match &live_boot.esp_location {
                TargetLocation::MountedPath(path) => {
                    if !path.exists() {
                        anyhow::bail!("ESP mounted path does not exist: {:?}", path);
                    }
                }
                TargetLocation::Device(_) => {
                    // Device will be mounted later
                }
            }
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
            name: "test".to_string(),
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
                live_boot_subvolume: None,
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
            name = "test"

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
            name = "test"

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
            name = "test"

            [[sources]]
            path = "/test"
            use_snapper = false

            [target]
            location = "/mnt"
            enable_live_boot = false
        "#;
        temp_file.write_all(toml_content.as_bytes()).unwrap();

        let config = Config::from_file(temp_file.path());
        assert!(config.is_ok());
    }

    // TargetLocation deserialization tests

    #[test]
    fn test_target_location_dev_path() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/"
            [target]
            location = "/dev/sda1"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        assert!(matches!(config.target.location, TargetLocation::Device(_)));
    }

    #[test]
    fn test_target_location_uuid() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/"
            [target]
            location = "UUID=abcd-1234"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        assert!(matches!(config.target.location, TargetLocation::Device(_)));
    }

    #[test]
    fn test_target_location_label() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/"
            [target]
            location = "LABEL=backup"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        assert!(matches!(config.target.location, TargetLocation::Device(_)));
    }

    #[test]
    fn test_target_location_partuuid() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/"
            [target]
            location = "PARTUUID=abcd-1234"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        assert!(matches!(config.target.location, TargetLocation::Device(_)));
    }

    #[test]
    fn test_target_location_mounted_path() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/"
            [target]
            location = "/mnt/backup"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        match &config.target.location {
            TargetLocation::MountedPath(p) => assert_eq!(p, &PathBuf::from("/mnt/backup")),
            _ => panic!("Expected MountedPath"),
        }
    }

    #[test]
    fn test_target_location_relative_path() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/"
            [target]
            location = "backup/dir"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        match &config.target.location {
            TargetLocation::MountedPath(p) => assert_eq!(p, &PathBuf::from("backup/dir")),
            _ => panic!("Expected MountedPath"),
        }
    }

    // Multiple sources and alias tests

    #[test]
    fn test_config_multiple_sources() {
        let toml_content = r#"
            name = "multi"
            [[sources]]
            path = "/"
            [[sources]]
            path = "/home"
            [[sources]]
            path = "/var/log"
            [target]
            location = "/mnt/backup"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        assert_eq!(config.sources.len(), 3);
        assert_eq!(config.sources[0].path, PathBuf::from("/"));
        assert_eq!(config.sources[1].path, PathBuf::from("/home"));
        assert_eq!(config.sources[2].path, PathBuf::from("/var/log"));
    }

    #[test]
    fn test_config_source_alias() {
        let toml_content = r#"
            name = "alias"
            [[source]]
            path = "/home"
            [target]
            location = "/mnt/backup"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        assert_eq!(config.sources.len(), 1);
        assert_eq!(config.sources[0].path, PathBuf::from("/home"));
    }

    // Live boot configuration tests

    #[test]
    fn test_config_live_boot_full() {
        let toml_content = r#"
            name = "liveboot"
            [[sources]]
            path = "/"
            [target]
            location = "/dev/sda1"
            enable_live_boot = true
            [live_boot]
            esp_location = "/dev/sda2"
            esp_path = "/efi"
            bootloader = "SystemdBoot"
            [live_boot.boot_entry]
            title = "My Backup"
            kernel = "/boot/vmlinuz-linux"
            initramfs = "/boot/initramfs-linux.img"
            options = ["root=UUID=xxxx", "rw"]
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        let lb = config.live_boot.unwrap();
        assert!(matches!(lb.esp_location, TargetLocation::Device(_)));
        assert_eq!(lb.esp_path, PathBuf::from("/efi"));
        assert_eq!(lb.boot_entry.title, "My Backup");
        assert_eq!(lb.boot_entry.options.len(), 2);
    }

    #[test]
    fn test_config_live_boot_defaults() {
        let toml_content = r#"
            name = "liveboot"
            [[sources]]
            path = "/"
            [target]
            location = "/dev/sda1"
            [live_boot]
            esp_location = "/efi"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        let lb = config.live_boot.unwrap();
        match lb.esp_location {
            TargetLocation::MountedPath(path) => assert_eq!(path, PathBuf::from("/efi")),
            TargetLocation::Device(_) => panic!("Expected mounted path for esp_location"),
        }
        assert_eq!(lb.esp_path, PathBuf::from("/efi"));
        assert_eq!(lb.boot_entry.title, "Backup Environment");
        assert_eq!(lb.boot_entry.kernel, PathBuf::from("/boot/vmlinuz-linux"));
        assert_eq!(
            lb.boot_entry.initramfs,
            PathBuf::from("/boot/initramfs-linux.img")
        );
        assert!(lb.boot_entry.options.is_empty());
    }

    #[test]
    fn test_config_boot_entry_options() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/"
            [target]
            location = "/dev/sda1"
            [live_boot]
            esp_location = "/efi"
            [live_boot.boot_entry]
            options = ["root=UUID=abcd", "rw", "quiet"]
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        let opts = &config.live_boot.unwrap().boot_entry.options;
        assert_eq!(opts.len(), 3);
        assert_eq!(opts[0], "root=UUID=abcd");
        assert_eq!(opts[2], "quiet");
    }

    #[test]
    fn test_live_boot_esp_path_helpers() {
        let live_boot = LiveBootConfig {
            esp_location: TargetLocation::Device("/dev/sda1".to_string()),
            esp_path: PathBuf::from("/boot/efi"),
            bootloader: BootloaderType::SystemdBoot,
            boot_entry: BootEntryConfig::default(),
        };

        assert_eq!(live_boot.esp_path_relative(), PathBuf::from("boot/efi"));
        assert_eq!(live_boot.esp_mount_point(), "/boot/efi");
        live_boot.validate_esp_path().unwrap();
    }

    #[test]
    fn test_live_boot_esp_path_rejects_parent_dir() {
        let live_boot = LiveBootConfig {
            esp_location: TargetLocation::Device("/dev/sda1".to_string()),
            esp_path: PathBuf::from("/boot/../efi"),
            bootloader: BootloaderType::SystemdBoot,
            boot_entry: BootEntryConfig::default(),
        };

        assert!(live_boot.validate_esp_path().is_err());
    }

    // HookConfig and EncryptionConfig tests

    #[test]
    fn test_hook_config_defaults() {
        // When [hooks] section is present but empty, serde field defaults apply
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/"
            [target]
            location = "/mnt"
            [hooks]
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        assert!(config.hooks.copy_kernel);
        assert!(config.hooks.regenerate_fstab);
        assert!(config.hooks.remove_snapper_config);
    }

    #[test]
    fn test_hook_config_override() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/"
            [target]
            location = "/mnt"
            [hooks]
            copy_kernel = false
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        assert!(!config.hooks.copy_kernel);
        assert!(config.hooks.regenerate_fstab);
        assert!(config.hooks.remove_snapper_config);
    }

    #[test]
    fn test_encryption_default_mapping_name() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/"
            [target]
            location = "/dev/sda1"
            [target.encryption]
            keyfile = "/tmp/key"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        let enc = config.target.encryption.unwrap();
        assert_eq!(enc.mapping_name, "backup_target");
    }

    #[test]
    fn test_encryption_no_keyfile_no_env() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/"
            [target]
            location = "/dev/sda1"
            [target.encryption]
            mapping_name = "custom"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        let enc = config.target.encryption.unwrap();
        assert!(enc.keyfile.is_none());
        assert!(enc.passphrase_env.is_none());
        assert_eq!(enc.mapping_name, "custom");
    }

    // SourceConfig defaults and validation tests

    #[test]
    fn test_source_config_default_snapshot_dir() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/home"
            [target]
            location = "/mnt"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        assert_eq!(config.sources[0].snapshot_dir, PathBuf::from(".snapshots"));
    }

    #[test]
    fn test_source_config_default_snapshot_name() {
        let toml_content = r#"
            name = "test"
            [[sources]]
            path = "/home"
            [target]
            location = "/mnt"
        "#;
        let config: Config = toml::from_str(toml_content).unwrap();
        assert_eq!(config.sources[0].snapshot_name, "btrbak");
    }

    #[test]
    fn test_validate_empty_name() {
        let config = Config {
            name: "  ".to_string(),
            sources: vec![SourceConfig {
                path: PathBuf::from("/test"),
                snapshot_dir: default_snapshot_dir(),
                use_snapper: false,
                snapshot_name: default_snapshot_name(),
                snapper_config: None,
            }],
            target: TargetConfig {
                location: TargetLocation::MountedPath(PathBuf::from("/mnt")),
                enable_live_boot: false,
                snapshot_subvolume: None,
                live_boot_subvolume: None,
                encryption: None,
            },
            live_boot: None,
            hooks: HookConfig::default(),
        };
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("non-empty"));
    }

    #[test]
    fn test_validate_empty_sources() {
        let config = Config {
            name: "test".to_string(),
            sources: vec![],
            target: TargetConfig {
                location: TargetLocation::MountedPath(PathBuf::from("/mnt")),
                enable_live_boot: false,
                snapshot_subvolume: None,
                live_boot_subvolume: None,
                encryption: None,
            },
            live_boot: None,
            hooks: HookConfig::default(),
        };
        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No source configurations")
        );
    }

    #[test]
    fn test_config_from_file_nonexistent() {
        let result = Config::from_file(Path::new("/nonexistent/path/config.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_config_from_file_invalid_toml() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"this is not valid toml {{{{").unwrap();
        let result = Config::from_file(temp_file.path());
        assert!(result.is_err());
    }
}
