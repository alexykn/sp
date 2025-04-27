//! Contains the logic for the `update` command.
use std::fs;
use std::sync::Arc;

use sp_common::cache::Cache;
use sp_common::config::Config;
use sp_common::error::Result;
use sp_net::fetch::api;

use crate::ui;

#[derive(clap::Args, Debug)]
pub struct Update;

impl Update {
    pub async fn run(&self, config: &Config, cache: Arc<Cache>) -> Result<()> {
        tracing::debug!("Running manual update..."); // Log clearly it's the manual one

        // Use the ui utility function to create the spinner
        let pb = ui::create_spinner("Updating package lists"); // <-- CHANGED

        tracing::debug!("Using cache directory: {:?}", config.cache_dir);

        // Fetch and store raw formula data
        match api::fetch_all_formulas().await {
            Ok(raw_data) => {
                cache.store_raw("formula.json", &raw_data)?;
                tracing::debug!("✓ Successfully cached formulas data");
                pb.set_message("Cached formulas data");
            }
            Err(e) => {
                let err_msg = format!("Failed to fetch/store formulas from API: {e}");
                tracing::error!("{}", err_msg);
                pb.finish_and_clear(); // Clear spinner on error
                return Err(e);
            }
        }

        // Fetch and store raw cask data
        match api::fetch_all_casks().await {
            Ok(raw_data) => {
                cache.store_raw("cask.json", &raw_data)?;
                tracing::debug!("✓ Successfully cached casks data");
                pb.set_message("Cached casks data");
            }
            Err(e) => {
                let err_msg = format!("Failed to fetch/store casks from API: {e}");
                tracing::error!("{}", err_msg);
                pb.finish_and_clear(); // Clear spinner on error
                return Err(e);
            }
        }

        // Update timestamp file
        let timestamp_file = config.cache_dir.join(".sp_last_update_check");
        tracing::debug!(
            "Manual update successful. Updating timestamp file: {}",
            timestamp_file.display()
        );
        match fs::File::create(&timestamp_file) {
            Ok(_) => {
                tracing::debug!("Updated timestamp file successfully.");
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to create or update timestamp file '{}': {}",
                    timestamp_file.display(),
                    e
                );
            }
        }

        pb.finish_with_message("Update completed successfully!");
        Ok(())
    }
}
