// ===== sapphire-core/src/build/cask/mod.rs =====

pub mod app;
pub mod dmg;
pub mod pkg;

use crate::model::cask::{Cask, UrlField}; // Added Artifact enum
use crate::utils::cache::Cache;
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use crate::build::extract; // To use extract_archive for ZIP
use reqwest::Url;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};
use tempfile::TempDir; // For staging directory
use log::{debug, info, warn, error}; // For logging

/// Get installation path for a cask's specific version
pub fn get_cask_version_path(cask: &Cask, config: &Config) -> PathBuf {
    let version = cask
        .version
        .clone()
        .unwrap_or_else(|| "latest".to_string());
    config.cask_version_path(&cask.token, &version)
}

/// Download a cask
pub async fn download_cask(cask: &Cask, cache: &Cache) -> Result<PathBuf> {
    // Extract the URL(s) from our UrlField enum
    let url_field = cask.url.as_ref().ok_or_else(|| {
        SapphireError::Generic(format!("Cask {} has no URL", cask.token))
    })?;

    // Convert to a Vec<&String> for compatibility with existing logic
    let urls: Vec<&String> = match url_field {
        UrlField::Simple(u) => vec![u],
        UrlField::WithSpec { url, .. } => vec![url],
    };

    if urls.is_empty() {
        return Err(SapphireError::Generic(format!(
            "Cask {} has empty URL list",
            cask.token
        )));
    }

    let url_str = urls[0].as_str();
    println!("==> Downloading cask from {}", url_str);

    let parsed = Url::parse(url_str)
        .map_err(|e| SapphireError::Generic(format!("Invalid URL '{}': {}", url_str, e)))?;

    let file_name = parsed
        .path_segments()
        .and_then(|segments| segments.last())
        .unwrap_or("download.tmp");

    let cache_key = format!("cask-{}-{}", cask.token, file_name);
    let cache_path = cache.get_dir().join(&cache_key);

    if cache_path.exists() {
        println!("==> Using cached download at {}", cache_path.display());
        return Ok(cache_path);
    }

    let client = reqwest::Client::new();
    let response = client
        .get(parsed.clone())
        .send()
        .await
        .map_err(|e| SapphireError::Generic(format!("Failed to download cask: {}", e)))?;

    if !response.status().is_success() {
        return Err(SapphireError::Generic(format!(
            "Failed to download cask: HTTP status {}",
            response.status()
        )));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| SapphireError::Generic(format!("Failed to read response: {}", e)))?;

    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = fs::File::create(&cache_path)?;
    file.write_all(&bytes)?;

    println!("==> Download completed: {}", cache_path.display());
    Ok(cache_path)
}

