// sps-core/src/uninstall/common.rs

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::{fs, io};

use sps_common::config::Config;
use tracing::{debug, error, warn};

#[derive(Debug, Clone, Default)]
pub struct UninstallOptions {
    pub skip_zap: bool,
}

/// Removes a filesystem artifact (file or directory).
///
/// Attempts direct removal. If `use_sudo` is true and direct removal
/// fails due to permission errors, it will attempt `sudo rm -rf`.
///
/// Returns `true` if the artifact is successfully removed or was already gone,
/// `false` otherwise.
pub(crate) fn remove_filesystem_artifact(path: &Path, use_sudo: bool) -> bool {
    match path.symlink_metadata() {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            // A directory is only a "real" directory if it's not a symlink.
            // Symlinks to directories should be removed with remove_file.
            let is_real_dir = file_type.is_dir();

            debug!(
                "Removing filesystem artifact ({}) at: {}",
                if is_real_dir {
                    "directory"
                } else if file_type.is_symlink() {
                    "symlink"
                } else {
                    "file"
                },
                path.display()
            );

            let remove_op = || -> io::Result<()> {
                if is_real_dir {
                    fs::remove_dir_all(path)
                } else {
                    // This handles both files and symlinks
                    fs::remove_file(path)
                }
            };

            if let Err(e) = remove_op() {
                if use_sudo && e.kind() == io::ErrorKind::PermissionDenied {
                    warn!(
                        "Direct removal failed (Permission Denied). Trying with sudo rm -rf: {}",
                        path.display()
                    );
                    let output = Command::new("sudo").arg("rm").arg("-rf").arg(path).output();
                    match output {
                        Ok(out) if out.status.success() => {
                            debug!("Successfully removed {} with sudo.", path.display());
                            true
                        }
                        Ok(out) => {
                            error!(
                                "Failed to remove {} with sudo: {}",
                                path.display(),
                                String::from_utf8_lossy(&out.stderr).trim()
                            );
                            false
                        }
                        Err(sudo_err) => {
                            error!(
                                "Error executing sudo rm for {}: {}",
                                path.display(),
                                sudo_err
                            );
                            false
                        }
                    }
                } else if e.kind() != io::ErrorKind::NotFound {
                    error!("Failed to remove artifact {}: {}", path.display(), e);
                    false
                } else {
                    debug!("Artifact {} already removed.", path.display());
                    true
                }
            } else {
                debug!("Successfully removed artifact: {}", path.display());
                true
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            debug!("Artifact not found (already removed?): {}", path.display());
            true
        }
        Err(e) => {
            warn!(
                "Failed to get metadata for artifact {}: {}",
                path.display(),
                e
            );
            false
        }
    }
}

/// Expands a path string that may start with `~` to the user's home directory.
pub(crate) fn expand_tilde(path_str: &str, home: &Path) -> PathBuf {
    if let Some(stripped) = path_str.strip_prefix("~/") {
        home.join(stripped)
    } else {
        PathBuf::from(path_str)
    }
}

/// Checks if a path is safe for zap operations.
/// Safe paths are typically within user Library, .config, /Applications, /Library,
/// or the sps cache directory. Root, home, /Applications, /Library themselves are not safe.
pub(crate) fn is_safe_path(path: &Path, home: &Path, config: &Config) -> bool {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        warn!("Zap path rejected (contains '..'): {}", path.display());
        return false;
    }
    let allowed_roots = [
        home.join("Library"),
        home.join(".config"),
        PathBuf::from("/Applications"),
        PathBuf::from("/Library"),
        config.cache_dir().clone(),
        // Consider adding more specific allowed user dirs if necessary
    ];

    // Check if the path is exactly one of the top-level restricted paths
    if path == Path::new("/")
        || path == home
        || path == Path::new("/Applications")
        || path == Path::new("/Library")
    {
        warn!("Zap path rejected (too broad): {}", path.display());
        return false;
    }

    if allowed_roots.iter().any(|root| path.starts_with(root)) {
        return true;
    }

    warn!(
        "Zap path rejected (outside allowed areas): {}",
        path.display()
    );
    false
}
