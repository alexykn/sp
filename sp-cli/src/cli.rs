//! Defines the command-line argument structure using clap.
use std::sync::Arc;

use clap::{ArgAction, Parser, Subcommand};
use sp_common::error::Result;
use sp_common::{Cache, Config};

use crate::cli::info::Info;
use crate::cli::install::InstallArgs;
use crate::cli::reinstall::ReinstallArgs;
use crate::cli::search::Search;
use crate::cli::uninstall::Uninstall;
use crate::cli::update::Update;
use crate::cli::upgrade::UpgradeArgs;

pub mod info;
pub mod install;
pub mod pipeline;
pub mod reinstall;
pub mod search;
pub mod uninstall;
pub mod update;
pub mod upgrade;

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
    Install(InstallArgs),

    /// Uninstall one or more formulas or casks
    Uninstall(Uninstall),

    /// Reinstall one or more formulas or casks
    Reinstall(ReinstallArgs),

    /// Upgrade one or more formulas or casks
    Upgrade(UpgradeArgs),
}

impl Command {
    pub async fn run(&self, config: &Config, cache: Arc<Cache>) -> Result<()> {
        match self {
            Self::Search(command) => command.run(config, cache).await,
            Self::Info(command) => command.run(config, cache).await,
            Self::Update(command) => command.run(config, cache).await,
            Self::Install(command) => command.run(config, cache).await,
            Self::Uninstall(command) => command.run(config, cache).await,
            Self::Reinstall(command) => command.run(config, cache).await,
            Self::Upgrade(command) => command.run(config, cache).await,
        }
    }
}
