use crate::model::cask::Cask; // Artifact type alias is just Value
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use std::fs;
use std::path::Path;
use std::process::Command;
use log::{debug, error, info};

// --- Function install_pkg_from_path ---
/// Install a pkg directly from a file path
pub fn install_pkg_from_path(
    cask: &Cask,
    pkg_path: &Path,
    cask_version_install_path: &Path, // e.g., /opt/homebrew/Caskroom/foo/1.2.3
    _config: &Config, // May be needed later for more complex pkg logic
) -> Result<()> {
    info!("==> Installing pkg file: {}", pkg_path.display());

    // --- Validate PKG Path ---
    if !pkg_path.exists() || !pkg_path.is_file() {
         return Err(SapphireError::NotFound(format!(
             "Package file not found or is not a file: {}", pkg_path.display()
         )));
     }

    let pkg_name = pkg_path
        .file_name()
        .ok_or_else(|| SapphireError::Generic(format!("Invalid pkg path: {}", pkg_path.display())))?;

    // --- Copy PKG to Caskroom for Reference ---
    let caskroom_pkg_path = cask_version_install_path.join(pkg_name);
    info!("==> Copying pkg to caskroom for reference: {}", caskroom_pkg_path.display());
    // Ensure target directory exists
    if let Some(parent) = caskroom_pkg_path.parent() {
         fs::create_dir_all(parent).map_err(|e| SapphireError::Io(
             std::io::Error::new(e.kind(), format!("Failed create parent dir {}: {}", parent.display(), e))
         ))?;
     }
    if let Err(e) = fs::copy(pkg_path, &caskroom_pkg_path) {
         error!("Failed to copy PKG {} to {}: {}", pkg_path.display(), caskroom_pkg_path.display(), e);
         return Err(SapphireError::Io(
             std::io::Error::new(e.kind(), format!("Failed copy PKG to caskroom: {}", e))
         ));
     }

    // --- Run Installer ---
    info!("==> Running installer (this may require sudo)");
    debug!("Executing: sudo installer -pkg {} -target /", pkg_path.display());
    let output = Command::new("sudo")
        .arg("installer")
        .arg("-pkg")
        .arg(pkg_path) // Use the original downloaded/staged path for installation
        .arg("-target") // Standard target is root '/'
        .arg("/")
        .output() // Use output() to capture stderr
        .map_err(|e| SapphireError::Io( // Handle command execution error
            std::io::Error::new(e.kind(), format!("Failed to execute sudo installer: {}", e))
        ))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("sudo installer failed ({}): {}", output.status, stderr);
        // Attempt to clean up copied pkg? Maybe not necessary.
        return Err(SapphireError::InstallError(format!(
            "Package installation failed for {}: {}", pkg_path.display(), stderr
        )));
    }
    info!("Successfully ran installer command.");
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() { // Log stdout from installer if any
        debug!("Installer stdout:\n{}", stdout);
    }


    // --- Receipt Writing ---
    let mut artifacts_to_record = vec![
        caskroom_pkg_path.to_string_lossy().to_string(), // Record the path to the *copy* in the caskroom
    ];

    // Check cask definition for uninstall pkgutil stanzas
    if let Some(artifacts) = &cask.artifacts { // artifacts is Option<Vec<JsonValue>>
        for artifact_value in artifacts.iter() { // artifact_value is &JsonValue
            // Check if it's an object and has the "uninstall" key
            if let Some(uninstall_array) = artifact_value.get("uninstall").and_then(|v| v.as_array()) {
                 // Found an uninstall stanza, now iterate through its contents
                 for stanza_value in uninstall_array {
                     // Check if the stanza is an object containing "pkgutil"
                     if let Some(stanza_obj) = stanza_value.as_object() {
                         if let Some(pkgutil_id) = stanza_obj.get("pkgutil").and_then(|v| v.as_str()) {
                            let pkg_record = format!("pkgutil:{}", pkgutil_id);
                            info!("Found pkgutil ID for receipt: {}", pkg_record);
                            // Avoid duplicates if multiple uninstall stanzas list the same ID
                            if !artifacts_to_record.contains(&pkg_record) {
                                 artifacts_to_record.push(pkg_record);
                            }
                         }
                         // Check for other uninstall keys like "launchctl", "delete", etc. here if needed
                     }
                 }
            }
            // We might also find pkgutil info in "zap" stanzas, consider checking artifact_value.get("zap") too if necessary.
        }
     }

    // Write the receipt using the function from the parent module (cask::mod.rs)
    // Ensure write_receipt is accessible (e.g., using `super::write_receipt`)
    match super::write_receipt(cask, cask_version_install_path, artifacts_to_record) {
         Ok(_) => debug!("Successfully wrote PKG install receipt."),
         Err(e) => {
             // Don't fail the whole install for a receipt error, but log it
             error!("Failed to write PKG install receipt for {}: {}", cask.token, e);
         }
     }


    info!("==> Successfully installed pkg: {}", pkg_path.display());
    Ok(())
}
