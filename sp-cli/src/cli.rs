//! Defines the command-line argument structure using clap.
use std::sync::Arc;

use clap::{ArgAction, Parser, Subcommand};
use sp_core::utils::error::Result;
use sp_core::utils::Cache;
use sp_core::Config;

use self::info::Info;
use self::install::Install;
use self::search::Search;
use self::uninstall::Uninstall;
use self::update::Update;

pub mod info;
pub mod install;
pub mod search;
pub mod uninstall;
pub mod update;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None, name = "sp", bin_name = "sp")]
#[command(propagate_version = true)]
pub struct CliArgs {
    /// Increase verbosity (-v for debug output, -vv for trace)
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Search for available formulas and casks
    Search(Search),

    /// Display information about a formula or cask
    Info(Info),

    /// Fetch the latest package list from the API
    Update(Update),

    /// Install a formula or cask
    Install(Install),

    /// Uninstall one or more formulas or casks
    Uninstall(Uninstall),
}

impl Command {
    pub async fn run(&self, config: &Config, cache: Arc<Cache>) -> Result<()> {
        match self {
            Self::Search(command) => command.run(config, cache).await,
            Self::Info(command) => command.run(config, cache).await,
            Self::Update(command) => command.run(config, cache).await,
            Self::Install(command) => command.run(config, cache).await,
            Self::Uninstall(command) => command.run(config, cache).await,
        }
    }
}
