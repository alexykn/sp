// brew-rs-client/src/config.rs
// This module handles loading and accessing configuration settings.
// Settings might include things like preferred mirror URLs, cache locations,
// or user preferences.

use std::path::PathBuf;
use crate::utils::error::Result;
use crate::utils::cache;

#[derive(Debug)]
pub struct Config {
    /// Directory where cache files are stored
    pub cache_dir: PathBuf,
    /// API base URL for Homebrew
    pub api_base_url: String,
}

impl Config {
    /// Load configuration from default locations
    pub fn load() -> Result<Self> {
        log::debug!("Loading configuration...");

        // Get cache directory
        let cache_dir = cache::get_cache_dir()?;

        // Default API URL
        let api_base_url = "https://formulae.brew.sh/api".to_string();

        Ok(Config {
            cache_dir,
            api_base_url,
        })
    }
}

// Legacy function for backwards compatibility
pub fn load_config() -> Result<Config> {
    Config::load()
}
