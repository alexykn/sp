// src/model/cask.rs
// Model for Homebrew casks

use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Represents a Homebrew cask (application)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cask {
    /// Unique identifier for the cask
    pub token: String,

    /// Name of the cask, may be an array
    pub name: Option<Vec<String>>,

    /// Current cask version
    pub version: Option<String>,

    /// Cask description
    pub desc: Option<String>,

    /// Homepage URL
    pub homepage: Option<String>,

    /// Installation artifacts
    pub artifacts: Option<Vec<Artifact>>,

    /// Cask download URLs
    pub url: Option<Vec<String>>,

    /// SHA-256 checksums for download URLs
    pub sha256: Option<String>,

    /// Appcast information
    pub appcast: Option<Appcast>,

    /// Installation method
    pub auto_updates: Option<bool>,

    /// Dependencies
    pub depends_on: Option<Dependencies>,

    /// Conflicts
    pub conflicts_with: Option<Vec<String>>,

    /// Caveats
    pub caveats: Option<String>,

    /// Installation stage
    pub stage_only: Option<bool>,
}

/// Represents an artifact for a cask
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Artifact {
    /// App bundle
    App(String),

    /// Binary file
    Binary(String),

    /// Package installer
    Pkg {
        pkg: String,
        allow_untrusted: Option<bool>,
    },

    /// Generic artifact map
    Map(HashMap<String, serde_json::Value>),
}

/// Represents appcast information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Appcast {
    /// Appcast URL
    pub url: String,

    /// Appcast checkpoint
    pub checkpoint: Option<String>,
}

/// Represents dependencies for a cask
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependencies {
    /// Cask dependencies
    pub cask: Option<Vec<String>>,

    /// Formula dependencies
    pub formula: Option<Vec<String>>,

    /// macOS version dependencies
    pub macos: Option<MacOSRequirement>,
}

/// Represents macOS version requirements
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MacOSRequirement {
    /// Single version requirement
    Single(String),

    /// Multiple version requirements
    Multiple(Vec<String>),
}

/// Represents a list of casks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaskList {
    /// List of casks
    pub casks: Vec<Cask>,
}

impl Cask {
    /// Check if this cask is installed
    pub fn is_installed(&self) -> bool {
        // Check if the cask directory exists in Caskroom
        let caskroom_dir = get_caskroom_dir();
        let cask_dir = caskroom_dir.join(&self.token);
        cask_dir.exists()
    }

    /// Get the installed version of this cask
    pub fn installed_version(&self) -> Option<String> {
        let caskroom_dir = get_caskroom_dir();
        let cask_dir = caskroom_dir.join(&self.token);

        if !cask_dir.exists() {
            return None;
        }

        // Read the cask's subdirectories to find versions
        match std::fs::read_dir(cask_dir) {
            Ok(entries) => {
                // Return the first version directory found
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
            },
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

        // Fall back to token
        self.token.clone()
    }
}

/// Get the Caskroom directory
fn get_caskroom_dir() -> PathBuf {
    // On macOS, Homebrew Caskroom is typically at /opt/homebrew/Caskroom
    // or /usr/local/Caskroom
    if std::path::Path::new("/opt/homebrew/Caskroom").exists() {
        PathBuf::from("/opt/homebrew/Caskroom")
    } else {
        PathBuf::from("/usr/local/Caskroom")
    }
}

// Helper functions can be added here if needed
