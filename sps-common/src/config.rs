// sps-common/src/config.rs
use std::env;
use std::path::{Path, PathBuf};

use directories::UserDirs; // Ensure this crate is in sps-common/Cargo.toml
use tracing::debug;

use super::error::Result; // Assuming SpsResult is Result from super::error

// This constant will serve as a fallback if HOMEBREW_PREFIX is not set or is empty.
const DEFAULT_FALLBACK_SPS_ROOT: &str = "/opt/homebrew";
const SPS_ROOT_MARKER_FILENAME: &str = ".sps_root_v1";

#[derive(Debug, Clone)]
pub struct Config {
    pub sps_root: PathBuf, // Public for direct construction in main for init if needed
    pub api_base_url: String,
    pub artifact_domain: Option<String>,
    pub docker_registry_token: Option<String>,
    pub docker_registry_basic_auth: Option<String>,
    pub github_api_token: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        debug!("Loading sps configuration");

        // Try to get SPS_ROOT from HOMEBREW_PREFIX environment variable.
        // Fallback to DEFAULT_FALLBACK_SPS_ROOT if not set or empty.
        let sps_root_str = env::var("HOMEBREW_PREFIX").ok().filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                debug!(
                    "HOMEBREW_PREFIX environment variable not set or empty, falling back to default: {}",
                    DEFAULT_FALLBACK_SPS_ROOT
                );
                DEFAULT_FALLBACK_SPS_ROOT.to_string()
            });

        let sps_root_path = PathBuf::from(&sps_root_str);
        debug!("Effective SPS_ROOT set to: {}", sps_root_path.display());

        let api_base_url = "https://formulae.brew.sh/api".to_string();

        let artifact_domain = env::var("HOMEBREW_ARTIFACT_DOMAIN").ok();
        let docker_registry_token = env::var("HOMEBREW_DOCKER_REGISTRY_TOKEN").ok();
        let docker_registry_basic_auth = env::var("HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN").ok();
        let github_api_token = env::var("HOMEBREW_GITHUB_API_TOKEN").ok();

        debug!("Configuration loaded successfully.");
        Ok(Self {
            sps_root: sps_root_path,
            api_base_url,
            artifact_domain,
            docker_registry_token,
            docker_registry_basic_auth,
            github_api_token,
        })
    }

    pub fn sps_root(&self) -> &Path {
        &self.sps_root
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.sps_root.join("bin")
    }

    pub fn cellar_dir(&self) -> PathBuf {
        self.sps_root.join("Cellar") // Changed from "cellar" to "Cellar" to match Homebrew
    }

    pub fn cask_room_dir(&self) -> PathBuf {
        self.sps_root.join("Caskroom") // Changed from "cask_room" to "Caskroom"
    }

    pub fn cask_store_dir(&self) -> PathBuf {
        self.sps_root.join("sps_cask_store")
    }

    pub fn opt_dir(&self) -> PathBuf {
        self.sps_root.join("opt")
    }

    pub fn taps_dir(&self) -> PathBuf {
        self.sps_root.join("Library/Taps") // Adjusted to match Homebrew structure
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.sps_root.join("sps_cache")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.sps_root.join("sps_logs")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.sps_root.join("tmp")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.sps_root.join("state")
    }

    pub fn man_base_dir(&self) -> PathBuf {
        self.sps_root.join("share").join("man")
    }

    pub fn sps_root_marker_path(&self) -> PathBuf {
        self.sps_root.join(SPS_ROOT_MARKER_FILENAME)
    }

    pub fn applications_dir(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            PathBuf::from("/Applications")
        } else {
            self.home_dir().join("Applications")
        }
    }

    pub fn formula_cellar_dir(&self, formula_name: &str) -> PathBuf {
        self.cellar_dir().join(formula_name)
    }

    pub fn formula_keg_path(&self, formula_name: &str, version_str: &str) -> PathBuf {
        self.formula_cellar_dir(formula_name).join(version_str)
    }

    pub fn formula_opt_path(&self, formula_name: &str) -> PathBuf {
        self.opt_dir().join(formula_name)
    }

    pub fn cask_room_token_path(&self, cask_token: &str) -> PathBuf {
        self.cask_room_dir().join(cask_token)
    }

    pub fn cask_store_token_path(&self, cask_token: &str) -> PathBuf {
        self.cask_store_dir().join(cask_token)
    }

    pub fn cask_store_version_path(&self, cask_token: &str, version_str: &str) -> PathBuf {
        self.cask_store_token_path(cask_token).join(version_str)
    }

    pub fn cask_store_app_path(
        &self,
        cask_token: &str,
        version_str: &str,
        app_name: &str,
    ) -> PathBuf {
        self.cask_store_version_path(cask_token, version_str)
            .join(app_name)
    }

    pub fn cask_room_version_path(&self, cask_token: &str, version_str: &str) -> PathBuf {
        self.cask_room_token_path(cask_token).join(version_str)
    }

    pub fn home_dir(&self) -> PathBuf {
        UserDirs::new().map_or_else(|| PathBuf::from("/"), |ud| ud.home_dir().to_path_buf())
    }

    pub fn get_tap_path(&self, name: &str) -> Option<PathBuf> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() == 2 {
            Some(
                self.taps_dir()
                    .join(parts[0]) // user, e.g., homebrew
                    .join(format!("homebrew-{}", parts[1])), // repo, e.g., homebrew-core
            )
        } else {
            None
        }
    }

    pub fn get_formula_path_from_tap(&self, tap_name: &str, formula_name: &str) -> Option<PathBuf> {
        self.get_tap_path(tap_name).and_then(|tap_path| {
            let json_path = tap_path
                .join("Formula") // Standard Homebrew tap structure
                .join(format!("{formula_name}.json"));
            if json_path.exists() {
                return Some(json_path);
            }
            // Fallback to .rb for completeness, though API primarily gives JSON
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
