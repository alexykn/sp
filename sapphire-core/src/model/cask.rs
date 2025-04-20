// ===== sapphire-core/src/model/cask.rs =====
use crate::utils::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    NoCheck { no_check: bool },
    PerArch(HashMap<String, String>),
}

/// Appcast metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Helper for architecture requirements: single or list
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArchReq {
    One(String),
    Many(Vec<String>),
}

/// Helper for macOS requirements: symbol, list, comparison, or map
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MacOSReq {
    Symbol(String),         // ":big_sur"
    Symbols(Vec<String>),   // [":catalina", ":big_sur"]
    Comparison(String),     // ">= :big_sur"
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default)]
    pub zap: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaskList {
    pub casks: Vec<Cask>,
}

impl Cask {
    /// Check if this cask is installed
    pub fn is_installed(&self, config: &Config) -> bool {
        config.cask_dir(&self.token).exists()
    }

    /// Get the installed version of this cask
    pub fn installed_version(&self, config: &Config) -> Option<String> {
        let cask_dir = config.cask_dir(&self.token);
        if !cask_dir.exists() {
            return None;
        }
        match std::fs::read_dir(&cask_dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    if let Ok(metadata) = entry.metadata() {
                        if metadata.is_dir() {
                            if let Some(ver) = entry.file_name().to_str() {
                                return Some(ver.to_string());
                            }
                        }
                    }
                }
                None
            }
            Err(_) => None,
        }
    }

    /// Get a friendly name for display purposes
    pub fn display_name(&self) -> String {
        self.name
            .as_ref()
            .and_then(|names| names.get(0).cloned())
            .unwrap_or_else(|| self.token.clone())
    }
}
