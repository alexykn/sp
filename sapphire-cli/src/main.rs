// src/main.rs
use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use sapphire_core::utils::config::Config;
use std::process;

// Added imports for auto-update
use std::env;
use std::fs;
use std::time::{Duration, SystemTime};
use sapphire_core::utils::error::Result as SapphireResult; // Alias to avoid clash
use sapphire_core::utils::cache::Cache;
use std::sync::Arc;

mod cli; // For argument parsing (Cli struct, Commands enum)
mod cmd; // For command implementations (run functions)
mod ui; // <-- ADDED: UI utilities module

use cli::{Cli, Commands}; // Import the structs/enums from the cli module


/// Checks if auto-update is needed and runs it.
async fn check_and_run_auto_update(config: &Config, cache: &Arc<Cache>) -> SapphireResult<()> {
    // 1. Check if auto-update is disabled
    if env::var("SAPPHIRE_NO_AUTO_UPDATE").map_or(false, |v| v == "1") {
        log::debug!("Auto-update disabled via SAPPHIRE_NO_AUTO_UPDATE=1.");
        return Ok(());
    }

    // 2. Determine update interval
    let default_interval_secs: u64 = 86400; // 24 hours
    let update_interval_secs = env::var("SAPPHIRE_AUTO_UPDATE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(default_interval_secs);
    let update_interval = Duration::from_secs(update_interval_secs);
    log::debug!("Auto-update interval: {:?}", update_interval);


    // 3. Check timestamp file
    let timestamp_file = config.cache_dir.join(".sapphire_last_update_check");
    log::debug!("Checking timestamp file: {}", timestamp_file.display());

    let mut needs_update = true; // Assume update needed unless file is recent
    if let Ok(metadata) = fs::metadata(&timestamp_file) {
        if let Ok(modified_time) = metadata.modified() {
             match SystemTime::now().duration_since(modified_time) {
                 Ok(age) => {
                    log::debug!("Time since last update check: {:?}", age);
                    if age < update_interval {
                        needs_update = false;
                        log::debug!("Auto-update interval not yet passed.");
                    } else {
                         log::debug!("Auto-update interval passed.");
                    }
                 }
                 Err(e) => {
                     log::warn!("Could not get duration since last update check (system time error?): {}", e);
                     // Proceed with update if we can't determine age
                 }
             }
        } else {
            log::warn!("Could not read modification time for timestamp file: {}", timestamp_file.display());
             // Proceed with update if we can't read time
        }
    } else {
        log::debug!("Timestamp file not found or not accessible.");
        // Proceed with update if file doesn't exist
    }

    // 4. Run update if needed
    if needs_update {
        log::info!("Running auto-update...");
        // Use the existing update command logic
        match cmd::update::run_update(config, cache).await { // Pass Arc::clone if needed, depends on run_update signature
             Ok(_) => {
                 log::info!("Auto-update successful.");
                 // 5. Update timestamp file on success
                 match fs::File::create(&timestamp_file) {
                    Ok(_) => {
                         log::debug!("Updated timestamp file: {}", timestamp_file.display());
                    }
                    Err(e) => {
                         log::warn!("Failed to create or update timestamp file '{}': {}", timestamp_file.display(), e);
                         // Continue even if timestamp update fails, but log it
                    }
                 }
             }
             Err(e) => {
                 // Log error but don't prevent the main command from running
                 log::error!("Auto-update failed: {}", e);
             }
         }
    } else {
        log::debug!("Skipping auto-update.");
    }

    Ok(())
}


#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments using the Cli struct
    let cli_args = Cli::parse();

    // Initialize logger based on verbosity (default to info)
    let log_level = match cli_args.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    env_logger::Builder::from_env(
        env_logger::Env::default().filter_or("SAPPHIRE_LOG", log_level)
    )
    .format_timestamp(None)
    .init();

    // Initialize config *before* auto-update check
    let config = Config::load().unwrap_or_else(|e| {
        eprintln!("{}: Could not load config: {}", "Error".red().bold(), e);
        process::exit(1);
    });

    // Create Cache once and wrap in Arc
    let cache = Arc::new(
        Cache::new(&config.cache_dir)
            .map_err(|e| anyhow::anyhow!("Could not initialize cache: {}", e))?
    );

    let needs_update_check = matches!(cli_args.command,
        Commands::Install(_) | Commands::Search { .. } | Commands::Info { .. }
        // Add Commands::Upgrade here when implemented
        // Note: Uninstall { .. } is intentionally excluded
    );

    if needs_update_check {
        if let Err(e) = check_and_run_auto_update(&config, &cache).await {
            // Log the error from the check itself, but don't exit
             log::error!("Error during auto-update check: {}", e);
        }
    } else {
        log::debug!("Skipping auto-update check for command: {:?}", cli_args.command);
    }


    // Run the requested command
    let command_result = match cli_args.command {
        Commands::Install(args) => cmd::install::execute(&args, &config, Arc::clone(&cache)).await,
        // Modified call to pass the vector `names`
        Commands::Uninstall { names } => cmd::uninstall::run_uninstall(&names, &config, Arc::clone(&cache)).await,
        Commands::Update => cmd::update::run_update(&config, &Arc::clone(&cache)).await, // User-invoked update still runs normally
        Commands::Search {
            query,
            formula,
            cask,
        } => {
            // Determine search type based on flags
            let search_type = if formula {
                cmd::search::SearchType::Formula
            } else if cask {
                cmd::search::SearchType::Cask
            } else {
                cmd::search::SearchType::All
            };
            cmd::search::run_search(&query, search_type, &config, &Arc::clone(&cache)).await
        }
        Commands::Info { name, cask } => cmd::info::run_info(&name, cask, &config, &Arc::clone(&cache)).await,
        //Commands::Upgrade => {
        //    cmd::upgrade::run_upgrade().await
        //}
    };

    // Handle potential errors from command execution
    if let Err(e) = command_result {
        // Use eprintln for errors that should be visible to the user
        eprintln!("{}: {:#}", "Error".red().bold(), e);
        process::exit(1);
    }

    Ok(())
}