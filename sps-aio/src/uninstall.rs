// sps-aio/src/uninstall.rs
// Provides async uninstall operations (filesystem, pkgutil, launchctl).
// Uses tokio::process for external commands.

use std::path::Path;
use std::process::Stdio; // Keep for Command setup
use std::sync::Arc;

use sps_common::{
    config::Config, // Keep Config dependency if needed by helpers
    error::{Result, SpsError},
};
use tokio::process::Command; // Use tokio's Command
use tracing::{debug, error, warn};

use crate::fs as sps_fs; // Use async fs helpers

/// Asynchronously removes a filesystem path (file, symlink, or directory recursively).
/// Uses sudo rm -rf if direct removal fails with PermissionDenied and use_sudo is true.
pub async fn remove_path_async(path: &Path, use_sudo: bool) -> Result<()> {
    match sps_fs::get_symlink_metadata_async(path).await {
        Ok(metadata) => {
            let is_dir = metadata.file_type().is_dir();
            let path_type = if is_dir {
                "directory"
            } else if metadata.file_type().is_symlink() {
                "symlink"
            } else {
                "file"
            };
            debug!("Async Removing {} at: {}", path_type, path.display());

            let remove_result = if is_dir {
                sps_fs::remove_directory_recursive_async(path).await
            } else {
                sps_fs::remove_file_async(path).await
            };

            match remove_result {
                Ok(()) => {
                    debug!(
                        "Async Successfully removed {}: {}",
                        path_type,
                        path.display()
                    );
                    Ok(())
                }
                // Check specifically for PermissionDenied ErrorKind
                Err(SpsError::Io(io_err_arc))
                    if use_sudo && io_err_arc.kind() == std::io::ErrorKind::PermissionDenied =>
                {
                    warn!(
                        "Async Direct removal failed (Permission Denied). Trying with sudo rm -rf: {}",
                        path.display()
                    );
                    // Async external command execution
                    let output = Command::new("sudo")
                        .arg("rm")
                        .arg("-rf")
                        .arg(path)
                        .output() // Use tokio's Command output()
                        .await;
                    match output {
                        Ok(out) if out.status.success() => {
                            debug!("Async Successfully removed {} with sudo.", path.display());
                            Ok(())
                        }
                        Ok(out) => {
                            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                            error!(
                                "Async Failed to remove {} with sudo: {}",
                                path.display(),
                                stderr
                            );
                            Err(SpsError::IoError(format!("sudo rm -rf failed: {stderr}")))
                        }
                        Err(sudo_err) => {
                            error!(
                                "Async Error executing sudo rm for {}: {}",
                                path.display(),
                                sudo_err
                            );
                            Err(SpsError::Io(Arc::new(sudo_err))) // Wrap the execution error
                        }
                    }
                }
                Err(e) => {
                    // Handles NotFound implicitly as Ok(()) within
                    // remove_file_async/remove_directory_recursive_async
                    error!("Async Failed to remove artifact {}: {}", path.display(), e);
                    Err(e) // Propagate other errors
                }
            }
        }
        Err(SpsError::Io(io_err_arc)) if io_err_arc.kind() == std::io::ErrorKind::NotFound => {
            debug!(
                "Async Path not found (already removed?): {}",
                path.display()
            );
            Ok(()) // Treat NotFound as success for uninstall
        }
        Err(e) => {
            warn!(
                "Async Failed to get metadata for artifact {}: {}",
                path.display(),
                e
            );
            Err(e) // Propagate other errors
        }
    }
}

