// src/cli/args.rs
// Defines the command-line argument structure using clap.

use crate::cmd::install::InstallArgs;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Search for available formulas and casks
    Search {
        /// The search term to look for
        query: String,

        /// Search only formulae
        #[arg(long, conflicts_with = "cask")]
        formula: bool,

        /// Search only casks
        #[arg(long, conflicts_with = "formula")]
        cask: bool,
    },

    /// Display information about a formula or cask
    Info {
        /// Name of the formula or cask
        name: String,

        /// Show information for a cask, not a formula
        #[arg(long)]
        cask: bool,
    },

    /// Fetch the latest package list from the API
    Update,

    /// Install a formula or cask
    Install(InstallArgs),

    /// Uninstall a formula or cask (placeholder)
    Uninstall { name: String },
}
