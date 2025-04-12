// Merged config module: combines logic from sapphire-core/src/config.rs and sapphire-core/src/utils/config.rs

use std::path::PathBuf;
use std::env;
use crate::utils::error::Result;
use crate::utils::cache;

/// Default installation prefixes
const DEFAULT_LINUX_PREFIX: &str = "/home/linuxbrew/.linuxbrew";
const DEFAULT_MACOS_INTEL_PREFIX: &str = "/usr/local";
const DEFAULT_MACOS_ARM_PREFIX: &str = "/opt/homebrew";

/// Determines the active prefix for installation.
/// Checks SAPPHIRE_PREFIX/HOMEBREW_PREFIX env vars, then OS-specific defaults.
fn determine_prefix() -> PathBuf {
    if let Ok(prefix) = env::var("SAPPHIRE_PREFIX").or_else(|_| env::var("HOMEBREW_PREFIX")) {
        return PathBuf::from(prefix);
    }

    if cfg!(target_os = "linux") {
        PathBuf::from(DEFAULT_LINUX_PREFIX)
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            PathBuf::from(DEFAULT_MACOS_ARM_PREFIX)
        } else {
            PathBuf::from(DEFAULT_MACOS_INTEL_PREFIX)
        }
    } else {
        // Fallback for unsupported OS
        PathBuf::from("/usr/local/sapphire")
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Installation prefix (e.g., /opt/homebrew)
    pub prefix: PathBuf,
    /// Cellar directory (e.g., /opt/homebrew/Cellar)
    pub cellar: PathBuf,
    /// Directory for tap repositories
    pub taps_dir: PathBuf,
    /// Directory where cache files are stored
    pub cache_dir: PathBuf,
    /// API base URL for Homebrew
    pub api_base_url: String,
    // Add other config fields as needed
}

impl Config {
    /// Loads configuration from environment and system defaults.
    pub fn load() -> Result<Self> {
        let prefix = determine_prefix();
        let cellar = prefix.join("Cellar");
        let taps_parent_dir = prefix.join("share").join("sapphire").join("taps");
        let cache_dir = cache::get_cache_dir()?;
        let api_base_url = "https://formulae.brew.sh/api".to_string();

        Ok(Self {
            prefix,
            cellar,
            taps_dir: taps_parent_dir,
            cache_dir,
            api_base_url,
        })
    }

    /// Gets the path to a specific tap repository.
    /// name should be in "user/repo" format (e.g., "homebrew/core").
    pub fn get_tap_path(&self, name: &str) -> Option<PathBuf> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() == 2 {
            Some(self.taps_dir.join(parts[0]).join(parts[1]))
        } else {
            None // Invalid tap name format
        }
    }

    /// Gets the conventional path to a formula file within a specific tap.
    pub fn get_formula_path(&self, tap_name: &str, formula_name: &str) -> Option<PathBuf> {
        self.get_tap_path(tap_name)
            .map(|tap_path| tap_path.join("Formula").join(format!("{}.json", formula_name)))
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::load().expect("Failed to load default configuration")
    }
}

// Legacy function for backwards compatibility
pub fn load_config() -> Result<Config> {
    Config::load()
}
