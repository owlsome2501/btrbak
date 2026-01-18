use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "btrbak")]
#[command(about = "Btrfs subvolume backup with live boot environment support")]
pub struct Cli {
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
