// src/cmd/update.rs
// Contains the logic for the `update` command.

use sapphire_core::fetch::api;
// Removed unused colored import
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::Result;
use std::sync::Arc; // <-- ADDED

/// Updates the local cache of formulas and casks.
/// This downloads the current lists from the Homebrew API.
pub async fn run_update(config: &Config, cache: &Arc<Cache>) -> Result<()> {
    log::debug!("Updating formula and cask lists...");
    // Spinner for update
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::with_template("{spinner:.yellow} {msg}").unwrap());
    pb.set_message("Updating package lists");
    pb.enable_steady_tick(Duration::from_millis(100));

    log::debug!("Using cache directory: {:?}", config.cache_dir);

    // Fetch and store raw formula data
    match api::fetch_all_formulas().await {
        Ok(raw_data) => {
            cache.store_raw("formula.json", &raw_data)?;
            log::debug!("✓ Successfully cached formulas data");
            pb.set_message("Cached formulas data");
        }
        Err(e) => {
            log::error!("Failed to fetch formulas from API: {}", e);
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
            log::error!("Failed to fetch casks from API: {}", e);
            return Err(e.into());
        }
    }

    pb.finish_with_message("Update completed successfully!");
    Ok(())
}
