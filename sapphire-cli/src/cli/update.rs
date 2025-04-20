//! Contains the logic for the `update` command.
use std::fs;
use std::sync::Arc;

use sapphire_core::fetch::api;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::Result;

use crate::ui;

#[derive(clap::Args, Debug)]
pub struct Update;

impl Update {
    pub async fn run(&self, config: &Config, cache: Arc<Cache>) -> Result<()> {
        log::debug!("Running manual update..."); // Log clearly it's the manual one

        // Use the ui utility function to create the spinner
        let pb = ui::create_spinner("Updating package lists"); // <-- CHANGED

        log::debug!("Using cache directory: {:?}", config.cache_dir);

        // Fetch and store raw formula data
        match api::fetch_all_formulas().await {
            Ok(raw_data) => {
                cache.store_raw("formula.json", &raw_data)?;
                log::debug!("✓ Successfully cached formulas data");
                pb.set_message("Cached formulas data");
            }
            Err(e) => {
                let err_msg = format!("Failed to fetch/store formulas from API: {}", e);
                log::error!("{}", err_msg);
                pb.finish_and_clear(); // Clear spinner on error
                return Err(e.into());
            }
        }

        // Fetch and store raw cask data
        match api::fetch_all_casks().await {
            Ok(raw_data) => {
                cache.store_raw("cask.json", &raw_data)?;
                log::debug!("✓ Successfully cached casks data");
                pb.set_message("Cached casks data");
            }
            Err(e) => {
                let err_msg = format!("Failed to fetch/store casks from API: {}", e);
                log::error!("{}", err_msg);
                pb.finish_and_clear(); // Clear spinner on error
                return Err(e.into());
            }
        }

        // Update timestamp file
        let timestamp_file = config.cache_dir.join(".sapphire_last_update_check");
        log::debug!(
            "Manual update successful. Updating timestamp file: {}",
            timestamp_file.display()
        );
        match fs::File::create(&timestamp_file) {
            Ok(_) => {
                log::debug!("Updated timestamp file successfully.");
            }
            Err(e) => {
                log::warn!(
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
