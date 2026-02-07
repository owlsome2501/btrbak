use btrbak::{BackupError, Cli, Config, backup, ui};
use clap::Parser;
use std::process;

fn run() -> Result<(), BackupError> {
    let cli = Cli::parse();
    ui::init(cli.verbose, cli.quiet);

    match cli.command {
        btrbak::cli::Commands::Backup {
            config,
            dry_run,
            privileged_mode,
        } => {
            backup::run_backup(&config, dry_run, privileged_mode)?;
        }
        btrbak::cli::Commands::PrepareLive {
            config,
            privileged_mode,
        } => {
            backup::prepare_live_environment(&config, privileged_mode)?;
        }
        btrbak::cli::Commands::Validate { config } => {
            let config = Config::from_file(&config)?;
            config.validate()?;
            ui::success("Configuration is valid");
        }
    }

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        ui::error_with_hints(&e.to_string(), &e.hints());
        process::exit(1);
    }
}
