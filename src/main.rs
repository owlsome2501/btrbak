use btrbak::{BackupError, Cli, Config, backup};
use clap::Parser;

fn main() -> Result<(), BackupError> {
    env_logger::init();

    let cli = Cli::parse();

    match cli.command {
        btrbak::cli::Commands::Backup { config, dry_run } => {
            backup::run_backup(&config, dry_run)?;
        }
        btrbak::cli::Commands::PrepareLive { config } => {
            backup::prepare_live_environment(&config)?;
        }
        btrbak::cli::Commands::ListSnapshots { config } => {
            backup::list_snapshots(&config)?;
        }
        btrbak::cli::Commands::Validate { config } => {
            let config = Config::from_file(&config)?;
            config.validate()?;
            println!("Configuration is valid.");
        }
    }

    Ok(())
}
