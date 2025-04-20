// ===== sapphire-core/src/model/cask.rs =====
use crate::utils::config::Config; // Import Config
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ... (Struct definitions remain the same) ...
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cask {
    pub token: String,
    pub name: Option<Vec<String>>,
    pub version: Option<String>,
    pub desc: Option<String>,
    pub homepage: Option<String>,
    pub artifacts: Option<Vec<Artifact>>,
    pub url: Option<Vec<String>>,
    pub sha256: Option<String>,
    pub appcast: Option<Appcast>,
    pub auto_updates: Option<bool>,
    pub depends_on: Option<Dependencies>,
    pub conflicts_with: Option<Vec<String>>,
    pub caveats: Option<String>,
    pub stage_only: Option<bool>,
    #[serde(default)]
    pub uninstall: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    pub zap: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Artifact {
    App(String),
    Binary(String),
    Pkg {
        pkg: String,
        allow_untrusted: Option<bool>,
    },
    Map(HashMap<String, serde_json::Value>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Appcast {
    pub url: String,
    pub checkpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependencies {
    pub cask: Option<Vec<String>>,
    pub formula: Option<Vec<String>>,
    pub macos: Option<MacOSRequirement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MacOSRequirement {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaskList {
    pub casks: Vec<Cask>,
}


impl Cask {
    /// Check if this cask is installed
    // Added Config parameter
    pub fn is_installed(&self, config: &Config) -> bool {
        // Use Config method
        let cask_dir = config.cask_dir(&self.token);
        cask_dir.exists()
    }

    /// Get the installed version of this cask
    // Added Config parameter
    pub fn installed_version(&self, config: &Config) -> Option<String> {
        // Use Config method
        let cask_dir = config.cask_dir(&self.token);

        if !cask_dir.exists() {
            return None;
        }

        // Read the cask's subdirectories to find versions
        match std::fs::read_dir(cask_dir) {
            Ok(entries) => {
                for entry in entries {
                    if let Ok(entry) = entry {
                        if let Ok(metadata) = entry.metadata() {
                            if metadata.is_dir() {
                                if let Some(version) = entry.file_name().to_str() {
                                    return Some(version.to_string());
                                }
                            }
                        }
                    }
                }
                None
            }
            Err(_) => None,
        }
    }

    /// Get friendly name for display purposes
    pub fn display_name(&self) -> String {
        if let Some(ref names) = self.name {
            if !names.is_empty() {
                return names[0].clone();
            }
        }
        self.token.clone()
    }
}

// REMOVED: get_caskroom_dir (now in Config)