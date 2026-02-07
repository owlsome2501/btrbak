use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "btrbak")]
#[command(about = "Btrfs subvolume backup with live boot environment support")]
pub struct Cli {
    /// Enable verbose output (show all details)
    #[arg(short, long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Suppress all output except errors
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run backup according to configuration file
    Backup {
        /// Path to configuration file (TOML)
        #[arg(short, long, default_value = "btrbak.toml")]
        config: PathBuf,

        /// Dry run: show what would be done without making changes
        #[arg(long)]
        dry_run: bool,

        /// Use privileged system tools (`cryptsetup`, `mount`, `umount`) instead of user-space tools
        #[arg(long)]
        privileged_mode: bool,
    },

    /// Prepare live boot environment (initialize subvolumes and bootloader)
    PrepareLive {
        /// Path to configuration file (TOML)
        #[arg(short, long, default_value = "btrbak.toml")]
        config: PathBuf,

        /// Use privileged system tools (`cryptsetup`, `mount`, `umount`) instead of user-space tools
        #[arg(long)]
        privileged_mode: bool,
    },

    /// Validate configuration file
    Validate {
        /// Path to configuration file (TOML)
        #[arg(short, long, default_value = "btrbak.toml")]
        config: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_backup_privileged_mode_default_false() {
        let cli = Cli::parse_from(["btrbak", "backup"]);
        match cli.command {
            Commands::Backup {
                privileged_mode, ..
            } => {
                assert!(!privileged_mode);
            }
            _ => panic!("expected backup command"),
        }
    }

    #[test]
    fn test_backup_privileged_mode_true_when_set() {
        let cli = Cli::parse_from(["btrbak", "backup", "--privileged-mode"]);
        match cli.command {
            Commands::Backup {
                privileged_mode, ..
            } => {
                assert!(privileged_mode);
            }
            _ => panic!("expected backup command"),
        }
    }

    #[test]
    fn test_prepare_live_privileged_mode_true_when_set() {
        let cli = Cli::parse_from(["btrbak", "prepare-live", "--privileged-mode"]);
        match cli.command {
            Commands::PrepareLive {
                privileged_mode, ..
            } => {
                assert!(privileged_mode);
            }
            _ => panic!("expected prepare-live command"),
        }
    }
}
