// In sps-core/src/build/cask/app.rs

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_common::model::artifact::InstalledArtifact;
use sps_common::model::cask::Cask;
use tracing::{debug, error, warn}; // Ensure warn is imported

#[cfg(target_os = "macos")]
use crate::macos::xattr; // Import the xattr utility

/// Finds the primary .app bundle in a directory. Returns an error if none or ambiguous.
/// If multiple .app bundles are found, returns the first and logs a warning.
pub fn find_primary_app_bundle_in_dir(dir: &Path) -> Result<PathBuf> {
    if !dir.is_dir() {
        return Err(SpsError::NotFound(format!(
            "Directory {} not found for app bundle scan.",
            dir.display()
        )));
    }
    let mut app_bundles = Vec::new();
    for entry_res in fs::read_dir(dir)? {
        let entry = entry_res?;
        let path = entry.path();
        if path.is_dir() && path.extension().is_some_and(|ext| ext == "app") {
            app_bundles.push(path);
        }
    }
    if app_bundles.is_empty() {
        Err(SpsError::NotFound(format!(
            "No .app bundle found in {}",
            dir.display()
        )))
    } else if app_bundles.len() == 1 {
        Ok(app_bundles.remove(0))
    } else {
        // Heuristic: return the largest .app bundle if multiple are found, or one matching a common
        // pattern. For now, error if multiple are present to force explicit handling in
        // Cask definitions if needed.
        warn!("Multiple .app bundles found in {}: {:?}. Returning the first one, but this might be ambiguous.", dir.display(), app_bundles);
        Ok(app_bundles.remove(0)) // Or error out
    }
}

