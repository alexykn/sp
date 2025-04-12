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
    // Initialize logger
    env_logger::init_from_env(
        env_logger::Env::default().filter_or("SAPPHIRE_LOG", "info"),
    );

    // Initialize config
    let config = Config::load().unwrap_or_else(|e| {
        eprintln!("{}: Could not load config: {}", "Error".red().bold(), e);
        process::exit(1);
    });

    // Parse command line arguments using the Cli struct from the cli module
    let cli_args = Cli::parse();

    // Run the requested command
    let command_result = match cli_args.command {
        Commands::Install(args) => {
            cmd::install::execute(&args, &config).await
        }
        Commands::Uninstall { name } => {
            cmd::uninstall::run_uninstall(&name).await
        }
        Commands::Update => {
            cmd::update::run_update().await
        }
        Commands::Search { query, formula, cask } => {
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
        Commands::Info { name, cask } => {
            cmd::info::run_info(&name, cask).await
        }
        Commands::Upgrade => {
            cmd::upgrade::run_upgrade().await
        }
    };

    // Handle potential errors from command execution
    if let Err(e) = command_result {
        eprintln!("{}: {:#}", "Error".red().bold(), e);
        process::exit(1);
    }

    Ok(())
}