/// Asynchronously forgets a package receipt using `pkgutil`. Requires sudo.
#[cfg(target_os = "macos")] // This function is macOS specific
pub async fn forget_pkgutil_async(id: &str) -> Result<()> {
    if id.contains('/') || id.contains("..") {
        let msg = format!("Invalid pkgutil receipt id contains disallowed characters: {id}");
        error!(msg);
        return Err(SpsError::ValidationError(msg));
    }
    debug!("Async Forgetting package receipt (requires sudo): {}", id);

    let output = Command::new("sudo")
        .arg("pkgutil")
        .arg("--forget")
        .arg(id)
        .stderr(Stdio::piped()) // Capture stderr
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            debug!("Async Successfully forgot package receipt {}", id);
            Ok(())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if stderr.contains("No receipt for") {
                debug!(
                    "Async Package receipt {} already forgotten or never existed.",
                    id
                );
                Ok(()) // Treat "not found" as success
            } else {
                error!("Async Failed to forget package receipt {}: {}", id, stderr);
                Err(SpsError::CommandExecError(format!(
                    "pkgutil --forget failed: {stderr}"
                )))
            }
        }
        Err(e) => {
            error!(
                "Async Failed to execute sudo pkgutil --forget {}: {}",
                id, e
            );
            Err(SpsError::Io(Arc::new(e)))
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub async fn forget_pkgutil_async(id: &str) -> Result<()> {
    warn!("forget_pkgutil called on non-macOS for id: {}", id);
    Err(SpsError::Generic(
        "pkgutil is only supported on macOS".to_string(),
    ))
}

/// Asynchronously unloads a launchd service/agent and optionally removes its plist file.
#[cfg(target_os = "macos")] // This function is macOS specific
pub async fn unload_launchd_async(
    label: &str,
    plist_path: Option<&Path>,
    _config: &Config,
) -> Result<()> {
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
    }

    debug!(
        "Async Unloading launchd agent/daemon (requires sudo): {}",
        label
    );
    let mut first_error: Option<SpsError> = None;

    let unload_output = Command::new("sudo")
        .arg("launchctl")
        .arg("unload")
        .arg("-w")
        .arg(label)
        .stderr(Stdio::piped())
        .output()
        .await;

    match unload_output {
        Ok(out) => {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if !stderr.contains("Could not find specified service")
                    && !stderr.contains("service not loaded")
                    && !stderr.contains("No such process")
                    && !stderr.contains("launchctl unload error")
                    && !stderr.is_empty()
                {
                    warn!("Async Failed to unload launchd item {}: {}", label, stderr);
                    if first_error.is_none() {
                        first_error = Some(SpsError::CommandExecError(format!(
                            "launchctl unload failed: {stderr}"
                        )));
                    }
                } else {
                    debug!(
                        "Async Launchd item {} already unloaded or not found.",
                        label
                    );
                }
            } else {
                debug!("Async Successfully unloaded launchd item {}.", label);
            }
        }
        Err(e) => {
            error!(
                "Async Failed to execute sudo launchctl unload for {}: {}",
                label, e
            );
            if first_error.is_none() {
                first_error = Some(SpsError::Io(Arc::new(e)));
            }
        }
    }

    if let Some(p) = plist_path {
        let use_sudo =
            p.starts_with("/Library/LaunchDaemons") || p.starts_with("/Library/LaunchAgents");
        debug!("Async Attempting removal of launchd plist: {}", p.display());
        if let Err(e) = remove_path_async(p, use_sudo).await {
            warn!(
                "Async Failed to remove launchd plist file {}: {}",
                p.display(),
                e
            );
            if first_error.is_none() {
                first_error = Some(e);
            }
        }
    }

    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(not(target_os = "macos"))]
pub async fn unload_launchd_async(
    label: &str,
    plist_path: Option<&Path>,
    _config: &Config,
) -> Result<()> {
    warn!(
        "unload_launchd called on non-macOS for label: {}, path: {:?}",
        label, plist_path
    );
    Err(SpsError::Generic(
        "launchd is only supported on macOS".to_string(),
    ))
}

/// Asynchronously moves a path to the trash. Requires external `trash` CLI or similar.
/// Uses spawn_blocking as the `trash` crate is sync.
#[cfg(target_os = "macos")] // Trash functionality might differ significantly
pub async fn trash_path_async(path: &Path) -> Result<()> {
    debug!(
        "Async Trashing path (using spawn_blocking): {}",
        path.display()
    );
    let path_owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        // Use the synchronous 'trash' crate inside spawn_blocking
        match trash::delete(&path_owned) {
            Ok(()) => {
                debug!("Successfully trashed {}", path_owned.display());
                Ok(())
            }
            Err(e) => {
                error!("Failed to trash {}: {}", path_owned.display(), e);
                // Convert trash::Error to SpsError
                Err(SpsError::Generic(format!("Trash operation failed: {e}")))
            }
        }
    })
    .await
    .map_err(|e| SpsError::Generic(format!("Trash task failed: {e}")))? // Handle JoinError
}

#[cfg(not(target_os = "macos"))]
pub async fn trash_path_async(path: &Path) -> Result<()> {
    warn!(
        "Trash functionality not implemented for this platform: {}",
        path.display()
    );
    Err(SpsError::Generic(
        "Trash not supported on this platform".to_string(),
    ))
}

// --- Sync Versions (Kept for reference) ---