/// Install a cask from a downloaded file using artifact-driven logic
pub fn install_cask(
    cask: &Cask,
    download_path: &Path,
    config: &Config,
) -> Result<()> {
    info!("==> Installing cask: {}", cask.token);

    let cask_version_install_path = get_cask_version_path(cask, config);
    if !cask_version_install_path.exists() {
        fs::create_dir_all(&cask_version_install_path).map_err(|e| SapphireError::Io(e))?;
        debug!("Created cask version directory: {}", cask_version_install_path.display());
    }

    let extension = download_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // --- Handle PKG directly (no staging needed) ---
    if extension == "pkg" || extension == "mpkg" {
        info!("Detected PKG, installing directly...");
        // The PKG installer should handle writing its own receipt details.
        // It might also check cask.artifacts for pkgutil IDs to record.
        // Assuming pkg::install_pkg_from_path exists and is correctly implemented
        return pkg::install_pkg_from_path(cask, download_path, &cask_version_install_path, config);
    }

    // --- Staging Area Setup (for DMG/ZIP) ---
    let stage_dir = TempDir::new().map_err(|e| SapphireError::Io(std::io::Error::new(
        e.kind(),
        format!("Failed to create staging directory: {}", e)
    )))?;
    let stage_path = stage_dir.path();
    debug!("Created staging directory: {}", stage_path.display());

    let mut installed_artifact_paths: Vec<String> = Vec::new(); // Track what gets installed for the receipt

    // --- Extract to Staging ---
    match extension.as_str() {
        "dmg" => {
            debug!("Extracting DMG {} to stage {}...", download_path.display(), stage_path.display());
            // TODO: Implement dmg::extract_dmg_to_stage function in `dmg.rs`.
            // It should mount the DMG, copy contents (e.g., using `ditto`) to stage_path, and unmount.
            match dmg::extract_dmg_to_stage(download_path, stage_path) {
                 Ok(_) => info!("Successfully extracted DMG to staging area."),
                 Err(e) => {
                     error!("Failed to extract DMG to staging area: {}", e);
                     // Cleanup temp dir before returning error
                     drop(stage_dir);
                     return Err(e); // Stop installation if extraction fails
                 }
             }
        }
        "zip" => {
            debug!("Extracting ZIP {} to stage {}...", download_path.display(), stage_path.display());
            // Use the existing general-purpose extractor
            // Assuming extract::extract_archive exists and is correctly implemented
            match extract::extract_archive(download_path, stage_path, 0) { // strip_components = 0
                Ok(_) => info!("Successfully extracted ZIP to staging area."),
                Err(e) => {
                    error!("Failed to extract ZIP to staging area: {}", e);
                    drop(stage_dir);
                    return Err(e);
                }
            }
        }
        _ => {
             // Cleanup temp dir before returning error
             drop(stage_dir);
            return Err(SapphireError::Generic(format!(
                "Unsupported file type for staged installation: {}",
                extension
            )));
        }
    }

    // --- Process Staged Artifacts based on Cask Definition ---
    if let Some(artifacts) = &cask.artifacts { // 'artifacts' is Option<Vec<JsonValue>>
        info!("Processing {} declared artifacts from staging area...", artifacts.len());
        // Iterate over the vector of JsonValue objects
        for artifact_value in artifacts.iter() { // artifact_value is &JsonValue
            // Check if the artifact is a JSON object
            if let Some(artifact_obj) = artifact_value.as_object() {
                // Iterate over the single key-value pair expected per artifact object
                // (e.g., {"app": ["Foo.app"]}, {"pkg": ["installer.pkg"]})
                for (key, value) in artifact_obj.iter() {
                    match key.as_str() {
                        "app" => {
                            if let Some(app_array) = value.as_array() {
                                for app_name_val in app_array {
                                    if let Some(app_name) = app_name_val.as_str() {
                                        let staged_app_path = stage_path.join(app_name);
                                        if staged_app_path.exists() {
                                            info!("Installing app artifact: {}", app_name);
                                            // TODO: Ensure app::install_app_from_staged is implemented correctly in app.rs
                                            match app::install_app_from_staged(&cask, &staged_app_path, &cask_version_install_path, config) {
                                                Ok(installed_paths) => {
                                                    debug!("Installed app artifact '{}': paths {:?}", app_name, installed_paths);
                                                    installed_artifact_paths.extend(installed_paths);
                                                }
                                                Err(e) => {
                                                    error!("Failed to install app artifact '{}': {}", app_name, e);
                                                    // Continue with other artifacts despite error? Yes for now.
                                                }
                                            }
                                        } else {
                                            warn!("Declared app artifact '{}' not found in staged directory {}", app_name, stage_path.display());
                                        }
                                    } else {
                                         warn!("Non-string value found in 'app' artifact array: {:?}", app_name_val);
                                     }
                                }
                            } else {
                                 warn!("'app' artifact value is not an array: {:?}", value);
                             }
                        }
                        "pkg" => {
                             if let Some(pkg_array) = value.as_array() {
                                 for pkg_name_val in pkg_array {
                                     if let Some(pkg_name) = pkg_name_val.as_str() {
                                         let staged_pkg_path = stage_path.join(pkg_name);
                                         if staged_pkg_path.exists() {
                                             info!("Installing pkg artifact from stage: {}", pkg_name);
                                             // Assuming pkg::install_pkg_from_path is implemented correctly
                                             match pkg::install_pkg_from_path(cask, &staged_pkg_path, &cask_version_install_path, config) {
                                                Ok(()) => {
                                                     // Record the *staged* path as an indicator? Or rely on pkg installer receipt?
                                                     installed_artifact_paths.push(staged_pkg_path.to_string_lossy().to_string());
                                                     debug!("Installed pkg artifact '{}' from stage.", pkg_name);
                                                },
                                                Err(e) => error!("Failed to install pkg artifact '{}' from stage: {}", pkg_name, e),
                                             }
                                         } else {
                                              warn!("Declared pkg artifact '{}' not found in staged directory {}", pkg_name, stage_path.display());
                                         }
                                     } else {
                                         warn!("Non-string value found in 'pkg' artifact array: {:?}", pkg_name_val);
                                     }
                                 }
                             } else {
                                 warn!("'pkg' artifact value is not an array: {:?}", value);
                             }
                        }
                        "binary" => {
                            info!("Processing binary artifacts (linking)...");
                            // TODO: Implement binary handling by parsing the 'value' (JsonValue)
                            warn!("Binary artifact linking not yet implemented.");
                            // Example placeholder logic:
                            // if let Some(specs) = value.as_array() {
                            //    match binary::install_binaries_from_staged(cask, specs, stage_path, &cask_version_install_path, config) {
                            //       Ok(paths) => installed_artifact_paths.extend(paths),
                            //       Err(e) => error!("Failed to link binary artifacts: {}", e),
                            //    }
                            // }
                        }
                        // Skip other keys like "uninstall", "zap", etc. during install phase
                        _ => {
                            debug!("Skipping artifact type '{}' during install phase.", key);
                        }
                    }
                    // Assume only one key per artifact object
                    break;
                }
            } else {
                warn!("Unexpected non-object artifact found in list: {:?}", artifact_value);
            }
        }
    } else {
        error!("Cask {} definition has no 'artifacts' array. Cannot determine what to install.", cask.token);
        drop(stage_dir); // Ensure cleanup
        return Err(SapphireError::InstallError(format!("Cask '{}' has no artifacts defined.", cask.token)));
    }

    // --- Finalize and Write Receipt ---
    if installed_artifact_paths.is_empty() {
         error!("No declared artifacts were found or installed for cask '{}' from the staged content.", cask.token);
         drop(stage_dir); // Ensure cleanup
         return Err(SapphireError::InstallError(format!("Installation failed for cask '{}': No artifacts installed.", cask.token)));
    } else {
         info!("Writing receipt for installed artifacts...");
         // Ensure write_receipt exists and has the correct signature.
         write_receipt(cask, &cask_version_install_path, installed_artifact_paths)?;
    }

    // Staging directory automatically cleaned up when `stage_dir` goes out of scope here (RAII)
    debug!("Staging directory {} will be cleaned up.", stage_path.display());

    info!("✅ Successfully installed cask: {}", cask.token);
    Ok(())
}


