// sps-aio/src/git2.rs
use std::path::Path;

use sps_common::error::{Result, SpsError};
use tokio::task; // Import Tokio task module
use tracing::{debug, error, instrument, warn}; // Use instrument

// Keep the original synchronous function
#[instrument(skip(repo_path), fields(repo = %repo_path.display()))]
fn update_repo_sync_internal(repo_path: &Path) -> Result<()> {
    debug!("Sync Updating git repository"); // Add Sync marker

    // --- The existing synchronous git2 logic ---
    let repo = git2::Repository::open(repo_path).map_err(|e| {
        error!("Sync Failed open repo: {}", e);
        SpsError::Generic(format!("Failed to open tap repository: {e}"))
    })?;

    let mut remote = repo.find_remote("origin").map_err(|e| {
        error!("Sync Failed find remote 'origin': {}", e);
        SpsError::Generic(format!("Failed to find remote 'origin': {e}"))
    })?;

    let mut fetch_options = git2::FetchOptions::new();
    // TODO: Add authentication callbacks if needed
    // fetch_options.remote_callbacks(...);

    debug!("Sync Fetching updates");
    remote
        .fetch(
            &["refs/heads/*:refs/remotes/origin/*"],
            Some(&mut fetch_options),
            None,
        )
        .map_err(|e| {
            error!("Sync Failed fetch repo: {}", e);
            SpsError::Generic(format!("Failed to fetch updates: {e}"))
        })?;
    debug!("Sync Fetch complete");

    let local_branch_name = "refs/heads/master";
    let remote_branch_name = "refs/remotes/origin/master";

    let remote_branch_ref = repo.find_reference(remote_branch_name).map_err(|e| {
        error!("Sync Failed find ref '{}': {}", remote_branch_name, e);
        SpsError::Generic(format!(
            "Failed to find remote tracking branch '{remote_branch_name}': {e}"
        ))
    })?;

    let fetch_commit = repo
        .reference_to_annotated_commit(&remote_branch_ref)
        .map_err(|e| {
            error!(
                "Sync Failed get commit from '{}': {}",
                remote_branch_name, e
            );
            SpsError::Generic(format!(
                "Failed to get commit from remote tracking branch '{remote_branch_name}': {e}"
            ))
        })?;

    let (analysis, _) = repo.merge_analysis(&[&fetch_commit]).map_err(|e| {
        error!("Sync Failed merge analysis: {}", e);
        SpsError::Generic(format!("Failed to analyze merge: {e}"))
    })?;

    if analysis.is_up_to_date() {
        debug!("Sync Repository already up-to-date.");
        return Ok(());
    }

    if analysis.is_fast_forward() {
        debug!(
            "Sync Performing fast-forward merge for '{}'",
            local_branch_name
        );
        let mut local_ref = repo.find_reference(local_branch_name).map_err(|e| {
            error!("Sync Failed find ref '{}': {}", local_branch_name, e);
            SpsError::Generic(format!(
                "Failed to find local branch '{local_branch_name}': {e}"
            ))
        })?;

        local_ref
            .set_target(
                fetch_commit.id(),
                &format!("Fast-forward {local_branch_name} to origin"),
            )
            .map_err(|e| {
                error!("Sync Failed set target for fast-forward: {}", e);
                SpsError::Generic(format!("Failed to fast-forward: {e}"))
            })?;

        repo.set_head(local_branch_name).map_err(|e| {
            error!("Sync Failed set HEAD to '{}': {}", local_branch_name, e);
            SpsError::Generic(format!("Failed to set HEAD: {e}"))
        })?;

        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
            .map_err(|e| {
                error!("Sync Failed checkout HEAD after update: {}", e);
                SpsError::Generic(format!("Failed to checkout HEAD: {e}"))
            })?;

        debug!("Sync Successfully fast-forwarded '{}'", local_branch_name);
        Ok(())
    } else if analysis.is_normal() {
        warn!("Sync Repository requires merge, automatic merging not implemented.");
        Err(SpsError::Generic(
            "Tap requires merge but automatic merging is not implemented".to_string(),
        ))
    } else {
        error!("Sync Unexpected merge analysis state ({:?})", analysis);
        Err(SpsError::Generic(format!(
            "Unexpected repository state: {analysis:?}"
        )))
    }
    // --- End of existing synchronous logic ---
}

/// Asynchronously updates a Git repository by running the synchronous git2 logic
/// within `spawn_blocking`.
#[instrument(skip(repo_path), fields(repo = %repo_path.display()))]
pub async fn update_repo_async(repo_path: &Path) -> Result<()> {
    debug!("Async Updating git repository");
    let repo_path_owned = repo_path.to_path_buf(); // Clone repo_path for the closure

    // Spawn the synchronous git2 operations on Tokio's blocking thread pool
    task::spawn_blocking(move || update_repo_sync_internal(&repo_path_owned))
        .await
        .map_err(|e| SpsError::Generic(format!("Git update task failed: {e}")))?
    // Handle JoinError
    // The inner Result<() is the result from update_repo_sync_internal
}

// Keep the original sync function exported if needed
pub fn update_repo_sync(repo_path: &Path) -> Result<()> {
    update_repo_sync_internal(repo_path)
}
