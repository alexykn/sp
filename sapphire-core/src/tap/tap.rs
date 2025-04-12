// tap/tap.rs - Basic tap functionality

use std::path::PathBuf;
use crate::utils::error::{BrewRsError, Result};

/// Represents a source of packages (formulas and casks)
pub struct Tap {
    /// The user part of the tap name (e.g., "homebrew" in "homebrew/core")
    pub user: String,

    /// The repository part of the tap name (e.g., "core" in "homebrew/core")
    pub repo: String,

    /// The full path to the tap directory
    pub path: PathBuf,
}

impl Tap {
    /// Create a new tap from user/repo format
    pub fn new(name: &str) -> Result<Self> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() != 2 {
            return Err(BrewRsError::Generic(format!("Invalid tap name: {}", name)));
        }

        let user = parts[0].to_string();
        let repo = parts[1].to_string();

        // TODO: Calculate the actual path based on the sapphire prefix
        let path = PathBuf::from("/tmp").join("sapphire/taps").join(&user).join(&repo);

        Ok(Self { user, repo, path })
    }

    /// Get the full name of the tap (user/repo)
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.user, self.repo)
    }

    /// Check if this tap is installed locally
    pub fn is_installed(&self) -> bool {
        self.path.exists()
    }
}
