// In sps-core/src/build/cask/app.rs

use std::fs;
use std::path::Path;
use std::process::Command;

use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_common::model::artifact::InstalledArtifact;
use sps_common::model::cask::Cask;
use tracing::{debug, error, warn}; // Ensure warn is imported

#[cfg(target_os = "macos")]
use crate::macos::xattr; // Import the xattr utility

/// Installs an app bundle from a staged location to /Applications and creates a symlink in the
/// caskroom. Returns a Vec containing the details of artifacts created.
pub fn install_app_from_staged(
    _cask: &Cask, // Keep cask for potential future use (e.g., specific app flags)
    staged_app_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    // <-- Return type changed

    if !staged_app_path.exists() || !staged_app_path.is_dir() {
        return Err(SpsError::NotFound(format!(
            "Staged app bundle not found or is not a directory: {}",
            staged_app_path.display()
        )));
    }

    let app_name = staged_app_path
        .file_name()
        .ok_or_else(|| {
            SpsError::Generic(format!(
                "Invalid staged app path: {}",
                staged_app_path.display()
            ))
        })?
        .to_string_lossy();

    let applications_dir = config.applications_dir();
    let final_app_destination = applications_dir.join(app_name.as_ref());

    debug!(
        "Preparing to install app '{}' from stage {} to {}",
        app_name,
        staged_app_path.display(),
        final_app_destination.display()
    );

    // --- Ensure parent of Caskroom link path exists ---
    // The cask_version_install_path is the parent for the symlink.
    if !cask_version_install_path.exists() {
        fs::create_dir_all(cask_version_install_path).map_err(|e| {
            SpsError::Io(std::sync::Arc::new(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to create cask version dir {}: {}",
                    cask_version_install_path.display(),
                    e
                ),
            )))
        })?;
    }

    // --- Remove Existing Destination in /Applications ---
    if final_app_destination.exists() || final_app_destination.symlink_metadata().is_ok() {
        debug!(
            "Removing existing app at {}",
            final_app_destination.display()
        );
        let remove_result = if final_app_destination.is_dir()
            && !final_app_destination
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        {
            fs::remove_dir_all(&final_app_destination)
        } else {
            fs::remove_file(&final_app_destination) // Remove file or symlink
        };

        if let Err(e) = remove_result {
            // Try with sudo only if direct removal fails with specific errors
            if e.kind() == std::io::ErrorKind::PermissionDenied
                || e.kind() == std::io::ErrorKind::DirectoryNotEmpty
            {
                warn!(
                    "Direct removal of {} failed ({}). Trying with sudo rm -rf.",
                    final_app_destination.display(),
                    e
                );
                let output = Command::new("sudo")
                    .arg("rm")
                    .arg("-rf")
                    .arg(&final_app_destination)
                    .output()?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    error!(
                        "sudo rm -rf {} failed ({}): {}",
                        final_app_destination.display(),
                        output.status,
                        stderr.trim()
                    );
                    return Err(SpsError::InstallError(format!(
                        "Failed to remove existing app at {}: {}",
                        final_app_destination.display(),
                        stderr.trim()
                    )));
                }
                debug!(
                    "Successfully removed existing app {} with sudo.",
                    final_app_destination.display()
                );
            } else {
                error!(
                    "Failed to remove existing app at {}: {}",
                    final_app_destination.display(),
                    e
                );
                return Err(SpsError::Io(std::sync::Arc::new(e)));
            }
        } else {
            debug!(
                "Successfully removed existing app at {}.",
                final_app_destination.display()
            );
        }
    }

    // --- Move/Copy from Stage to /Applications ---
    debug!(
        "Moving staged app {} to {}",
        staged_app_path.display(),
        final_app_destination.display()
    );
    // Prefer `mv` for speed and atomicity (on same filesystem), but handle cross-device/permission
    // issues.
    let move_output = Command::new("mv")
        .arg(staged_app_path)
        .arg(&final_app_destination)
        .output()?;

    if !move_output.status.success() {
        let mv_stderr = String::from_utf8_lossy(&move_output.stderr).to_lowercase();
        // Typical errors for `mv` that might warrant a `cp -R` fallback:
        // "cross-device link", "operation not permitted", "permission denied" (though permissions
        // for target dir should be checked first)
        if mv_stderr.contains("cross-device link")
            || mv_stderr.contains("operation not permitted")
            || mv_stderr.contains("permission denied")
        {
            warn!(
                "Direct `mv` failed ({}). Attempting `cp -R`.",
                mv_stderr.trim()
            );
            let copy_output = Command::new("cp")
                .arg("-R") // Recursive copy for directories
                .arg(staged_app_path)
                .arg(&final_app_destination)
                .output()?;
            if !copy_output.status.success() {
                let cp_stderr = String::from_utf8_lossy(&copy_output.stderr);
                error!(
                    "`cp -R` from {} to {} failed ({}): {}",
                    staged_app_path.display(),
                    final_app_destination.display(),
                    copy_output.status,
                    cp_stderr.trim()
                );
                return Err(SpsError::InstallError(format!(
                    "Failed to copy app from stage to {}: {}",
                    final_app_destination.display(),
                    cp_stderr.trim()
                )));
            }
            debug!(
                "Successfully copied app to {} using `cp -R`.",
                final_app_destination.display()
            );
            // If cp was used, the original staged_app_path still exists.
            // It should be cleaned up by the TempDir RAII guard when `stage_dir` (from
            // `build/cask/mod.rs`) goes out of scope.
        } else {
            error!(
                "`mv` command failed to move {} to {} ({}): {}",
                staged_app_path.display(),
                final_app_destination.display(),
                move_output.status,
                mv_stderr.trim()
            );
            return Err(SpsError::InstallError(format!(
                "Failed to move app from stage to {}: {}",
                final_app_destination.display(),
                mv_stderr.trim()
            )));
        }
    } else {
        debug!(
            "Successfully moved app to {} using `mv`.",
            final_app_destination.display()
        );
    }

    // --- Set Quarantine Attribute on the final app bundle (P1 Fix) ---
    #[cfg(target_os = "macos")]
    {
        // Use the cask token as the agent name for the quarantine attribute
        let agent_name = &_cask.token;
        if let Err(e) = xattr::set_quarantine_attribute(&final_app_destination, agent_name) {
            // This is now an error that propagates, as it's critical for fixing the data loss bug.
            error!("CRITICAL: Failed to set quarantine attribute on {}: {}. This WILL likely cause data loss or Gatekeeper issues.", final_app_destination.display(), e);
            return Err(e);
        }
    }

    // --- Record the main app artifact ---
    let mut created_artifacts = vec![InstalledArtifact::AppBundle {
        path: final_app_destination.clone(),
    }];

    // --- Create Caskroom Symlink (references the app in /Applications) ---
    let caskroom_app_link_path = cask_version_install_path.join(app_name.as_ref());
    debug!(
        "Linking {} -> {}",
        caskroom_app_link_path.display(),
        final_app_destination.display()
    );

    // Remove existing symlink in Caskroom if it exists
    if caskroom_app_link_path.symlink_metadata().is_ok() {
        // Check if it's a symlink or file
        if let Err(e) = fs::remove_file(&caskroom_app_link_path) {
            // remove_file works for symlinks too
            warn!(
                "Failed to remove existing item at caskroom link path {}: {}. Proceeding with link creation attempt.",
                caskroom_app_link_path.display(),
                e
            );
        }
    }

    #[cfg(unix)] // Symlinks are primarily a Unix concept
    {
        if let Err(e) = std::os::unix::fs::symlink(&final_app_destination, &caskroom_app_link_path)
        {
            // This is an important reference for sps to manage the cask.
            error!(
                "Failed to create symlink {} -> {}: {}",
                caskroom_app_link_path.display(),
                final_app_destination.display(),
                e
            );
            return Err(SpsError::Io(std::sync::Arc::new(e))); // Fail if symlink creation fails.
        } else {
            debug!("Successfully created caskroom symlink.");
            created_artifacts.push(InstalledArtifact::CaskroomLink {
                link_path: caskroom_app_link_path.clone(),
                target_path: final_app_destination.clone(),
            });
        }
    }
    #[cfg(not(unix))]
    {
        warn!(
            // Changed to warn as this is a non-critical feature on non-Unix
            "Symlink creation not supported on this platform. Skipping link for {}.",
            caskroom_app_link_path.display()
        );
    }

    debug!("Successfully installed app artifact: {}", app_name);
    Ok(created_artifacts)
}
