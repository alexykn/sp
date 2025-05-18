// sps-cli/src/cli/install.rs

use std::sync::Arc;

use clap::Args;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::Result;
use tracing::instrument;

// Import pipeline components from the new module
use crate::pipeline::runner::{self, CommandType, PipelineFlags};

// Keep the Args struct specific to 'install' if needed, or reuse a common one
#[derive(Debug, Args)]
pub struct InstallArgs {
    #[arg(required = true)]
    names: Vec<String>,

    // Keep flags relevant to install/pipeline
    #[arg(long)]
    skip_deps: bool, // Note: May not be fully supported by core resolution yet
    #[arg(long, help = "Force install specified targets as casks")]
    cask: bool,
    #[arg(long, help = "Force install specified targets as formulas")]
    formula: bool,
    #[arg(long)]
    include_optional: bool,
    #[arg(long)]
    skip_recommended: bool,
    #[arg(
        long,
        help = "Force building the formula from source, even if a bottle is available"
    )]
    build_from_source: bool,
    // Worker/Queue size flags might belong here or be global CLI flags
    // #[arg(long, value_name = "sps_WORKERS")]
    // max_workers: Option<usize>,
    // #[arg(long, value_name = "sps_QUEUE")]
    // queue_size: Option<usize>,
}

impl InstallArgs {
    #[instrument(skip(self, config, cache), fields(targets = ?self.names))]
    pub async fn run(&self, config: &Config, cache: Arc<Cache>) -> Result<()> {
        // --- Argument Validation (moved from old run) ---
        if self.formula && self.cask {
            return Err(sps_common::error::SpsError::Generic(
                "Cannot use --formula and --cask together.".to_string(),
            ));
        }
        // Add validation for skip_deps if needed

        // --- Prepare Pipeline Flags ---
        let flags = PipelineFlags {
            build_from_source: self.build_from_source,
            include_optional: self.include_optional,
            skip_recommended: self.skip_recommended,
            // Add other flags...
        };

        // --- Determine Initial Targets based on --formula/--cask flags ---
        // (This logic might be better inside plan_package_operations based on CommandType)
        let initial_targets = self.names.clone(); // For install, all names are initial targets

        // --- Execute the Pipeline ---
        runner::run_pipeline(
            &initial_targets,
            CommandType::Install, // Specify the command type
            config,
            cache,
            &flags, // Pass the flags struct
        )
        .await
    }
}
