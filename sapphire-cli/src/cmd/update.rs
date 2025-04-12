// src/cmd/update.rs
// Contains the logic for the `update` command.

use sapphire_core::fetch::api;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::Result;
use std::path::PathBuf;

/// Updates the local cache of formulas and casks.
/// This downloads the current lists from the Homebrew API.
pub async fn run_update() -> Result<()> {
    log::info!("Updating formula and cask lists...");

    // Initialize config and cache
    let config = Config::load()?;
    let cache_dir: PathBuf = config.cache_dir.clone();
    println!("Cache directory: {:?}", cache_dir);
    let cache = Cache::new(&config.cache_dir)?;

    // Fetch and store raw formula data
    match api::fetch_all_formulas().await {
        Ok(raw_data) => {
            cache.store_raw("formula.json", &raw_data)?;
            log::info!("✓ Successfully cached formulas data");
        },
        Err(e) => {
            log::error!("Failed to fetch formulas from API: {}", e);
            return Err(e);
        }
    }

    // Fetch and store raw cask data
    match api::fetch_all_casks().await {
        Ok(raw_data) => {
            cache.store_raw("cask.json", &raw_data)?;
            log::info!("✓ Successfully cached casks data");
        },
        Err(e) => {
            log::error!("Failed to fetch casks from API: {}", e);
            return Err(e);
        }
    }

    println!("Update completed successfully!");
    Ok(())
}
