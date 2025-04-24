use std::fs;
use std::path::Path;
use std::process::Command;

use tracing::{debug, error};

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask; // Artifact type alias is just Value
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};

/// Installs a PKG file and returns details of artifacts created/managed.
pub fn install_pkg_from_path(
    cask: &Cask,
    pkg_path: &Path,
    cask_version_install_path: &Path, // e.g., /opt/homebrew/Caskroom/foo/1.2.3
    _config: &Config,                 // Keep for potential future use
) -> Result<Vec<InstalledArtifact>> {
    // <-- Return type changed
    debug!("Installing pkg file: {}", pkg_path.display());

    if !pkg_path.exists() || !pkg_path.is_file() {
        return Err(SapphireError::NotFound(format!(
            "Package file not found or is not a file: {}",
            pkg_path.display()
        )));
    }

    let pkg_name = pkg_path.file_name().ok_or_else(|| {
        SapphireError::Generic(format!("Invalid pkg path: {}", pkg_path.display()))
    })?;

    // --- Prepare list for artifacts ---
    let mut installed_artifacts: Vec<InstalledArtifact> = Vec::new();

    // --- Copy PKG to Caskroom for Reference ---
    let caskroom_pkg_path = cask_version_install_path.join(pkg_name);
    debug!(
        "Copying pkg to caskroom for reference: {}",
        caskroom_pkg_path.display()
    );
    if let Some(parent) = caskroom_pkg_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed create parent dir {}: {}", parent.display(), e),
            ))
        })?;
    }
    if let Err(e) = fs::copy(pkg_path, &caskroom_pkg_path) {
        error!(
            "Failed to copy PKG {} to {}: {}",
            pkg_path.display(),
            caskroom_pkg_path.display(),
            e
        );
        return Err(SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed copy PKG to caskroom: {e}"),
        )));
    } else {
        // Record the reference copy artifact
        installed_artifacts.push(InstalledArtifact::CaskroomReference {
            path: caskroom_pkg_path.clone(),
        });
    }

    // --- Run Installer ---
    debug!("Running installer (this may require sudo)");
    debug!(
        "Executing: sudo installer -pkg {} -target /",
        pkg_path.display()
    );
    let output = Command::new("sudo")
        .arg("installer")
        .arg("-pkg")
        .arg(pkg_path)
        .arg("-target")
        .arg("/")
        .output()
        .map_err(|e| {
            SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to execute sudo installer: {e}"),
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("sudo installer failed ({}): {}", output.status, stderr);
        // Don't clean up the reference copy here, let the main process handle directory removal on
        // failure
        return Err(SapphireError::InstallError(format!(
            "Package installation failed for {}: {}",
            pkg_path.display(),
            stderr
        )));
    }
    debug!("Successfully ran installer command.");
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        debug!("Installer stdout:\n{}", stdout);
    }

    // --- Record PkgUtil Receipts (based on cask definition) ---
    if let Some(artifacts) = &cask.artifacts {
        // artifacts is Option<Vec<JsonValue>>
        for artifact_value in artifacts.iter() {
            if let Some(uninstall_array) =
                artifact_value.get("uninstall").and_then(|v| v.as_array())
            {
                for stanza_value in uninstall_array {
                    if let Some(stanza_obj) = stanza_value.as_object() {
                        if let Some(pkgutil_id) = stanza_obj.get("pkgutil").and_then(|v| v.as_str())
                        {
                            debug!("Found pkgutil ID to record: {}", pkgutil_id);
                            // Check for duplicates before adding
                            let new_artifact = InstalledArtifact::PkgUtilReceipt {
                                id: pkgutil_id.to_string(),
                            };
                            if !installed_artifacts.contains(&new_artifact) {
                                // Need PartialEq for InstalledArtifact
                                installed_artifacts.push(new_artifact);
                            }
                        }
                        // Consider other uninstall keys like launchctl, delete?
                    }
                }
            }
            // Optionally check "zap" stanzas too
        }
    }
    debug!("Successfully installed pkg: {}", pkg_path.display());
    Ok(installed_artifacts) // <-- Return collected artifacts
}
