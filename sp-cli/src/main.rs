// sp-cli/src/main.rs
// Corrected logging setup for file output.

use std::sync::Arc;
use std::time::{Duration, SystemTime};
use std::{env, fs, process};

use clap::Parser;
use colored::Colorize;
use sp_common::cache::Cache;
use sp_common::config::Config;
use sp_common::error::{Result as spResult, SpError};
use tracing::Level; // Import the Level type
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

mod cli;
mod ui;

use cli::{CliArgs, Command};

#[tokio::main]
async fn main() -> spResult<()> {
    let cli_args = CliArgs::parse();

    // Initialize config *before* logging setup, as we need the cache path for logs
    let config =
        Config::load().map_err(|e| SpError::Config(format!("Could not load config: {e}")))?;

    // --- Logging Setup ---
    let level_filter = match cli_args.verbose {
        0 => LevelFilter::INFO,
        1 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };

    // Convert LevelFilter to Option<Level> for use with with_max_level
    // We know INFO, DEBUG, TRACE filters correspond to Some(Level), so unwrap is safe.
    let max_log_level = level_filter.into_level().unwrap_or(Level::INFO);
    let info_level = LevelFilter::INFO.into_level().unwrap_or(Level::INFO); // INFO level specifically

    let env_filter = EnvFilter::builder()
        .with_default_directive(level_filter.into()) // Use LevelFilter for general filtering
        .with_env_var("SP_LOG") // Allow overriding via env var
        .from_env_lossy();

    // Create a logs directory if it doesn't exist
    let log_dir = config.cache_dir.join("logs");
    if let Err(e) = fs::create_dir_all(&log_dir) {
        // Log to stderr initially if log dir creation fails
        eprintln!(
            "{} Failed to create log directory {}: {}",
            "Error:".red().bold(),
            log_dir.display(),
            e
        );
        // Fallback to stderr logging
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .without_time()
            .init();
    } else {
        // Set up file logging only if verbose > 0
        if cli_args.verbose > 0 {
            let file_appender = tracing_appender::rolling::daily(&log_dir, "sp.log");
            let (non_blocking_appender, _guard) = tracing_appender::non_blocking(file_appender);

            // Log DEBUG/TRACE to file, INFO+ still goes to stderr
            // Use the converted Level type here
            let stderr_writer = std::io::stderr.with_max_level(info_level);
            let file_writer = non_blocking_appender.with_max_level(max_log_level); // Use the calculated Level

            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_writer(stderr_writer.and(file_writer)) // Combine writers
                .with_ansi(true) // Keep ANSI codes for stderr
                .without_time() // Keep time disabled for CLI feel
                .init();

            // Keep the guard alive for the duration of the program
            // Leaking is simpler for a CLI app's main function.
            Box::leak(Box::new(_guard));

            tracing::debug!(
                "Verbose logging enabled. Writing logs to: {}/sp.log",
                log_dir.display()
            );
        } else {
            // Default: INFO+ to stderr only
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_writer(std::io::stderr)
                .with_ansi(true)
                .without_time()
                .init();
        }
    }
    // --- End Logging Setup ---

    // Create Cache once and wrap in Arc (after config load)
    let cache = Arc::new(
        Cache::new(&config.cache_dir)
            .map_err(|e| SpError::Cache(format!("Could not initialize cache: {e}")))?,
    );

    let needs_update_check = matches!(
        cli_args.command,
        Command::Install(_) | Command::Search { .. } | Command::Info { .. }
    );

    if needs_update_check {
        if let Err(e) = check_and_run_auto_update(&config, Arc::clone(&cache)).await {
            tracing::error!("Error during auto-update check: {}", e);
        }
    } else {
        tracing::debug!(
            "Skipping auto-update check for command: {:?}",
            cli_args.command
        );
    }

    if let Err(e) = cli_args.command.run(&config, cache).await {
        // Log error using tracing *before* printing to stderr, so it goes to file too if verbose
        tracing::error!("Command failed: {:#}", e);
        eprintln!("{}: {:#}", "Error".red().bold(), e);
        process::exit(1);
    }

    tracing::debug!("Command completed successfully."); // Add success debug log
    Ok(())
}

// check_and_run_auto_update function remains the same
async fn check_and_run_auto_update(config: &Config, cache: Arc<Cache>) -> spResult<()> {
    // 1. Check if auto-update is disabled
    if env::var("SP_NO_AUTO_UPDATE").is_ok_and(|v| v == "1") {
        tracing::debug!("Auto-update disabled via SP_NO_AUTO_UPDATE=1.");
        return Ok(());
    }

    // 2. Determine update interval
    let default_interval_secs: u64 = 86400; // 24 hours
    let update_interval_secs = env::var("SP_AUTO_UPDATE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(default_interval_secs);
    let update_interval = Duration::from_secs(update_interval_secs);
    tracing::debug!("Auto-update interval: {:?}", update_interval);

    // 3. Check timestamp file
    let timestamp_file = config.cache_dir.join(".sp_last_update_check");
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
        println!("Running auto-update..."); // Keep user feedback on stderr
        // Use the existing update command logic
        match cli::update::Update.run(config, cache).await {
            Ok(_) => {
                println!("Auto-update successful."); // Keep user feedback on stderr
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
                eprintln!("{} Auto-update failed: {}", "Warning:".yellow(), e); // Also inform user
                // on stderr
            }
        }
    } else {
        tracing::debug!("Skipping auto-update.");
    }

    Ok(())
}
