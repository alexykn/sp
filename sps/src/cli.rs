// sps/src/cli.rs
//! Defines the command-line argument structure using clap.
use std::sync::Arc;

use clap::{ArgAction, Parser, Subcommand};
use sps_common::error::Result;
use sps_common::{Cache, Config};

// Module declarations
pub mod info;
pub mod init; // Already public
pub mod install;
pub mod list;
pub mod reinstall;
pub mod runner;
pub mod search;
pub mod status;
pub mod uninstall;
pub mod update;
pub mod upgrade;

// Re-export InitArgs to make it accessible as cli::InitArgs
// Re-export other command Args structs if they are used directly by main or other top-level
// logic
use crate::cli::info::Info;
pub use crate::cli::init::InitArgs;
use crate::cli::install::InstallArgs;
use crate::cli::list::List;
use crate::cli::reinstall::ReinstallArgs;
use crate::cli::search::Search;
use crate::cli::uninstall::Uninstall;
use crate::cli::update::Update;
use crate::cli::upgrade::UpgradeArgs;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None, name = "sps", bin_name = "sps")]
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
    /// Initialize SPS on your system
    Init(InitArgs), // This will now correctly resolve due to the pub use above
    /// Search for available formulas and casks
    Search(Search),
    /// List all installed formulas and casks
    List(List),
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
            Self::Init(command) => command.run(config).await, // InitArgs::run takes &Config
            Self::Search(command) => command.run(config, cache).await,
            Self::List(command) => command.run(config, cache).await,
            Self::Info(command) => command.run(config, cache).await,
            Self::Update(command) => command.run(config, cache).await,
            Self::Install(command) => command.run(config, cache).await,
            Self::Uninstall(command) => command.run(config, cache).await,
            Self::Reinstall(command) => command.run(config, cache).await,
            Self::Upgrade(command) => command.run(config, cache).await,
        }
    }
}