// --- Ensure write_receipt exists and is correct ---
/// Write a receipt file for the cask installation
pub fn write_receipt(
    cask: &Cask,
    cask_version_install_path: &Path,
    artifacts: Vec<String>, // List of installed paths/identifiers (like /Applications/Foo.app, pkgutil:com.foo.pkg)
) -> Result<()> {
    let receipt_path = cask_version_install_path.join("INSTALL_RECEIPT.json");
    debug!("Writing cask receipt: {}", receipt_path.display());

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e: SystemTimeError| {
            SapphireError::Generic(format!("System time error: {}", e))
        })?
        .as_secs();

    let receipt_data = json!({
        "token": cask.token,
        "name": cask.name.as_ref().and_then(|n| n.first()).cloned(), // Add primary name if available
        "version": cask.version.as_ref().unwrap_or(&"latest".to_string()),
        "installed_at": timestamp,
        "artifacts_installed": artifacts // Use a more descriptive key
    });

    // Ensure parent directory exists
     if let Some(parent) = receipt_path.parent() {
         fs::create_dir_all(parent).map_err(SapphireError::Io)?;
     }

    let mut file = fs::File::create(&receipt_path).map_err(SapphireError::Io)?;
    serde_json::to_writer_pretty(&mut file, &receipt_data).map_err(SapphireError::Json)?;
    debug!("Successfully wrote receipt with {} artifact entries.", artifacts.len());
    Ok(())
}
