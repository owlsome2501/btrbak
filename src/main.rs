use backup_btrfs::{BackupError, Cli, Config, backup};
use clap::Parser;

fn main() -> Result<(), BackupError> {
    env_logger::init();

    let cli = Cli::parse();

    match cli.command {
        backup_btrfs::cli::Commands::Backup { config, dry_run } => {
            backup::run_backup(&config, dry_run)?;
        }
        backup_btrfs::cli::Commands::PrepareLive { config } => {
            backup::prepare_live_environment(&config)?;
        }
        backup_btrfs::cli::Commands::ListSnapshots { config } => {
            backup::list_snapshots(&config)?;
        }
        backup_btrfs::cli::Commands::Validate { config } => {
            let config = Config::from_file(&config)?;
            config.validate()?;
            println!("Configuration is valid.");
        }
    }

    Ok(())
}
