use std::fs;
use std::path::Path;
use std::process::Command; // Added for rsync

use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use tracing::{debug, error};

/// Synchronizes the content of a source app bundle into a destination app bundle directory using
/// rsync. If the destination does not exist, it behaves like a move.
/// This is intended for cask upgrades to preserve user data within the bundle.
pub fn sync_app_bundle_contents(
    source_app_path: &Path,
    destination_app_path: &Path,
    _config: &Config,
) -> Result<()> {
    debug!(
        "Syncing app bundle contents from {} to {}",
        source_app_path.display(),
        destination_app_path.display()
    );

    if !source_app_path.exists() || !source_app_path.is_dir() {
        return Err(SpsError::NotFound(format!(
            "Source app bundle for sync not found or not a directory: {}",
            source_app_path.display()
        )));
    }

    // Ensure parent of destination exists for rsync.
    if let Some(parent) = destination_app_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| SpsError::Io(std::sync::Arc::new(e)))?;
        }
    } else {
        // Should not happen for typical app paths but good to be safe
        return Err(SpsError::Generic(format!(
            "Destination app path {} has no parent",
            destination_app_path.display()
        )));
    }

    // rsync -a --delete source_app_path/ destination_app_path/
    let rsync_source = format!("{}/", source_app_path.to_string_lossy());
    let rsync_dest = format!("{}/", destination_app_path.to_string_lossy());

    debug!(
        "Executing rsync -a --delete \"{}\" \"{}\"",
        rsync_source, rsync_dest
    );
    let status = Command::new("rsync")
        .arg("-a") // archive mode: recursive, preserves symlinks, perms, times, group, owner, devices
        .arg("--delete") // delete extraneous files from dest dirs (making it a true sync)
        .arg(&rsync_source)
        .arg(&rsync_dest)
        .status()
        .map_err(|e| SpsError::CommandExecError(format!("Failed to execute rsync: {e}")))?;

    if !status.success() {
        error!("rsync command failed with status: {:?}", status);
        return Err(SpsError::InstallError(format!(
            "rsync failed to sync app bundle from {} to {}",
            source_app_path.display(),
            destination_app_path.display()
        )));
    }

    debug!(
        "Successfully synced app bundle contents to {}",
        destination_app_path.display()
    );
    Ok(())
} // Added error, warn

/// Robustly removes a file or directory, handling symlinks and permissions.
/// If `use_sudo_if_needed` is true, will attempt `sudo rm -rf` on permission errors.
pub fn remove_path_robustly(path: &Path, _config: &Config, use_sudo_if_needed: bool) -> bool {
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
            debug!(
                "Direct removal of {} failed (Permission Denied). Trying with sudo rm -rf.",
                path.display()
            );
            let output = std::process::Command::new("sudo")
                .arg("rm")
                .arg("-rf")
                .arg(path)
                .output();
            match output {
                Ok(out) if out.status.success() => {
                    debug!("Successfully removed {} with sudo.", path.display());
                    return true;
                }
                Ok(out) => {
                    debug!(
                        "`sudo rm -rf {}` failed ({}): {}",
                        path.display(),
                        out.status,
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                    return false;
                }
                Err(sudo_e) => {
                    debug!(
                        "Error executing `sudo rm -rf` for {}: {}",
                        path.display(),
                        sudo_e
                    );
                    return false;
                }
            }
        } else {
            debug!("Failed to remove {}: {}", path.display(), e);
            return false;
        }
    }
    debug!("Successfully removed {}.", path.display());
    true
}

/// Recursively cleans up empty parent directories in the private cask store.
/// Starts from the given path and walks up, removing empty directories until a non-empty or root is
/// found.
pub fn cleanup_empty_parent_dirs_in_private_store(start_path: &Path, stop_at: &Path) {
    let mut current = start_path.to_path_buf();
    while current != *stop_at {
        if let Ok(read_dir) = fs::read_dir(&current) {
            if read_dir.count() == 0 {
                match fs::remove_dir(&current) {
                    Ok(_) => {
                        debug!("Removed empty directory: {}", current.display());
                    }
                    Err(e) => {
                        debug!("Failed to remove directory {}: {}", current.display(), e);
                        break;
                    }
                }
            } else {
                break;
            }
        } else {
            break;
        }
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            break;
        }
    }
}
