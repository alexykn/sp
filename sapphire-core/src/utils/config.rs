// Merged config module: combines logic from sapphire-core/src/config.rs and sapphire-core/src/utils/config.rs

use crate::utils::cache; // Keep cache import if needed elsewhere
use crate::utils::error::Result;
use log::debug;
use std::env;
use std::path::PathBuf; // Use log crate

/// Default installation prefixes
const DEFAULT_LINUX_PREFIX: &str = "/home/linuxbrew/.linuxbrew";
const DEFAULT_MACOS_INTEL_PREFIX: &str = "/usr/local";
const DEFAULT_MACOS_ARM_PREFIX: &str = "/opt/homebrew";

/// Determines the active prefix for installation.
/// Checks SAPPHIRE_PREFIX/HOMEBREW_PREFIX env vars, then OS-specific defaults.
fn determine_prefix() -> PathBuf {
    if let Ok(prefix) = env::var("SAPPHIRE_PREFIX").or_else(|_| env::var("HOMEBREW_PREFIX")) {
        debug!("Using prefix from environment variable: {}", prefix);
        return PathBuf::from(prefix);
    }

    let default_prefix = if cfg!(target_os = "linux") {
        DEFAULT_LINUX_PREFIX
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            DEFAULT_MACOS_ARM_PREFIX
        } else {
            DEFAULT_MACOS_INTEL_PREFIX
        }
    } else {
        // Fallback for unsupported OS
        "/usr/local/sapphire"
    };
    debug!("Using default prefix for OS/Arch: {}", default_prefix);
    PathBuf::from(default_prefix)
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
    /// API base URL for Homebrew (formulae.brew.sh)
    pub api_base_url: String,

    // --- Added Authentication Fields ---
    /// Custom OCI registry domain (from HOMEBREW_ARTIFACT_DOMAIN)
    pub artifact_domain: Option<String>,
    /// Explicit OCI registry bearer token (from HOMEBREW_DOCKER_REGISTRY_TOKEN)
    pub docker_registry_token: Option<String>,
    /// Explicit OCI registry basic auth token (from HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN)
    pub docker_registry_basic_auth: Option<String>,
    /// GitHub API token (from HOMEBREW_GITHUB_API_TOKEN)
    pub github_api_token: Option<String>,
    // --- End Added Fields ---

    // Add other config fields as needed
}

impl Config {
    /// Loads configuration from environment and system defaults.
    pub fn load() -> Result<Self> {
        debug!("Loading Sapphire configuration...");
        let prefix = determine_prefix();
        let cellar = prefix.join("Cellar");
        // Consider making taps dir configurable or relative to prefix consistently
        let taps_parent_dir = prefix.join("Library/Taps"); // More standard location?
        let cache_dir = cache::get_cache_dir()?; // Uses dirs crate internally
        let api_base_url = "https://formulae.brew.sh/api".to_string();

        // Load authentication environment variables
        let artifact_domain = env::var("HOMEBREW_ARTIFACT_DOMAIN").ok();
        let docker_registry_token = env::var("HOMEBREW_DOCKER_REGISTRY_TOKEN").ok();
        let docker_registry_basic_auth = env::var("HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN").ok();
        let github_api_token = env::var("HOMEBREW_GITHUB_API_TOKEN").ok();

        if artifact_domain.is_some() {
            debug!("Loaded HOMEBREW_ARTIFACT_DOMAIN");
        }
        if docker_registry_token.is_some() {
            debug!("Loaded HOMEBREW_DOCKER_REGISTRY_TOKEN");
        }
        if docker_registry_basic_auth.is_some() {
            debug!("Loaded HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN");
        }
        if github_api_token.is_some() {
            debug!("Loaded HOMEBREW_GITHUB_API_TOKEN");
        }

        debug!("Configuration loaded successfully.");
        Ok(Self {
            prefix,
            cellar,
            taps_dir: taps_parent_dir,
            cache_dir,
            api_base_url,
            artifact_domain,
            docker_registry_token,
            docker_registry_basic_auth,
            github_api_token,
        })
    }

    /// Gets the path to a specific tap repository.
    /// name should be in "user/repo" format (e.g., "homebrew/core").
    pub fn get_tap_path(&self, name: &str) -> Option<PathBuf> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() == 2 {
            // Construct path like /prefix/Library/Taps/user/homebrew-repo
            Some(
                self.prefix
                    .join("Library/Taps")
                    .join(parts[0])
                    .join(format!("homebrew-{}", parts[1])),
            )
        } else {
            None // Invalid tap name format
        }
    }

    /// Gets the conventional path to a formula file within a specific tap's local clone.
    /// Assumes standard formula location (e.g., Formula/*.rb or Formula/*.json).
    /// Note: Homebrew API doesn't rely on local taps as much now.
    pub fn get_formula_path_from_tap(&self, tap_name: &str, formula_name: &str) -> Option<PathBuf> {
        self.get_tap_path(tap_name).and_then(|tap_path| {
            // Check for both .rb (legacy) and .json (API cache mimic)
            let json_path = tap_path
                .join("Formula")
                .join(format!("{}.json", formula_name));
            if json_path.exists() {
                return Some(json_path);
            }
            let rb_path = tap_path
                .join("Formula")
                .join(format!("{}.rb", formula_name));
            if rb_path.exists() {
                return Some(rb_path);
            }
            // Add check for Aliases if needed
            None
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::load().expect("Failed to load default configuration")
    }
}

// Legacy function wrapper (consider removing if not used externally)
pub fn load_config() -> Result<Config> {
    Config::load()
}
