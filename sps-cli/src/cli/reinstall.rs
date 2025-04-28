// sps-cli/src/cli/reinstall.rs
use std::sync::Arc;

use clap::Args;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::Result;

use crate::cli::pipeline::{CommandType, PipelineExecutor, PipelineFlags};

#[derive(Args, Debug)]
pub struct ReinstallArgs {
    #[arg(required = true)]
    pub names: Vec<String>,

    #[arg(
        long,
        help = "Force building the formula from source, even if a bottle is available"
    )]
    pub build_from_source: bool,
}

impl ReinstallArgs {
    pub async fn run(&self, config: &Config, cache: Arc<Cache>) -> Result<()> {
        println!("Reinstalling: {:?}", self.names); // User feedback
        let flags = PipelineFlags {
            // Populate flags from args
            build_from_source: self.build_from_source,
            include_optional: false, // Reinstall usually doesn't change optional deps
            skip_recommended: true,  /* Reinstall usually doesn't change recommended deps
                                      * ... add other common flags if needed ... */
        };
        PipelineExecutor::execute_pipeline(
            &self.names,
            CommandType::Reinstall,
            config,
            cache,
            &flags,
        )
        .await
    }
}
