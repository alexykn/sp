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

    // Path for the "master" copy of the app bundle within the Caskroom
    let caskroom_app_master_path = cask_version_install_path.join(app_name.as_ref());
    // Final destination for the app bundle
    let final_app_destination_in_applications = config.applications_dir().join(app_name.as_ref());
    // Path for the symlink within the Caskroom that points to the app in /Applications
    let caskroom_symlink_to_final_app = cask_version_install_path.join(app_name.as_ref());

    debug!(
        "Installing app '{}': Staged -> Caskroom Master -> /Applications -> Caskroom Symlink",
        app_name
    );
    debug!("  Staged app source: {}", staged_app_path.display());
    debug!(
        "  Caskroom master copy target: {}",
        caskroom_app_master_path.display()
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

    // 2. Clean existing Caskroom master path (if any from a failed prior attempt)
    if caskroom_app_master_path.exists() || caskroom_app_master_path.symlink_metadata().is_ok() {
        debug!(
            "Removing existing item at Caskroom master path: {}",
            caskroom_app_master_path.display()
        );
        let _ = remove_path_robustly(&caskroom_app_master_path, config, false);
    }

    // 3. Move from temporary stage to Caskroom master path
    debug!(
        "Moving staged app {} to Caskroom master path {}",
        staged_app_path.display(),
        caskroom_app_master_path.display()
    );
    if let Err(e) = fs::rename(staged_app_path, &caskroom_app_master_path) {
        error!(
            "Failed to move staged app to Caskroom: {}. Source: {}, Dest: {}",
            e,
            staged_app_path.display(),
            caskroom_app_master_path.display()
        );
        return Err(SpsError::Io(std::sync::Arc::new(e)));
    }

    // 4. Set/Verify Quarantine on Caskroom master copy
    #[cfg(target_os = "macos")]
    {
        debug!(
            "Setting/verifying quarantine on Caskroom master copy: {}",
            caskroom_app_master_path.display()
        );
        if let Err(e) = xattr::set_quarantine_attribute(&caskroom_app_master_path, &cask.token) {
            error!(
                "Failed to set quarantine on Caskroom master copy {}: {}. This is critical.",
                caskroom_app_master_path.display(),
                e
            );
            return Err(e);
        }
    }

    // 5. Clean existing final app destination in /Applications
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

    // 6. Move from Caskroom master to final /Applications destination
    debug!(
        "Moving app from Caskroom master {} to /Applications {}",
        caskroom_app_master_path.display(),
        final_app_destination_in_applications.display()
    );
    let mv_to_apps_output = Command::new("mv")
        .arg(&caskroom_app_master_path)
        .arg(&final_app_destination_in_applications)
        .output()?;

    if !mv_to_apps_output.status.success() {
        let stderr = String::from_utf8_lossy(&mv_to_apps_output.stderr);
        error!(
            "Failed to move app from Caskroom master {} to /Applications {} (status: {}): {}. Attempting copy as fallback.",
            caskroom_app_master_path.display(),
            final_app_destination_in_applications.display(),
            mv_to_apps_output.status,
            stderr.trim()
        );
        let cp_output = Command::new("cp")
            .arg("-R")
            .arg(&caskroom_app_master_path)
            .arg(&final_app_destination_in_applications)
            .output()?;
        if !cp_output.status.success() {
            let cp_stderr = String::from_utf8_lossy(&cp_output.stderr);
            error!(
                "`cp -R` from Caskroom to /Applications also failed (status: {}): {}",
                cp_output.status,
                cp_stderr.trim()
            );
            return Err(SpsError::InstallError(format!(
                "Failed to move or copy app from Caskroom to {}: {}",
                final_app_destination_in_applications.display(),
                cp_stderr.trim()
            )));
        }
        debug!("Successfully copied app from Caskroom to /Applications using `cp -R`.");
        let _ = fs::remove_dir_all(&caskroom_app_master_path);
    } else {
        debug!("Successfully moved app from Caskroom master to /Applications using `mv`.");
    }

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
    if !path.exists() && !path.symlink_metadata().is_ok() {
        debug!("Path {} not found for removal.", path.display());
        return true;
    }
    let is_dir = path.is_dir()
        && !path
            .symlink_metadata()
            .map_or(false, |m| m.file_type().is_symlink());
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
