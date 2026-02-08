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
    },

    /// Prepare live boot environment (initialize subvolumes and bootloader)
    PrepareLive {
        /// Path to configuration file (TOML)
        #[arg(short, long, default_value = "btrbak.toml")]
        config: PathBuf,
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
    fn test_backup_accepts_default_flags() {
        let cli = Cli::parse_from(["btrbak", "backup"]);
        match cli.command {
            Commands::Backup { .. } => {}
            _ => panic!("expected backup command"),
        }
    }

    #[test]
    fn test_backup_rejects_removed_privileged_mode_flag() {
        let cli = Cli::try_parse_from(["btrbak", "backup", "--privileged-mode"]);
        assert!(cli.is_err());
    }

    #[test]
    fn test_prepare_live_accepts_default_flags() {
        let cli = Cli::parse_from(["btrbak", "prepare-live"]);
        match cli.command {
            Commands::PrepareLive { .. } => {}
            _ => panic!("expected prepare-live command"),
        }
    }

    #[test]
    fn test_prepare_live_rejects_removed_privileged_mode_flag() {
        let cli = Cli::try_parse_from(["btrbak", "prepare-live", "--privileged-mode"]);
        assert!(cli.is_err());
    }
}