pub fn remove_path_sync(path: &Path, use_sudo: bool) -> Result<()> {
    // Existing sync implementation...
    match sps_fs::get_symlink_metadata_sync(path) {
        Ok(metadata) => {
            let is_dir = metadata.file_type().is_dir();
            let path_type = if is_dir {
                "directory"
            } else if metadata.file_type().is_symlink() {
                "symlink"
            } else {
                "file"
            };
            debug!("Sync Removing {} at: {}", path_type, path.display());
            let remove_result = if is_dir {
                sps_fs::remove_directory_recursive_sync(path)
            } else {
                sps_fs::remove_file_sync(path)
            };
            match remove_result {
                Ok(()) => Ok(()),
                Err(SpsError::Io(io_err))
                    if use_sudo && io_err.kind() == std::io::ErrorKind::PermissionDenied =>
                {
                    warn!("Sync Direct removal failed (Permission Denied). Trying with sudo rm -rf: {}", path.display());
                    let output = std::process::Command::new("sudo")
                        .arg("rm")
                        .arg("-rf")
                        .arg(path)
                        .output();
                    match output {
                        Ok(out) if out.status.success() => Ok(()),
                        Ok(out) => {
                            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                            error!(
                                "Sync Failed to remove {} with sudo: {}",
                                path.display(),
                                stderr
                            );
                            Err(SpsError::IoError(format!("sudo rm -rf failed: {stderr}")))
                        }
                        Err(sudo_err) => {
                            error!(
                                "Sync Error executing sudo rm for {}: {}",
                                path.display(),
                                sudo_err
                            );
                            Err(SpsError::from(sudo_err))
                        }
                    }
                }
                Err(e) => {
                    error!("Sync Failed to remove artifact {}: {}", path.display(), e);
                    Err(e)
                }
            }
        }
        Err(SpsError::Io(io_err)) if io_err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            warn!(
                "Sync Failed to get metadata for artifact {}: {}",
                path.display(),
                e
            );
            Err(e)
        }
    }
}

#[cfg(target_os = "macos")]
pub fn forget_pkgutil_sync(id: &str) -> Result<()> {
    // Existing sync implementation...
    if id.contains('/') || id.contains("..") {
        return Err(SpsError::ValidationError("Invalid pkgutil id".into()));
    }
    debug!("Sync Forgetting package receipt (requires sudo): {}", id);
    let output = std::process::Command::new("sudo")
        .arg("pkgutil")
        .arg("--forget")
        .arg(id)
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if stderr.contains("No receipt for") {
                Ok(())
            } else {
                error!("Sync Failed to forget package receipt {}: {}", id, stderr);
                Err(SpsError::CommandExecError(format!(
                    "pkgutil --forget failed: {stderr}"
                )))
            }
        }
        Err(e) => {
            error!("Sync Failed to execute sudo pkgutil --forget {}: {}", id, e);
            Err(SpsError::from(e))
        }
    }
}
#[cfg(not(target_os = "macos"))]
pub fn forget_pkgutil_sync(id: &str) -> Result<()> {
    Err(SpsError::Generic("pkgutil not supported".into()))
}

#[cfg(target_os = "macos")]
pub fn unload_launchd_sync(label: &str, plist_path: Option<&Path>, _config: &Config) -> Result<()> {
    // Existing sync implementation...
    if label.contains('/') || label.contains("..") {
        return Err(SpsError::ValidationError("Invalid label".into()));
    }
    if let Some(p) = plist_path {
        if p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(SpsError::ValidationError("Invalid plist path".into()));
        }
    }
    debug!(
        "Sync Unloading launchd agent/daemon (requires sudo): {}",
        label
    );
    let mut first_error: Option<SpsError> = None;
    let unload_output = std::process::Command::new("sudo")
        .arg("launchctl")
        .arg("unload")
        .arg("-w")
        .arg(label)
        .stderr(Stdio::piped())
        .output();
    match unload_output {
        Ok(out) => {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if !stderr.contains("Could not find")
                    && !stderr.contains("not loaded")
                    && !stderr.contains("No such process")
                    && !stderr.contains("unload error")
                    && !stderr.is_empty()
                {
                    warn!("Sync Failed to unload launchd item {}: {}", label, stderr);
                    if first_error.is_none() {
                        first_error = Some(SpsError::CommandExecError(format!(
                            "launchctl unload failed: {stderr}"
                        )));
                    }
                }
            }
        }
        Err(e) => {
            error!(
                "Sync Failed to execute sudo launchctl unload for {}: {}",
                label, e
            );
            if first_error.is_none() {
                first_error = Some(SpsError::from(e));
            }
        }
    }
    if let Some(p) = plist_path {
        let use_sudo =
            p.starts_with("/Library/LaunchDaemons") || p.starts_with("/Library/LaunchAgents");
        debug!("Sync Attempting removal of launchd plist: {}", p.display());
        if let Err(e) = remove_path_sync(p, use_sudo) {
            warn!(
                "Sync Failed to remove launchd plist file {}: {}",
                p.display(),
                e
            );
            if first_error.is_none() {
                first_error = Some(e);
            }
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
#[cfg(not(target_os = "macos"))]
pub fn unload_launchd_sync(label: &str, plist_path: Option<&Path>, config: &Config) -> Result<()> {
    Err(SpsError::Generic("launchd not supported".into()))
}

#[cfg(target_os = "macos")]
pub fn trash_path_sync(path: &Path) -> Result<()> {
    debug!("Sync Trashing path: {}", path.display());
    trash::delete(path).map_err(|e| SpsError::Generic(format!("Trash failed: {e}")))
}
#[cfg(not(target_os = "macos"))]
pub fn trash_path_sync(path: &Path) -> Result<()> {
    Err(SpsError::Generic("Trash not supported".into()))
}
