use std::fs;
use std::path::Path;

use sps_common::config::Config;
use tracing::debug;

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