/// Installs an app bundle from a staged location to /Applications and creates a symlink in the
/// caskroom. Returns a Vec containing the details of artifacts created.
pub fn install_app_from_staged(
    cask: &Cask,
    staged_app_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    if !staged_app_path.exists() || !staged_app_path.is_dir() {
        return Err(SpsError::NotFound(format!(
            "Staged app bundle for {} not found or is not a directory: {}",
            cask.token,
            staged_app_path.display()
        )));
    }

    let app_name = staged_app_path
        .file_name()
        .ok_or_else(|| {
            SpsError::Generic(format!(
                "Invalid staged app path (no filename): {}",
                staged_app_path.display()
            ))
        })?
        .to_string_lossy();

    // Path for the pristine copy of the app bundle in the private cask store
    let private_store_app_path = config.private_cask_app_path(
        &cask.token,
        &cask.version.clone().unwrap_or_else(|| "latest".to_string()),
        app_name.as_ref(),
    );
    // Final destination for the app bundle
    let final_app_destination_in_applications = config.applications_dir().join(app_name.as_ref());
    // Path for the symlink within the Caskroom that points to the app in /Applications
    let caskroom_symlink_to_final_app = cask_version_install_path.join(app_name.as_ref());

    debug!(
        "Installing app '{}': Staged -> Private Store -> /Applications -> Caskroom Symlink",
        app_name
    );
    debug!("  Staged app source: {}", staged_app_path.display());
    debug!(
        "  Private store copy target: {}",
        private_store_app_path.display()
    );
    debug!(
        "  Final /Applications target: {}",
        final_app_destination_in_applications.display()
    );
    debug!(
        "  Caskroom symlink target: {}",
        caskroom_symlink_to_final_app.display()
    );

    // 1. Ensure Caskroom version path exists
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

    // 2. Create private store directory if it doesn't exist
    if let Some(parent) = private_store_app_path.parent() {
        if !parent.exists() {
            debug!("Creating private store directory: {}", parent.display());
            fs::create_dir_all(parent).map_err(|e| {
                SpsError::Io(std::sync::Arc::new(std::io::Error::new(
                    e.kind(),
                    format!(
                        "Failed to create private store dir {}: {}",
                        parent.display(),
                        e
                    ),
                )))
            })?;
        }
    }

    // 3. Clean existing app in private store (if any from a failed prior attempt)
    if private_store_app_path.exists() || private_store_app_path.symlink_metadata().is_ok() {
        debug!(
            "Removing existing item at private store path: {}",
            private_store_app_path.display()
        );
        let _ = remove_path_robustly(&private_store_app_path, config, false);
    }

    // 4. Move from temporary stage to private store
    debug!(
        "Moving staged app {} to private store path {}",
        staged_app_path.display(),
        private_store_app_path.display()
    );
    if let Err(e) = fs::rename(staged_app_path, &private_store_app_path) {
        error!(
            "Failed to move staged app to private store: {}. Source: {}, Dest: {}",
            e,
            staged_app_path.display(),
            private_store_app_path.display()
        );
        return Err(SpsError::Io(std::sync::Arc::new(e)));
    }

    // 5. Set/Verify Quarantine on private store copy
    #[cfg(target_os = "macos")]
    {
        debug!(
            "Setting/verifying quarantine on private store copy: {}",
            private_store_app_path.display()
        );
        if let Err(e) = xattr::set_quarantine_attribute(&private_store_app_path, &cask.token) {
            error!(
                "Failed to set quarantine on private store copy {}: {}. This is critical.",
                private_store_app_path.display(),
                e
            );
            return Err(e);
        }
    }

    // 6. Clean existing final app destination in /Applications
    if final_app_destination_in_applications.exists()
        || final_app_destination_in_applications
            .symlink_metadata()
            .is_ok()
    {
        debug!(
            "Removing existing app at /Applications: {}",
            final_app_destination_in_applications.display()
        );
        if !remove_path_robustly(&final_app_destination_in_applications, config, true) {
            return Err(SpsError::InstallError(format!(
                "Failed to remove existing app at {}",
                final_app_destination_in_applications.display()
            )));
        }
    }

    // 7. Copy from private store to final /Applications destination
    debug!(
        "Copying app from private store {} to /Applications {}",
        private_store_app_path.display(),
        final_app_destination_in_applications.display()
    );
    // Use cp -Rp to preserve attributes during copy
    let cp_to_apps_output = Command::new("cp")
        .arg("-Rp")
        .arg(&private_store_app_path)
        .arg(&final_app_destination_in_applications)
        .output()?;

    if !cp_to_apps_output.status.success() {
        let stderr = String::from_utf8_lossy(&cp_to_apps_output.stderr);
        error!(
            "Failed to copy app from private store {} to /Applications {} (status: {}): {}.",
            private_store_app_path.display(),
            final_app_destination_in_applications.display(),
            cp_to_apps_output.status,
            stderr.trim()
        );
        return Err(SpsError::InstallError(format!(
            "Failed to copy app from private store to {}: {}",
            final_app_destination_in_applications.display(),
            stderr.trim()
        )));
    }
    debug!("Successfully copied app from private store to /Applications using `cp -Rp`.");

    // 7. Re-Set/Ensure Quarantine on the Final App in /Applications
    #[cfg(target_os = "macos")]
    {
        debug!(
            "Final quarantine check/set on /Applications copy: {}",
            final_app_destination_in_applications.display()
        );
        if let Err(e) =
            xattr::set_quarantine_attribute(&final_app_destination_in_applications, &cask.token)
        {
            error!("CRITICAL: Failed to set final quarantine attribute on {}: {}. This WILL likely cause data loss or Gatekeeper issues.", final_app_destination_in_applications.display(), e);
            let _ = remove_path_robustly(&final_app_destination_in_applications, config, true);
            return Err(e);
        }
    }

    // 8. Create Caskroom Symlink TO the app in /Applications
    let actual_caskroom_symlink_path = cask_version_install_path.join(app_name.as_ref());
    debug!(
        "Creating Caskroom symlink {} -> {}",
        actual_caskroom_symlink_path.display(),
        final_app_destination_in_applications.display()
    );

    if actual_caskroom_symlink_path.symlink_metadata().is_ok() {
        if let Err(e) = fs::remove_file(&actual_caskroom_symlink_path) {
            warn!(
                "Failed to remove existing item at Caskroom symlink path {}: {}. Proceeding.",
                actual_caskroom_symlink_path.display(),
                e
            );
        }
    }

    #[cfg(unix)]
    {
        if let Err(e) = std::os::unix::fs::symlink(
            &final_app_destination_in_applications,
            &actual_caskroom_symlink_path,
        ) {
            error!(
                "Failed to create Caskroom symlink {} -> {}: {}",
                actual_caskroom_symlink_path.display(),
                final_app_destination_in_applications.display(),
                e
            );
            let _ = remove_path_robustly(&final_app_destination_in_applications, config, true);
            return Err(SpsError::Io(std::sync::Arc::new(e)));
        }
    }
    #[cfg(not(unix))]
    {
        warn!(
            "Symlink creation not supported on this platform. Skipping link for {}.",
            actual_caskroom_symlink_path.display()
        );
    }

    let mut created_artifacts = vec![InstalledArtifact::AppBundle {
        path: final_app_destination_in_applications.clone(),
    }];
    created_artifacts.push(InstalledArtifact::CaskroomLink {
        link_path: actual_caskroom_symlink_path,
        target_path: final_app_destination_in_applications.clone(),
    });

    debug!(
        "Successfully installed app artifact: {} (Cask: {})",
        app_name, cask.token
    );
    Ok(created_artifacts)
}

/// Helper function for robust path removal (internal to app.rs or moved to a common util)
fn remove_path_robustly(path: &Path, _config: &Config, use_sudo_if_needed: bool) -> bool {
    if !path.exists() && path.symlink_metadata().is_err() {
        debug!("Path {} not found for removal.", path.display());
        return true;
    }
    let is_dir = path.is_dir()
        && !path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink());
    let removal_op = || -> std::io::Result<()> {
        if is_dir {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    };

    if let Err(e) = removal_op() {
        if e.kind() == std::io::ErrorKind::NotFound {
            return true;
        }
        if use_sudo_if_needed && e.kind() == std::io::ErrorKind::PermissionDenied {
            warn!(
                "Direct removal of {} failed (Permission Denied). Trying with sudo rm -rf.",
                path.display()
            );
            let output = Command::new("sudo").arg("rm").arg("-rf").arg(path).output();
            match output {
                Ok(out) if out.status.success() => {
                    debug!("Successfully removed {} with sudo.", path.display());
                    return true;
                }
                Ok(out) => {
                    error!(
                        "`sudo rm -rf {}` failed ({}): {}",
                        path.display(),
                        out.status,
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                    return false;
                }
                Err(sudo_e) => {
                    error!(
                        "Error executing `sudo rm -rf` for {}: {}",
                        path.display(),
                        sudo_e
                    );
                    return false;
                }
            }
        } else {
            error!("Failed to remove {}: {}", path.display(), e);
            return false;
        }
    }
    debug!("Successfully removed {}.", path.display());
    true
}
