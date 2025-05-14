// ===== sps-common/src/model/cask.rs ===== // Corrected path
use std::collections::HashMap;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::config::Config;

pub type Artifact = serde_json::Value;

/// Represents the `url` field, which can be a simple string or a map with specs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UrlField {
    Simple(String),
    WithSpec {
        url: String,
        #[serde(default)]
        verified: Option<String>,
        #[serde(flatten)]
        other: HashMap<String, serde_json::Value>,
    },
}

/// Represents the `sha256` field: hex, no_check, or per-architecture
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Sha256Field {
    Hex(String),
    #[serde(rename_all = "snake_case")]
    NoCheck {
        no_check: bool,
    },
    PerArch(HashMap<String, String>),
}

/// Appcast metadata
#[derive(Debug, Clone, Serialize, Deserialize)] // Ensure Serialize/Deserialize are here
pub struct Appcast {
    pub url: String,
    pub checkpoint: Option<String>,
}

/// Represents conflicts with other casks or formulae
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictsWith {
    #[serde(default)]
    pub cask: Vec<String>,
    #[serde(default)]
    pub formula: Vec<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Represents the specific architecture details found in some cask definitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchSpec {
    #[serde(rename = "type")] // Map the JSON "type" field
    pub type_name: String, // e.g., "arm"
    pub bits: u32, // e.g., 64
}

/// Helper for architecture requirements: single string, list of strings, or list of spec objects
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArchReq {
    One(String),       // e.g., "arm64"
    Many(Vec<String>), // e.g., ["arm64", "x86_64"]
    Specs(Vec<ArchSpec>),
}

/// Helper for macOS requirements: symbol, list, comparison, or map
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MacOSReq {
    Symbol(String),       // ":big_sur"
    Symbols(Vec<String>), // [":catalina", ":big_sur"]
    Comparison(String),   // ">= :big_sur"
    Map(HashMap<String, Vec<String>>),
}

/// Helper to coerce string-or-list into Vec<String>
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringList {
    One(String),
    Many(Vec<String>),
}

impl From<StringList> for Vec<String> {
    fn from(item: StringList) -> Self {
        match item {
            StringList::One(s) => vec![s],
            StringList::Many(v) => v,
        }
    }
}

/// Represents `depends_on` block with multiple possible keys
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DependsOn {
    #[serde(default)]
    pub cask: Vec<String>,
    #[serde(default)]
    pub formula: Vec<String>,
    #[serde(default)]
    pub arch: Option<ArchReq>,
    #[serde(default)]
    pub macos: Option<MacOSReq>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// The main Cask model matching Homebrew JSON v2
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Cask {
    pub token: String,

    #[serde(default)]
    pub name: Option<Vec<String>>,
    pub version: Option<String>,
    pub desc: Option<String>,
    pub homepage: Option<String>,

    #[serde(default)]
    pub artifacts: Option<Vec<Artifact>>,

    #[serde(default)]
    pub url: Option<UrlField>,
    #[serde(default)]
    pub url_specs: Option<HashMap<String, serde_json::Value>>,

    #[serde(default)]
    pub sha256: Option<Sha256Field>,

    pub appcast: Option<Appcast>,
    pub auto_updates: Option<bool>,

    #[serde(default)]
    pub depends_on: Option<DependsOn>,

    #[serde(default)]
    pub conflicts_with: Option<ConflictsWith>,

    pub caveats: Option<String>,
    pub stage_only: Option<bool>,

    #[serde(default)]
    pub uninstall: Option<HashMap<String, serde_json::Value>>,

    #[serde(default)] // Only one default here
    pub zap: Option<Vec<ZapStanza>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaskList {
    pub casks: Vec<Cask>,
}

// --- ZAP STANZA SUPPORT ---

/// Helper for zap: string or array of strings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringOrVec {
    String(String),
    Vec(Vec<String>),
}
impl StringOrVec {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            StringOrVec::String(s) => vec![s],
            StringOrVec::Vec(v) => v,
        }
    }
}

/// Zap action details (trash, delete, rmdir, pkgutil, launchctl, script, signal, etc)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ZapActionDetail {
    Trash(Vec<String>),
    Delete(Vec<String>),
    Rmdir(Vec<String>),
    Pkgutil(StringOrVec),
    Launchctl(StringOrVec),
    Script {
        executable: String,
        args: Option<Vec<String>>,
    },
    Signal(Vec<String>),
    // Add more as needed
}

/// A zap stanza is a map of action -> detail
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZapStanza(pub std::collections::HashMap<String, ZapActionDetail>);

// --- Cask Impl ---

impl Cask {
    /// Check if this cask is installed by looking for a manifest file
    /// in any versioned directory within the Caskroom.
    pub fn is_installed(&self, config: &Config) -> bool {
        let cask_dir = config.cask_dir(&self.token); // e.g., /opt/homebrew/Caskroom/firefox
        if !cask_dir.exists() || !cask_dir.is_dir() {
            return false;
        }

        // Iterate through entries (version dirs) inside the cask_dir
        match fs::read_dir(&cask_dir) {
            Ok(entries) => {
                // Clippy fix: Use flatten() to handle Result entries directly
                for entry in entries.flatten() {
                    // <-- Use flatten() here
                    let version_path = entry.path();
                    // Check if it's a directory (representing a version)
                    if version_path.is_dir() {
                        // Check for the existence of the manifest file
                        let manifest_path = version_path.join("CASK_INSTALL_MANIFEST.json"); // <-- Correct filename
                        if manifest_path.is_file() {
                            // Check is_installed flag in manifest
                            let mut include = true;
                            if let Ok(manifest_str) = std::fs::read_to_string(&manifest_path) {
                                if let Ok(manifest_json) =
                                    serde_json::from_str::<serde_json::Value>(&manifest_str)
                                {
                                    if let Some(is_installed) =
                                        manifest_json.get("is_installed").and_then(|v| v.as_bool())
                                    {
                                        include = is_installed;
                                    }
                                }
                            }
                            if include {
                                // Found a manifest in at least one version directory, consider it
                                // installed
                                return true;
                            }
                        }
                    }
                }
                // If loop completes without finding a manifest in any version dir
                false
            }
            Err(e) => {
                // Log error if reading the directory fails, but assume not installed
                tracing::warn!(
                    "Failed to read cask directory {} to check for installed versions: {}",
                    cask_dir.display(),
                    e
                );
                false
            }
        }
    }

    /// Get the installed version of this cask by reading the directory names
    /// in the Caskroom. Returns the first version found (use cautiously if multiple
    /// versions could exist, though current install logic prevents this).
    pub fn installed_version(&self, config: &Config) -> Option<String> {
        let cask_dir = config.cask_dir(&self.token); //
        if !cask_dir.exists() {
            return None;
        }
        // Iterate through entries and return the first directory name found
        match fs::read_dir(&cask_dir) {
            Ok(entries) => {
                // Clippy fix: Use flatten()
                for entry in entries.flatten() {
                    // <-- Use flatten() here
                    let path = entry.path();
                    // Check if it's a directory (representing a version)
                    if path.is_dir() {
                        if let Some(version_str) = path.file_name().and_then(|name| name.to_str()) {
                            // Return the first version directory name found
                            return Some(version_str.to_string());
                        }
                    }
                }
                // No version directories found
                None
            }
            Err(_) => None, // Error reading directory
        }
    }

    /// Get a friendly name for display purposes
    pub fn display_name(&self) -> String {
        self.name
            .as_ref()
            .and_then(|names| names.first().cloned())
            .unwrap_or_else(|| self.token.clone())
    }
}
