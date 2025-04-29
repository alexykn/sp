/*
File: sp-aio/src/uninstall.rs (New File)
Purpose: Primitive uninstall operations (filesystem, pkgutil, launchctl).
*/
use std::{
    io,
    path::Path,
    process::{Command, Stdio},
};

use sps_common::{
    config::Config, // Depends on sp-common
    error::{Result, SpsError},
};
use tracing::{debug, error, warn};

use crate::fs as sp_fs; // Use helpers from sp_aio::fs

/// Removes a filesystem path (file, symlink, or directory recursively).
/// Uses sudo rm -rf if direct removal fails with PermissionDenied and use_sudo is true.
pub fn remove_path(path: &Path, use_sudo: bool) -> Result<()> {
    match sp_fs::get_symlink_metadata(path) {
        Ok(metadata) => {
            let is_dir = metadata.file_type().is_dir();
            let path_type = if is_dir {
                "directory"
            } else if metadata.file_type().is_symlink() {
                "symlink"
            } else {
                "file"
            };
            debug!("Removing {} at: {}", path_type, path.display());

            let remove_result = if is_dir {
                sp_fs::remove_directory_recursive(path)
            } else {
                sp_fs::remove_file(path)
            };

            match remove_result {
                Ok(()) => {
                    debug!("Successfully removed {}: {}", path_type, path.display());
                    Ok(())
                }
                Err(SpsError::Io(io_err))
                    if use_sudo && io_err.kind() == io::ErrorKind::PermissionDenied =>
                {
                    warn!(
                        "Direct removal failed (Permission Denied). Trying with sudo rm -rf: {}",
                        path.display()
                    );
                    // Blocking external command execution
                    let output = Command::new("sudo").arg("rm").arg("-rf").arg(path).output();
                    match output {
                        Ok(out) if out.status.success() => {
                            debug!("Successfully removed {} with sudo.", path.display());
                            Ok(())
                        }
                        Ok(out) => {
                            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                            error!("Failed to remove {} with sudo: {}", path.display(), stderr);
                            Err(SpsError::IoError(format!(
                                "sudo rm -rf failed: {stderr}"
                            ))) // More specific error
                        }
                        Err(sudo_err) => {
                            error!(
                                "Error executing sudo rm for {}: {}",
                                path.display(),
                                sudo_err
                            );
                            Err(SpsError::from(sudo_err)) // Wrap the execution error
                        }
                    }
                }
                Err(SpsError::Io(io_err)) if io_err.kind() == io::ErrorKind::NotFound => {
                    debug!("Path {} already removed.", path.display());
                    Ok(()) // Treat NotFound as success for uninstall
                }
                Err(e) => {
                    error!("Failed to remove artifact {}: {}", path.display(), e);
                    Err(e) // Propagate other errors
                }
            }
        }
        Err(SpsError::Io(io_err)) if io_err.kind() == io::ErrorKind::NotFound => {
            debug!("Path not found (already removed?): {}", path.display());
            Ok(()) // Treat NotFound as success for uninstall
        }
        Err(e) => {
            warn!("Failed to get metadata for artifact {}: {}", path.display(), e);
            Err(e) // Propagate other errors
        }
    }
}

/// Forgets a package receipt using `pkgutil`. Requires sudo.
pub fn forget_pkgutil(id: &str) -> Result<()> {
    // Basic validation
    if id.contains('/') || id.contains("..") {
        let msg = format!("Invalid pkgutil receipt id contains disallowed characters: {id}");
        error!(msg);
        return Err(SpsError::ValidationError(msg));
    }
    debug!("Forgetting package receipt (requires sudo): {}", id);

    // Blocking external command execution
    let output = Command::new("sudo")
        .arg("pkgutil")
        .arg("--forget")
        .arg(id)
        .stderr(Stdio::piped()) // Capture stderr to check for "No receipt found"
        .output();

    match output {
        Ok(out) if out.status.success() => {
            debug!("Successfully forgot package receipt {}", id);
            Ok(())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if stderr.contains("No receipt for") {
                debug!("Package receipt {} already forgotten or never existed.", id);
                Ok(()) // Treat "not found" as success for uninstall idempotency
            } else {
                error!("Failed to forget package receipt {}: {}", id, stderr);
                Err(SpsError::CommandExecError(format!(
                    "pkgutil --forget failed: {stderr}"
                )))
            }
        }
        Err(e) => {
            error!("Failed to execute sudo pkgutil --forget {}: {}", id, e);
            Err(SpsError::from(e))
        }
    }
}

/// Unloads a launchd service/agent and optionally removes its plist file.
pub fn unload_launchd(label: &str, plist_path: Option<&Path>, _config: &Config) -> Result<()> {
    // Basic validation
    if label.contains('/') || label.contains("..") {
        let msg = format!("Invalid launchd label contains disallowed characters: {label}");
        error!(msg);
        return Err(SpsError::ValidationError(msg));
    }
    if let Some(p) = plist_path {
        if p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            let msg = format!("Invalid launchd plist path contains '..': {}", p.display());
            error!(msg);
            return Err(SpsError::ValidationError(msg));
        }
        // Could add more safety checks based on config locations if needed
    }

    debug!("Unloading launchd agent/daemon (requires sudo): {}", label);
    let mut first_error: Option<SpsError> = None;

    // Blocking external command execution
    let unload_output = Command::new("sudo")
        .arg("launchctl")
        .arg("unload")
        .arg("-w") // -w removes it from future loads as well
        .arg(label)
        .stderr(Stdio::piped())
        .output();

    match unload_output {
        Ok(out) => {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                // Don't error out if it wasn't loaded or found
                if !stderr.contains("Could not find specified service")
                    && !stderr.contains("service not loaded")
                    && !stderr.contains("No such process")
                    && !stderr.contains("launchctl unload error") // Generic error check
                    && !stderr.is_empty() // Ignore empty stderr
                {
                    warn!("Failed to unload launchd item {}: {}", label, stderr);
                    // Store the error, but continue to attempt plist removal
                    if first_error.is_none() {
                        first_error = Some(SpsError::CommandExecError(format!(
                            "launchctl unload failed: {stderr}"
                        )));
                    }
                } else {
                    debug!("Launchd item {} already unloaded or not found.", label);
                }
            } else {
                debug!("Successfully unloaded launchd item {}.", label);
            }
        }
        Err(e) => {
            error!(
                "Failed to execute sudo launchctl unload for {}: {}",
                label, e
            );
            // Store the error and continue
            if first_error.is_none() {
                first_error = Some(SpsError::from(e));
            }
        }
    }

    // Attempt to remove plist file if path provided
    if let Some(p) = plist_path {
        // Determine if sudo needed based on typical locations
        let use_sudo = p.starts_with("/Library/LaunchDaemons")
            || p.starts_with("/Library/LaunchAgents");
        debug!("Attempting removal of launchd plist: {}", p.display());
        if let Err(e) = remove_path(p, use_sudo) {
            // Log failure but don't necessarily overwrite the unload error
            warn!("Failed to remove launchd plist file {}: {}", p.display(), e);
            if first_error.is_none() {
                first_error = Some(e);
            }
        }
    }

    // Return the first error encountered, or Ok(())
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
