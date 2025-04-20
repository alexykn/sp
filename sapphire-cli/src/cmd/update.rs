// src/cmd/update.rs
// Contains the logic for the `update` command.
use std::fs;
use sapphire_core::fetch::api;
// Removed unused colored import
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::Result;
use std::sync::Arc; // <-- ADDED

// Updated function signature (accepts Config and Cache)
pub async fn run_update(config: &Config, cache: &Arc<Cache>) -> Result<()> {
    log::debug!("Running manual update..."); // Log clearly it's the manual one
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
            let err_msg = format!("Failed to fetch/store formulas from API: {}", e);
            log::error!("{}", err_msg);
            pb.finish_and_clear(); // Clear spinner on error
                                   // Convert error if needed before returning
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
                                   // Convert error if needed before returning
            return Err(e.into());
        }
    }

    // *** Add timestamp update logic here ***
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
            // Log a warning but don't fail the whole update command if timestamp creation fails
            log::warn!(
                "Failed to create or update timestamp file '{}': {}",
                timestamp_file.display(),
                e
            );
        }
    }
    // *** End of added logic ***

    pb.finish_with_message("Update completed successfully!");
    Ok(())
}