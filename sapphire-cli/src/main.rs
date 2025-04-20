use std::sync::Arc;
use std::time::{Duration, SystemTime};
use std::{env, fs, process};

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::Result as SapphireResult; // Alias to avoid clash

mod cli;
mod ui;

use cli::{CliArgs, Command};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let cli_args = CliArgs::parse();

    // Initialize logger based on verbosity (default to info)
    let level_filter = match cli_args.verbose {
        0 => LevelFilter::INFO,
        1 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };
    let env_filter = EnvFilter::builder()
        .with_default_directive(level_filter.into())
        .with_env_var("SAPPHIRE_LOG")
        .from_env_lossy();

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .without_time()
        .init();

    // Initialize config *before* auto-update check
    let config = Config::load().unwrap_or_else(|e| {
        eprintln!("{}: Could not load config: {}", "Error".red().bold(), e);
        process::exit(1);
    });

    // Create Cache once and wrap in Arc
    let cache = Arc::new(
        Cache::new(&config.cache_dir)
            .map_err(|e| anyhow::anyhow!("Could not initialize cache: {}", e))?,
    );

    let needs_update_check = matches!(
        cli_args.command,
        // Add Commands::Upgrade here when implemented. Note: Uninstall is intentionally excluded
        Command::Install(_) | Command::Search { .. } | Command::Info { .. }
    );

    if needs_update_check {
        if let Err(e) = check_and_run_auto_update(&config, Arc::clone(&cache)).await {
            // Log the error from the check itself, but don't exit
            tracing::error!("Error during auto-update check: {}", e);
        }
    } else {
        tracing::debug!(
            "Skipping auto-update check for command: {:?}",
            cli_args.command
        );
    }

    if let Err(e) = cli_args.command.run(&config, cache).await {
        eprintln!("{}: {:#}", "Error".red().bold(), e);
        process::exit(1);
    }

    Ok(())
}

/// Checks if auto-update is needed and runs it.
async fn check_and_run_auto_update(config: &Config, cache: Arc<Cache>) -> SapphireResult<()> {
    // 1. Check if auto-update is disabled
    if env::var("SAPPHIRE_NO_AUTO_UPDATE").map_or(false, |v| v == "1") {
        tracing::debug!("Auto-update disabled via SAPPHIRE_NO_AUTO_UPDATE=1.");
        return Ok(());
    }

    // 2. Determine update interval
    let default_interval_secs: u64 = 86400; // 24 hours
    let update_interval_secs = env::var("SAPPHIRE_AUTO_UPDATE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(default_interval_secs);
    let update_interval = Duration::from_secs(update_interval_secs);
    tracing::debug!("Auto-update interval: {:?}", update_interval);

    // 3. Check timestamp file
    let timestamp_file = config.cache_dir.join(".sapphire_last_update_check");
    tracing::debug!("Checking timestamp file: {}", timestamp_file.display());

    let mut needs_update = true; // Assume update needed unless file is recent
    if let Ok(metadata) = fs::metadata(&timestamp_file) {
        if let Ok(modified_time) = metadata.modified() {
            match SystemTime::now().duration_since(modified_time) {
                Ok(age) => {
                    tracing::debug!("Time since last update check: {:?}", age);
                    if age < update_interval {
                        needs_update = false;
                        tracing::debug!("Auto-update interval not yet passed.");
                    } else {
                        tracing::debug!("Auto-update interval passed.");
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Could not get duration since last update check (system time error?): {}",
                        e
                    );
                    // Proceed with update if we can't determine age
                }
            }
        } else {
            tracing::warn!(
                "Could not read modification time for timestamp file: {}",
                timestamp_file.display()
            );
            // Proceed with update if we can't read time
        }
    } else {
        tracing::debug!("Timestamp file not found or not accessible.");
        // Proceed with update if file doesn't exist
    }

    // 4. Run update if needed
    if needs_update {
        println!("Running auto-update...");
        // Use the existing update command logic
        match cli::update::Update.run(config, cache).await {
            // Pass Arc::clone if needed, depends on run_update signature
            Ok(_) => {
                println!("Auto-update successful.");
                // 5. Update timestamp file on success
                match fs::File::create(&timestamp_file) {
                    Ok(_) => {
                        tracing::debug!("Updated timestamp file: {}", timestamp_file.display());
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to create or update timestamp file '{}': {}",
                            timestamp_file.display(),
                            e
                        );
                        // Continue even if timestamp update fails, but log it
                    }
                }
            }
            Err(e) => {
                // Log error but don't prevent the main command from running
                tracing::error!("Auto-update failed: {}", e);
            }
        }
    } else {
        tracing::debug!("Skipping auto-update.");
    }

    Ok(())
}
