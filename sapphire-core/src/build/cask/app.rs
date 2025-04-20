// In sapphire-core/src/build/cask/app.rs

use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use std::fs;
use std::path::Path;
use std::process::Command;
use log::{debug, error, info, warn}; // Added log imports

// --- NEW Function ---
/// Installs an app bundle from a staged location to /Applications and creates a symlink in the caskroom.
/// Returns a Vec containing the paths created (e.g., in /Applications and caskroom) for the receipt.
pub fn install_app_from_staged(
    _cask: &Cask, // <-- Prefix with underscore
    staged_app_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<String>> {

    if !staged_app_path.exists() || !staged_app_path.is_dir() {
         return Err(SapphireError::NotFound(format!(
             "Staged app bundle not found or is not a directory: {}",
             staged_app_path.display()
         )));
     }

    let app_name = staged_app_path
        .file_name()
        .ok_or_else(|| SapphireError::Generic(format!("Invalid staged app path: {}", staged_app_path.display())))?
        .to_string_lossy();

    // Use Config method for Applications directory
    let applications_dir = config.applications_dir();
    let final_app_destination = applications_dir.join(app_name.as_ref());

    info!(
        "Moving app '{}' from stage to {}",
        app_name,
        applications_dir.display()
    );

    // --- Remove Existing Destination ---
    // Use sudo only if necessary. Try direct removal first.
    if final_app_destination.exists() || final_app_destination.symlink_metadata().is_ok() {
        info!("==> Removing existing app at {}", final_app_destination.display());
        let remove_result = if final_app_destination.is_dir() {
            fs::remove_dir_all(&final_app_destination)
        } else {
            fs::remove_file(&final_app_destination) // Remove file or symlink
        };

        if let Err(e) = remove_result {
             // If permission denied or directory not empty (less likely for .app), try sudo
            if e.kind() == std::io::ErrorKind::PermissionDenied || e.kind() == std::io::ErrorKind::DirectoryNotEmpty {
                 warn!("Direct removal failed ({}). Trying with sudo rm -rf...", e);
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
                         "Failed to remove existing app at {}: {}", final_app_destination.display(), stderr
                     )));
                 }
                 info!("Successfully removed existing app with sudo.");
             } else {
                // Different error during removal
                error!("Failed to remove existing app at {}: {}", final_app_destination.display(), e);
                return Err(SapphireError::Io(e));
            }
        } else {
             info!("Successfully removed existing app.");
        }
    }

    // --- Move/Copy from Stage ---
    // Prefer move (`mv`) for speed, but copy (`cp -R`) might be safer if permissions are tricky.
    // Let's use `mv` for now. Use `sudo` if direct move fails due to permissions.
    debug!("Moving staged app {} to {}", staged_app_path.display(), final_app_destination.display());
    let move_output = Command::new("mv")
        .arg(staged_app_path) // Source
        .arg(&final_app_destination) // Destination
        .output()?;

    if !move_output.status.success() {
         // Check if it failed due to cross-device link (can happen if /tmp is separate) or permissions
         let stderr = String::from_utf8_lossy(&move_output.stderr).to_lowercase();
         if stderr.contains("cross-device link") || stderr.contains("operation not permitted") {
             warn!("Direct mv failed ({}), trying cp -R...", stderr);
             // Use copy as fallback
             debug!("Executing: cp -R {} {}", staged_app_path.display(), final_app_destination.display());
             let copy_output = Command::new("cp")
                 .arg("-R") // Recursive copy
                 .arg(staged_app_path)
                 .arg(&final_app_destination)
                 .output()?;
             if !copy_output.status.success() {
                 let copy_stderr = String::from_utf8_lossy(&copy_output.stderr);
                 error!("cp -R failed ({}): {}", copy_output.status, copy_stderr);
                 // Try sudo cp? Maybe too aggressive. Let's error here.
                 return Err(SapphireError::InstallError(format!(
                     "Failed to copy app from stage to {}: {}", final_app_destination.display(), copy_stderr
                 )));
             }
             info!("Successfully copied app using cp -R.");
         } else {
            // Different mv error
             error!("mv command failed ({}): {}", move_output.status, stderr);
             return Err(SapphireError::InstallError(format!(
                 "Failed to move app from stage to {}: {}", final_app_destination.display(), stderr
             )));
         }
    } else {
         info!("Successfully moved app using mv.");
    }


    // --- Create Caskroom Symlink ---
    // This link points from the cask version dir to the actual app in /Applications
    let caskroom_app_link_path = cask_version_install_path.join(app_name.as_ref());
    debug!("Linking {} -> {}", caskroom_app_link_path.display(), final_app_destination.display());

    // Remove any existing file/link at the target link path
    if caskroom_app_link_path.exists() || caskroom_app_link_path.symlink_metadata().is_ok() {
        if let Err(e) = fs::remove_file(&caskroom_app_link_path) {
            // Don't fail the whole install, but log a warning
             warn!("Failed to remove existing item at caskroom link path {}: {}", caskroom_app_link_path.display(), e);
        }
    }

    // Create the symlink
    #[cfg(unix)] // Symlinks are primarily a Unix concept
    {
         if let Err(e) = std::os::unix::fs::symlink(&final_app_destination, &caskroom_app_link_path) {
             // Log error but don't necessarily fail the whole install? Or should we? Let's warn for now.
             warn!(
                 "Failed to create symlink {} -> {}: {}",
                 caskroom_app_link_path.display(),
                 final_app_destination.display(),
                 e
             );
             // Return Err(SapphireError::Io(e)) ?
         } else {
              debug!("Successfully created caskroom link.");
         }
     }
     #[cfg(not(unix))] {
         warn!("Symlink creation not supported on this platform. Skipping link for {}.", caskroom_app_link_path.display());
     }

    // Prepare list of created paths for the receipt
    let created_paths = vec![
        final_app_destination.to_string_lossy().to_string(),
        caskroom_app_link_path.to_string_lossy().to_string(),
    ];

    // Note: Receipt writing is now handled in the main `install_cask` function
    // after all artifacts for a cask are processed. We just return the paths.

    info!("Successfully installed app: {}", app_name);
    Ok(created_paths)
}
