// sps/src/cli.rs
//! Defines the command-line argument structure using clap.
use std::sync::Arc;

use clap::{ArgAction, Parser, Subcommand};
use sps_common::error::Result;
use sps_common::{Cache, Config};

// Module declarations
pub mod info;
pub mod init;
pub mod install;
pub mod list;
pub mod reinstall;
pub mod search;
pub mod status;
pub mod uninstall;
pub mod update;
pub mod upgrade;
// Re-export InitArgs to make it accessible as cli::InitArgs
// Import other command Args structs
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
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    Init(InitArgs),
    Search(Search),
    List(List),
    Info(Info),
    Update(Update),
    Install(InstallArgs),
    Uninstall(Uninstall),
    Reinstall(ReinstallArgs),
    Upgrade(UpgradeArgs),
}

impl Command {
    pub async fn run(&self, config: &Config, cache: Arc<Cache>) -> Result<()> {
        match self {
            Self::Init(command) => command.run(config).await,
            Self::Search(command) => command.run(config, cache).await,
            Self::List(command) => command.run(config, cache).await,
            Self::Info(command) => command.run(config, cache).await,
            Self::Update(command) => command.run(config, cache).await,
            // Commands that use the pipeline
            Self::Install(command) => command.run(config, cache).await,
            Self::Reinstall(command) => command.run(config, cache).await,
            Self::Upgrade(command) => command.run(config, cache).await,
            Self::Uninstall(command) => command.run(config, cache).await,
        }
    }
}

// In install.rs, reinstall.rs, upgrade.rs, their run methods will now call
// sps::cli::pipeline_runner::run_pipeline(...)
// e.g., in sps/src/cli/install.rs:
// use crate::cli::pipeline_runner::{self, CommandType, PipelineFlags};
// ...
// pipeline_runner::run_pipeline(&initial_targets, CommandType::Install, config, cache,
// &flags).await
