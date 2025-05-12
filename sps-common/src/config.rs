// ===== sps-core/src/utils/config.rs =====
use std::env;
use std::path::{Path, PathBuf};

use dirs;
use tracing::debug;

use super::cache;
use super::error::Result; // for home directory lookup

/// Default installation prefixes
const DEFAULT_LINUX_PREFIX: &str = "/home/linuxbrew/.linuxbrew";
const DEFAULT_MACOS_INTEL_PREFIX: &str = "/usr/local";
const DEFAULT_MACOS_ARM_PREFIX: &str = "/opt/homebrew";

/// Determines the active prefix for installation.
/// Checks sps_PREFIX/HOMEBREW_PREFIX env vars, then OS-specific defaults.
fn determine_prefix() -> PathBuf {
    if let Ok(prefix) = env::var("SPS_PREFIX").or_else(|_| env::var("HOMEBREW_PREFIX")) {
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
        "/usr/local/sps"
    };
    debug!("Using default prefix for OS/Arch: {}", default_prefix);
    PathBuf::from(default_prefix)
}

#[derive(Debug, Clone)]
pub struct Config {
    pub prefix: PathBuf,
    pub cellar: PathBuf,
    pub taps_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub api_base_url: String,
    pub artifact_domain: Option<String>,
    pub docker_registry_token: Option<String>,
    pub docker_registry_basic_auth: Option<String>,
    pub github_api_token: Option<String>,
    pub private_cask_store_dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self> {
        debug!("Loadingspsconfiguration");
        let prefix = determine_prefix();
        let cellar = prefix.join("Cellar");
        let taps_dir = prefix.join("Library/Taps");
        let cache_dir = cache::get_cache_dir()?;
        let api_base_url = "https://formulae.brew.sh/api".to_string();

        // Set up private cask store in ~/.local/share/sps/cask_store
        let mut private_cask_store_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        private_cask_store_dir.push(".local");
        private_cask_store_dir.push("share");
        private_cask_store_dir.push("sps");
        private_cask_store_dir.push("cask_store");

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
            taps_dir,
            cache_dir,
            api_base_url,
            artifact_domain,
            docker_registry_token,
            docker_registry_basic_auth,
            github_api_token,
            private_cask_store_dir,
        })
    }

    // --- Start: New Path Methods ---

    pub fn prefix(&self) -> &Path {
        &self.prefix
    }

    pub fn cellar_path(&self) -> &Path {
        &self.cellar
    }

    pub fn caskroom_dir(&self) -> PathBuf {
        self.prefix.join("Caskroom")
    }

    pub fn opt_dir(&self) -> PathBuf {
        self.prefix.join("opt")
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.prefix.join("bin")
    }

    pub fn applications_dir(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            PathBuf::from("/Applications")
        } else {
            self.prefix.join("Applications")
        }
    }

    pub fn formula_cellar_dir(&self, formula_name: &str) -> PathBuf {
        self.cellar_path().join(formula_name)
    }

    pub fn formula_keg_path(&self, formula_name: &str, version_str: &str) -> PathBuf {
        self.formula_cellar_dir(formula_name).join(version_str)
    }

    pub fn formula_opt_link_path(&self, formula_name: &str) -> PathBuf {
        self.opt_dir().join(formula_name)
    }

    pub fn cask_dir(&self, cask_token: &str) -> PathBuf {
        self.caskroom_dir().join(cask_token)
    }

    /// Returns the path to the cask's token directory in the caskroom.
    pub fn cask_token_path(&self, cask_token: &str) -> PathBuf {
        self.caskroom_dir().join(cask_token)
    }

    /// Returns the base directory for the private cask store
    pub fn private_cask_store_base_dir(&self) -> &Path {
        &self.private_cask_store_dir
    }

    /// Returns the path to the cask's token directory in the private store
    pub fn private_cask_token_path(&self, cask_token: &str) -> PathBuf {
        self.private_cask_store_dir.join(cask_token)
    }

    /// Returns the path to the version directory in the private store
    pub fn private_cask_version_path(&self, cask_token: &str, version_str: &str) -> PathBuf {
        self.private_cask_token_path(cask_token).join(version_str)
    }

    /// Returns the path to an app in the private store
    pub fn private_cask_app_path(
        &self,
        cask_token: &str,
        version_str: &str,
        app_name: &str,
    ) -> PathBuf {
        self.private_cask_version_path(cask_token, version_str)
            .join(app_name)
    }

    pub fn cask_version_path(&self, cask_token: &str, version_str: &str) -> PathBuf {
        self.cask_dir(cask_token).join(version_str)
    }

    /// Returns the path to the current user's home directory.
    pub fn home_dir(&self) -> PathBuf {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    }

    /// Returns the base manpage directory (e.g., /usr/local/share/man).
    pub fn manpagedir(&self) -> PathBuf {
        self.prefix.join("share").join("man")
    }

    // --- End: New Path Methods ---

    pub fn get_tap_path(&self, name: &str) -> Option<PathBuf> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() == 2 {
            Some(
                self.taps_dir
                    .join(parts[0])
                    .join(format!("homebrew-{}", parts[1])),
            )
        } else {
            None
        }
    }

    pub fn get_formula_path_from_tap(&self, tap_name: &str, formula_name: &str) -> Option<PathBuf> {
        self.get_tap_path(tap_name).and_then(|tap_path| {
            let json_path = tap_path
                .join("Formula")
                .join(format!("{formula_name}.json"));
            if json_path.exists() {
                return Some(json_path);
            }
            let rb_path = tap_path.join("Formula").join(format!("{formula_name}.rb"));
            if rb_path.exists() {
                return Some(rb_path);
            }
            None
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::load().expect("Failed to load default configuration")
    }
}

pub fn load_config() -> Result<Config> {
    Config::load()
}
