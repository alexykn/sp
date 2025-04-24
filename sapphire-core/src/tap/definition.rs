// tap/tap.rs - Basic tap functionality // Should probably be in model module

use std::path::PathBuf;

use tracing::debug;

use crate::utils::error::{Result, SapphireError};

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
            return Err(SapphireError::Generic(format!("Invalid tap name: {name}")));
        }
        let user = parts[0].to_string();
        let repo = parts[1].to_string();
        let prefix = if cfg!(target_arch = "aarch64") {
            PathBuf::from("/opt/homebrew")
        } else {
            PathBuf::from("/usr/local")
        };
        let path = prefix
            .join("Library/Taps")
            .join(&user)
            .join(format!("homebrew-{repo}"));
        Ok(Self { user, repo, path })
    }

    /// Update this tap by pulling latest changes
    pub fn update(&self) -> Result<()> {
        use git2::{FetchOptions, Repository};

        let repo = Repository::open(&self.path)
            .map_err(|e| SapphireError::Generic(format!("Failed to open tap repository: {e}")))?;

        // Fetch updates from origin
        let mut remote = repo
            .find_remote("origin")
            .map_err(|e| SapphireError::Generic(format!("Failed to find remote 'origin': {e}")))?;

        let mut fetch_options = FetchOptions::new();
        remote
            .fetch(
                &["refs/heads/*:refs/heads/*"],
                Some(&mut fetch_options),
                None,
            )
            .map_err(|e| SapphireError::Generic(format!("Failed to fetch updates: {e}")))?;

        // Merge changes
        let fetch_head = repo
            .find_reference("FETCH_HEAD")
            .map_err(|e| SapphireError::Generic(format!("Failed to find FETCH_HEAD: {e}")))?;

        let fetch_commit = repo
            .reference_to_annotated_commit(&fetch_head)
            .map_err(|e| {
                SapphireError::Generic(format!("Failed to get commit from FETCH_HEAD: {e}"))
            })?;

        let analysis = repo
            .merge_analysis(&[&fetch_commit])
            .map_err(|e| SapphireError::Generic(format!("Failed to analyze merge: {e}")))?;

        if analysis.0.is_up_to_date() {
            debug!("Already up-to-date");
            return Ok(());
        }

        if analysis.0.is_fast_forward() {
            let mut reference = repo.find_reference("refs/heads/master").map_err(|e| {
                SapphireError::Generic(format!("Failed to find master branch: {e}"))
            })?;
            reference
                .set_target(fetch_commit.id(), "Fast-forward")
                .map_err(|e| SapphireError::Generic(format!("Failed to fast-forward: {e}")))?;
            repo.set_head("refs/heads/master")
                .map_err(|e| SapphireError::Generic(format!("Failed to set HEAD: {e}")))?;
            repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
                .map_err(|e| SapphireError::Generic(format!("Failed to checkout: {e}")))?;
        } else {
            return Err(SapphireError::Generic(
                "Tap requires merge but automatic merging is not implemented".to_string(),
            ));
        }

        Ok(())
    }

    /// Remove this tap by deleting its local repository
    pub fn remove(&self) -> Result<()> {
        if !self.path.exists() {
            return Err(SapphireError::NotFound(format!(
                "Tap {} is not installed",
                self.full_name()
            )));
        }
        debug!("Removing tap {}", self.full_name());
        std::fs::remove_dir_all(&self.path).map_err(|e| {
            SapphireError::Generic(format!("Failed to remove tap {}: {}", self.full_name(), e))
        })
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
