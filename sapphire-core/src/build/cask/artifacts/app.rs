// In sapphire-core/src/build/cask/app.rs

use std::fs;
use std::path::Path;
use std::process::Command;

use tracing::{debug, error}; // Added log imports

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};

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
        return Err(SapphireError::NotFound(format!(
            "Staged app bundle not found or is not a directory: {}",
            staged_app_path.display()
        )));
    }

    let app_name = staged_app_path
        .file_name()
        .ok_or_else(|| {
            SapphireError::Generic(format!(
                "Invalid staged app path: {}",
                staged_app_path.display()
            ))
        })?
        .to_string_lossy();

    let applications_dir = config.applications_dir();
    let final_app_destination = applications_dir.join(app_name.as_ref());

    debug!(
        "Moving app '{}' from stage to {}",
        app_name,
        applications_dir.display()
    );

    // --- Remove Existing Destination ---
    if final_app_destination.exists() || final_app_destination.symlink_metadata().is_ok() {
        debug!(
            "Removing existing app at {}",
            final_app_destination.display()
        );
        let remove_result = if final_app_destination.is_dir() {
            fs::remove_dir_all(&final_app_destination)
        } else {
            fs::remove_file(&final_app_destination) // Remove file or symlink
        };

        if let Err(e) = remove_result {
            if e.kind() == std::io::ErrorKind::PermissionDenied
                || e.kind() == std::io::ErrorKind::DirectoryNotEmpty
            {
                debug!("Direct removal failed ({}). Trying with sudo rm -rf...", e);
                debug!("Executing: sudo rm -rf {}", final_app_destination.display());
                let output = Command::new("sudo")
                    .arg("rm")
                    .arg("-rf")
                    .arg(&final_app_destination)
                    .output()?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    error!("sudo rm -rf failed ({}): {}", output.status, stderr);
                    return Err(SapphireError::InstallError(format!(
                        "Failed to remove existing app at {}: {}",
                        final_app_destination.display(),
                        stderr
                    )));
                }
                debug!("Successfully removed existing app with sudo.");
            } else {
                error!(
                    "Failed to remove existing app at {}: {}",
                    final_app_destination.display(),
                    e
                );
                return Err(SapphireError::Io(e));
            }
        } else {
            debug!("Successfully removed existing app.");
        }
    }

    // --- Move/Copy from Stage ---
    debug!(
        "Moving staged app {} to {}",
        staged_app_path.display(),
        final_app_destination.display()
    );
    let move_output = Command::new("mv")
        .arg(staged_app_path) // Source
        .arg(&final_app_destination) // Destination
        .output()?;

    if !move_output.status.success() {
        let stderr = String::from_utf8_lossy(&move_output.stderr).to_lowercase();
        if stderr.contains("cross-device link")
            || stderr.contains("operation not permitted")
            || stderr.contains("permission denied")
        {
            debug!("Direct mv failed ({}), trying cp -R...", stderr);
            debug!(
                "Executing: cp -R {} {}",
                staged_app_path.display(),
                final_app_destination.display()
            );
            let copy_output = Command::new("cp")
                .arg("-R") // Recursive copy
                .arg(staged_app_path)
                .arg(&final_app_destination)
                .output()?;
            if !copy_output.status.success() {
                let copy_stderr = String::from_utf8_lossy(&copy_output.stderr);
                error!("cp -R failed ({}): {}", copy_output.status, copy_stderr);
                return Err(SapphireError::InstallError(format!(
                    "Failed to copy app from stage to {}: {}",
                    final_app_destination.display(),
                    copy_stderr
                )));
            }
            debug!("Successfully copied app using cp -R.");
        } else {
            error!("mv command failed ({}): {}", move_output.status, stderr);
            return Err(SapphireError::InstallError(format!(
                "Failed to move app from stage to {}: {}",
                final_app_destination.display(),
                stderr
            )));
        }
    } else {
        debug!("Successfully moved app using mv.");
    }

    // --- Record the main app artifact ---
    let mut created_artifacts = vec![InstalledArtifact::App {
        path: final_app_destination.clone(),
    }];

    // --- Create Caskroom Symlink ---
    let caskroom_app_link_path = cask_version_install_path.join(app_name.as_ref());
    debug!(
        "Linking {} -> {}",
        caskroom_app_link_path.display(),
        final_app_destination.display()
    );

    if caskroom_app_link_path.exists() || caskroom_app_link_path.symlink_metadata().is_ok() {
        if let Err(e) = fs::remove_file(&caskroom_app_link_path) {
            debug!(
                "Failed to remove existing item at caskroom link path {}: {}",
                caskroom_app_link_path.display(),
                e
            );
        }
    }

    #[cfg(unix)]
    {
        if let Err(e) = std::os::unix::fs::symlink(&final_app_destination, &caskroom_app_link_path)
        {
            debug!(
                "Failed to create symlink {} -> {}: {}",
                caskroom_app_link_path.display(),
                final_app_destination.display(),
                e
            );
            // Decide if this should be a fatal error or just a warning
            // For now, let's just warn and continue.
        } else {
            debug!("Successfully created caskroom link.");
            // Record the link artifact if created successfully
            created_artifacts.push(InstalledArtifact::CaskroomLink {
                link_path: caskroom_app_link_path.clone(),
                target_path: final_app_destination.clone(),
            });
        }
    }
    #[cfg(not(unix))]
    {
        debug!(
            "Symlink creation not supported on this platform. Skipping link for {}.",
            caskroom_app_link_path.display()
        );
    }

    debug!("Successfully installed app artifact: {}", app_name);
    Ok(created_artifacts) // <-- Return the collected artifacts
}
