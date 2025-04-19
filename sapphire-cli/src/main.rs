// src/main.rs
use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use sapphire_core::utils::config::Config;
use std::process;

mod cli; // For argument parsing (Cli struct, Commands enum)
mod cmd; // For command implementations (run functions)

use cli::{Cli, Commands}; // Import the structs/enums from the cli module

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

    // Initialize config
    let config = Config::load().unwrap_or_else(|e| {
        eprintln!("{}: Could not load config: {}", "Error".red().bold(), e);
        process::exit(1);
    });


    // Run the requested command
    let command_result = match cli_args.command {
        Commands::Install(args) => cmd::install::execute(&args, &config).await,
        // Modified call to pass the vector `names`
        Commands::Uninstall { names } => cmd::uninstall::run_uninstall(&names).await,
        Commands::Update => cmd::update::run_update().await,
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
            cmd::search::run_search(&query, search_type).await
        }
        Commands::Info { name, cask } => cmd::info::run_info(&name, cask).await,
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