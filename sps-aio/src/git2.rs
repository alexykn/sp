/*
File: sp-aio/src/git.rs (New File)
Purpose: Synchronous Git operations using git2.
*/
use std::path::Path;

use git2::{FetchOptions, Repository};
use sps_common::error::{Result, SpsError};
use tracing::{debug, error, warn};

/// Updates a Git repository by fetching from 'origin' and fast-forwarding 'master'.
/// Contains blocking network and filesystem I/O.
pub fn update_repo(repo_path: &Path) -> Result<()> {
    debug!("Updating git repository at: {}", repo_path.display());

    let repo = Repository::open(repo_path).map_err(|e| {
        error!("Failed open repo {}: {}", repo_path.display(), e);
        SpsError::Generic(format!("Failed to open tap repository: {e}")) // Keep original context
    })?;

    // Fetch updates from origin
    let mut remote = repo.find_remote("origin").map_err(|e| {
        error!("Failed find remote 'origin' in {}: {}", repo_path.display(), e);
        SpsError::Generic(format!("Failed to find remote 'origin': {e}"))
    })?;

    let mut fetch_options = FetchOptions::new();
    // Add authentication callbacks here if needed (e.g., using ssh-agent or credentials)
    // fetch_options.remote_callbacks(...);

    debug!("Fetching updates for {}", repo_path.display());
    remote
        .fetch(
            &["refs/heads/*:refs/remotes/origin/*"], // Update remote tracking branches
            Some(&mut fetch_options),
            None,
        )
        .map_err(|e| {
            error!("Failed fetch repo {}: {}", repo_path.display(), e);
            SpsError::Generic(format!("Failed to fetch updates: {e}"))
        })?;
    debug!("Fetch complete for {}", repo_path.display());

    // --- Fast-forward local master to origin/master ---
    // Find the remote tracking branch corresponding to the local branch (e.g., master ->
    // origin/master) Assuming the local branch to update is 'master' and it tracks
    // 'origin/master' This might need to be more robust (e.g., check repo.head()).
    let local_branch_name = "refs/heads/master";
    let remote_branch_name = "refs/remotes/origin/master"; // Common default

    let remote_branch_ref = repo.find_reference(remote_branch_name).map_err(|e| {
        error!("Failed find ref '{}': {}", remote_branch_name, e);
        SpsError::Generic(format!(
            "Failed to find remote tracking branch '{}': {}",
            remote_branch_name, e
        ))
    })?;

    let fetch_commit = repo
        .reference_to_annotated_commit(&remote_branch_ref)
        .map_err(|e| {
            error!(
                "Failed get commit from '{}': {}",
                remote_branch_name,
                e
            );
            SpsError::Generic(format!(
                "Failed to get commit from remote tracking branch '{}': {}",
                remote_branch_name, e
            ))
        })?;

    // Analyze merge between local master and origin/master
    let (analysis, _) = repo.merge_analysis(&[&fetch_commit]).map_err(|e| {
        error!("Failed merge analysis: {}", e);
        SpsError::Generic(format!("Failed to analyze merge: {e}"))
    })?;

    if analysis.is_up_to_date() {
        debug!("Repository {} already up-to-date.", repo_path.display());
        return Ok(());
    }

    if analysis.is_fast_forward() {
        debug!(
            "Performing fast-forward merge for '{}' in {}",
            local_branch_name,
            repo_path.display()
        );
        let mut local_ref = repo.find_reference(local_branch_name).map_err(|e| {
            error!("Failed find ref '{}': {}", local_branch_name, e);
            SpsError::Generic(format!("Failed to find local branch '{local_branch_name}': {e}"))
        })?;

        local_ref
            .set_target(
                fetch_commit.id(),
                &format!("Fast-forward {local_branch_name} to origin"),
            )
            .map_err(|e| {
                error!("Failed set target for fast-forward: {}", e);
                SpsError::Generic(format!("Failed to fast-forward: {e}"))
            })?;

        repo.set_head(local_branch_name).map_err(|e| {
            error!("Failed set HEAD to '{}': {}", local_branch_name, e);
            SpsError::Generic(format!("Failed to set HEAD: {e}"))
        })?;

        // Checkout the updated head to update the working directory
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
            .map_err(|e| {
                error!("Failed checkout HEAD after update: {}", e);
                SpsError::Generic(format!("Failed to checkout HEAD: {e}"))
            })?;

        debug!(
            "Successfully fast-forwarded '{}' in {}",
            local_branch_name,
            repo_path.display()
        );
        Ok(())
    } else if analysis.is_normal() {
        // Merge required, but we don't implement automatic merging for taps.
        warn!(
            "Repository {} requires merge, automatic merging not implemented.",
            repo_path.display()
        );
        // Consider returning an error or specific status indicating merge needed
        Err(SpsError::Generic(
            "Tap requires merge but automatic merging is not implemented".to_string(),
        ))
    } else {
        // Other states (e.g., unborn head) - treat as error for now
        error!(
            "Unexpected merge analysis state ({:?}) for {}",
            analysis,
            repo_path.display()
        );
        Err(SpsError::Generic(format!(
            "Unexpected repository state in {}: {:?}",
            repo_path.display(),
            analysis
        )))
    }
}