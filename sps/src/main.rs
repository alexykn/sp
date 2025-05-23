// sps/src/main.rs
use std::process::{self}; // StdCommand is used
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use std::{env, fs};

use clap::Parser;
use colored::Colorize;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::{Result as spResult, SpsError};
use tracing::level_filters::LevelFilter;
use tracing::{debug, error, warn}; // Import all necessary tracing macros
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::EnvFilter;

mod cli;
mod pipeline;
// Correctly import InitArgs via the re-export in cli.rs or directly from its module
use cli::{CliArgs, Command, InitArgs};

// Standalone function to handle the init command logic
async fn run_init_command(init_args: &InitArgs, verbose_level: u8) -> spResult<()> {
    let init_level_filter = match verbose_level {
        0 => LevelFilter::INFO,
        1 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };
    let _ = tracing_subscriber::fmt()
        .with_max_level(init_level_filter)
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .without_time()
        .try_init();

    let initial_config_for_path = Config::load().map_err(|e| {
        // Handle error if even basic config loading fails for path determination
        SpsError::Config(format!(
            "Could not determine sps_root for init (config load failed): {e}"
        ))
    })?;

    // Create a minimal Config struct, primarily for sps_root() and derived paths.
    let temp_config_for_init = Config {
        sps_root: initial_config_for_path.sps_root().to_path_buf(),
        api_base_url: "https://formulae.brew.sh/api".to_string(),
        artifact_domain: None,
        docker_registry_token: None,
        docker_registry_basic_auth: None,
        github_api_token: None,
    };

    init_args.run(&temp_config_for_init).await
}

#[tokio::main]
async fn main() -> spResult<()> {
    let cli_args = CliArgs::parse();

    if let Command::Init(ref init_args_ref) = cli_args.command {
        match run_init_command(init_args_ref, cli_args.verbose).await {
            Ok(_) => {
                return Ok(());
            }
            Err(e) => {
                eprintln!("{}: Init command failed: {:#}", "Error".red().bold(), e);
                process::exit(1);
            }
        }
    }

    let config = Config::load().map_err(|e| {
        SpsError::Config(format!(
            "Could not load config (have you run 'sps init'?): {e}"
        ))
    })?;

    let level_filter = match cli_args.verbose {
        0 => LevelFilter::INFO,
        1 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };
    let max_log_level = level_filter.into_level().unwrap_or(tracing::Level::INFO);

    let env_filter = EnvFilter::builder()
        .with_default_directive(level_filter.into())
        .with_env_var("SPS_LOG")
        .from_env_lossy();

    let log_dir = config.logs_dir();
    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!(
            "{} Failed to create log directory {}: {} (ensure 'sps init' was successful or try with sudo for the current command if appropriate)",
            "Error:".red().bold(),
            log_dir.display(),
            e
        );
        let _ = tracing_subscriber::fmt() // Use `let _ =`
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .without_time()
            .try_init(); // Use try_init
    } else if cli_args.verbose > 0 {
        let file_appender = tracing_appender::rolling::daily(&log_dir, "sps.log");
        let (non_blocking_appender, guard) = tracing_appender::non_blocking(file_appender);

        // For verbose mode, show debug/trace logs on stderr too
        let stderr_writer = std::io::stderr.with_max_level(max_log_level);
        let file_writer = non_blocking_appender.with_max_level(max_log_level);

        let _ = tracing_subscriber::fmt() // Use `let _ =`
            .with_env_filter(env_filter)
            .with_writer(stderr_writer.and(file_writer))
            .with_ansi(true)
            .without_time()
            .try_init(); // Use try_init

        Box::leak(Box::new(guard)); // Keep guard alive

        tracing::debug!(
            // This will only work if try_init above was successful for this setup
            "Verbose logging enabled. Writing logs to: {}/sps.log",
            log_dir.display()
        );
    } else {
        let _ = tracing_subscriber::fmt() // Use `let _ =`
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .without_time()
            .try_init(); // Use try_init
    }

    let cache = Arc::new(Cache::new(&config).map_err(|e| {
        SpsError::Cache(format!(
            "Could not initialize cache (ensure 'sps init' was successful): {e}"
        ))
    })?);

    let needs_update_check = matches!(
        cli_args.command,
        Command::Install(_) | Command::Search { .. } | Command::Info { .. } | Command::Upgrade(_)
    );

    if needs_update_check {
        if let Err(e) = check_and_run_auto_update(&config, Arc::clone(&cache)).await {
            error!("Error during auto-update check: {}", e); // Use `error!` macro
        }
    } else {
        debug!(
            // Use `debug!` macro
            "Skipping auto-update check for command: {:?}",
            cli_args.command
        );
    }

    // Pass config and cache to the command's run method
    let command_execution_result = match &cli_args.command {
        Command::Init(_) => {
            /* This case is handled above and main exits */
            unreachable!()
        }
        _ => cli_args.command.run(&config, cache).await,
    };

    if let Err(e) = command_execution_result {
        error!("Command failed: {:#}", e); // Use `error!` macro
        eprintln!("{}: {:#}", "Error".red().bold(), e);
        process::exit(1);
    }

    debug!("Command completed successfully."); // Use `debug!` macro
    Ok(())
}

async fn check_and_run_auto_update(config: &Config, cache: Arc<Cache>) -> spResult<()> {
    if env::var("SPS_NO_AUTO_UPDATE").is_ok_and(|v| v == "1") {
        debug!("Auto-update disabled via SPS_NO_AUTO_UPDATE=1.");
        return Ok(());
    }

    let default_interval_secs: u64 = 86400;
    let update_interval_secs = env::var("SPS_AUTO_UPDATE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(default_interval_secs);
    let update_interval = Duration::from_secs(update_interval_secs);
    debug!("Auto-update interval: {:?}", update_interval);

    let timestamp_file = config.state_dir().join(".sps_last_update_check");
    debug!("Checking timestamp file: {}", timestamp_file.display());

    if let Some(parent_dir) = timestamp_file.parent() {
        if !parent_dir.exists() {
            if let Err(e) = fs::create_dir_all(parent_dir) {
                warn!("Could not create state directory {} as user: {}. Auto-update might rely on 'sps init' to create this with sudo.", parent_dir.display(), e);
            }
        }
    }

    let mut needs_update = true;
    if timestamp_file.exists() {
        if let Ok(metadata) = fs::metadata(&timestamp_file) {
            if let Ok(modified_time) = metadata.modified() {
                match SystemTime::now().duration_since(modified_time) {
                    Ok(age) => {
                        debug!("Time since last update check: {:?}", age);
                        if age < update_interval {
                            needs_update = false;
                            debug!("Auto-update interval not yet passed.");
                        } else {
                            debug!("Auto-update interval passed.");
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Could not get duration since last update check (system time error?): {}",
                            e
                        );
                    }
                }
            } else {
                warn!(
                    "Could not read modification time for timestamp file: {}",
                    timestamp_file.display()
                );
            }
        } else {
            debug!(
                "Timestamp file {} metadata could not be read. Update needed.",
                timestamp_file.display()
            );
        }
    } else {
        debug!(
            "Timestamp file not found at {}. Update needed.",
            timestamp_file.display()
        );
    }

    if needs_update {
        println!(
            "{}{}",
            "==> ".bold().blue(),
            "Running auto-update...".bold()
        );
        match cli::update::Update.run(config, cache).await {
            Ok(_) => {
                println!(
                    "{}{}",
                    "==> ".bold().blue(),
                    "Auto-update successful.".bold()
                );
                match fs::File::create(&timestamp_file) {
                    Ok(_) => {
                        debug!("Updated timestamp file: {}", timestamp_file.display());
                    }
                    Err(e) => {
                        warn!(
                            "Failed to create or update timestamp file '{}': {}",
                            timestamp_file.display(),
                            e
                        );
                    }
                }
            }
            Err(e) => {
                error!("Auto-update failed: {}", e);
                eprintln!("{} Auto-update failed: {}", "Warning:".yellow(), e);
            }
        }
    } else {
        debug!("Skipping auto-update.");
    }

    Ok(())
}
