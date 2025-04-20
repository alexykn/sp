// ===== sapphire-core/src/build/cask/mod.rs =====

pub mod artifacts;
pub mod dmg;

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
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstalledArtifact {
    App { path: PathBuf },
    CaskroomLink { link_path: PathBuf, target_path: PathBuf },
    BinaryLink { link_path: PathBuf, target_path: PathBuf },
    PkgUtilReceipt { id: String },
    Launchd { label: String, path: Option<PathBuf> }, // Path might be system-wide
    CaskroomReference { path: PathBuf }, // e.g., the copied PKG
    // Add others: Prefpane, Kext, Service, Font, etc. as needed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaskInstallManifest {
    pub manifest_format_version: String, // e.g., "1.0"
    pub token: String,
    pub version: String,
    pub installed_at: u64, // Unix timestamp
    pub artifacts: Vec<InstalledArtifact>,
}

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
    println!("Downloading cask from {}", url_str);

    let parsed = Url::parse(url_str)
        .map_err(|e| SapphireError::Generic(format!("Invalid URL '{}': {}", url_str, e)))?;

    let file_name = parsed
        .path_segments()
        .and_then(|segments| segments.last())
        .unwrap_or("download.tmp");

    let cache_key = format!("cask-{}-{}", cask.token, file_name);
    let cache_path = cache.get_dir().join(&cache_key);

    if cache_path.exists() {
        println!("Using cached download at {}", cache_path.display());
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

    println!("Download completed: {}", cache_path.display());
    Ok(cache_path)
}

/// Install a cask from a downloaded file using artifact-driven logic
pub fn install_cask(
    cask: &Cask,
    download_path: &Path,
    config: &Config,
) -> Result<()> {
    info!("Installing cask: {}", cask.token);

    let cask_version_install_path = get_cask_version_path(cask, config);
    if !cask_version_install_path.exists() {
        fs::create_dir_all(&cask_version_install_path).map_err(|e| SapphireError::Io(
           std::io::Error::new(e.kind(), format!("Failed create cask dir {}: {}", cask_version_install_path.display(), e))
        ))?;
        debug!("Created cask version directory: {}", cask_version_install_path.display());
    }

    let extension = download_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // --- Aggregate installed artifacts ---
    let mut all_installed_artifacts: Vec<InstalledArtifact> = Vec::new(); // Renamed

    // --- Handle PKG directly ---
    if extension == "pkg" || extension == "mpkg" {
        debug!("Detected PKG, installing directly...");
        match artifacts::pkg::install_pkg_from_path(cask, download_path, &cask_version_install_path, config) {
             Ok(artifacts) => {
                 all_installed_artifacts.extend(artifacts);
                 // PKG install succeeded, now write manifest and return
                 debug!("Writing PKG install manifest...");
                 write_cask_manifest(cask, &cask_version_install_path, all_installed_artifacts)?;
                 debug!("✅ Successfully installed PKG cask: {}", cask.token);
                 return Ok(());
             }
             Err(e) => {
                error!("PKG installation failed: {}", e);
                // Attempt to clean up caskroom version directory on PKG failure
                let _ = fs::remove_dir_all(&cask_version_install_path);
                return Err(e);
             }
        }
    }

    // --- Staging Area Setup (for DMG/ZIP/etc.) ---
    let stage_dir = TempDir::new().map_err(|e| SapphireError::Io(std::io::Error::new(
        e.kind(),
        format!("Failed to create staging directory: {}", e)
    )))?;
    let stage_path = stage_dir.path();
    debug!("Created staging directory: {}", stage_path.display());


    // --- Extract to Staging ---
    match extension.as_str() {
        "dmg" => {
            debug!("Extracting DMG {} to stage {}...", download_path.display(), stage_path.display());
            match dmg::extract_dmg_to_stage(download_path, stage_path) {
                 Ok(_) => debug!("Successfully extracted DMG to staging area."),
                 Err(e) => {
                     error!("Failed to extract DMG to staging area: {}", e);
                     return Err(e); // Stop installation if extraction fails
                 }
             }
        }
        "zip" => {
            debug!("Extracting ZIP {} to stage {}...", download_path.display(), stage_path.display());
            match extract::extract_archive(download_path, stage_path, 0) {
                Ok(_) => debug!("Successfully extracted ZIP to staging area."),
                Err(e) => {
                    error!("Failed to extract ZIP to staging area: {}", e);
                    return Err(e);
                }
            }
        }
         // Add cases for .tar.gz, .app (if distributed directly) etc. if needed
        _ => {
            return Err(SapphireError::Generic(format!(
                "Unsupported file type for staged installation: {}",
                extension
            )));
        }
    }

    // --- Process Staged Artifacts ---
    let mut artifact_install_errors = Vec::new();
    if let Some(artifacts_def) = &cask.artifacts { // Renamed 'artifacts' -> 'artifacts_def'
        debug!("Processing {} declared artifacts from staging area...", artifacts_def.len());
        for artifact_value in artifacts_def.iter() {
            if let Some(artifact_obj) = artifact_value.as_object() {
                for (key, value) in artifact_obj.iter() {
                    let result: Result<Vec<InstalledArtifact>> = match key.as_str() {

                        "preflight" => {
                            artifacts::preflight::run_preflight(cask, &stage_path, config)
                        }

                        "uninstall" => {
                            artifacts::uninstall::record_uninstall(cask)
                        }


                        "app" => {
                            // your existing app logic
                            artifacts::app::install_app_from_staged(
                                cask,
                                &stage_path.join(value.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).next().unwrap()),
                                &cask_version_install_path,
                                config,
                            )
                        }
                    
                        "suite" => {
                            artifacts::suite::install_suite(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "installer" => {
                            artifacts::installer::run_installer(cask, &stage_path, &cask_version_install_path, config)
                        }

                        "zap" => {
                            // here we call our new zap handler
                            // it only needs the Cask and Config, since it cleans things up on uninstall
                            artifacts::zap::install_zap(cask, config)
                        }
                    
                        "pkg" => {
                            // your existing pkg logic
                            let mut all = Vec::new();
                            for pkg_name in value.as_array().unwrap().iter().filter_map(|v| v.as_str()) {
                                let path = stage_path.join(pkg_name);
                                let mut inst = artifacts::pkg::install_pkg_from_path(
                                    cask, &path, &cask_version_install_path, config,
                                )?;
                                all.append(&mut inst);
                            }
                            Ok(all)
                        }
                    
                        "binary" => artifacts::binary::install_binary(cask, stage_path, &cask_version_install_path, config),
                    
                        "manpage" => {
                            artifacts::manpage::install_manpage(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "colorpicker" => {
                            artifacts::colorpicker::install_colorpicker(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "dictionary" => {
                            artifacts::dictionary::install_dictionary(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "font" => {
                            artifacts::font::install_font(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "input_method" => {
                            artifacts::input_method::install_input_method(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "internet_plugin" => {
                            artifacts::internet_plugin::install_internet_plugin(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "keyboard_layout" => {
                            artifacts::keyboard_layout::install_keyboard_layout(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "prefpane" => {
                            artifacts::prefpane::install_prefpane(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "qlplugin" => {
                            artifacts::qlplugin::install_qlplugin(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "mdimporter" => {
                            artifacts::mdimporter::install_mdimporter(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "screen_saver" => {
                            artifacts::screen_saver::install_screen_saver(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "service" => {
                            artifacts::service::install_service(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "audio_unit_plugin" => {
                            artifacts::audio_unit_plugin::install_audio_unit_plugin(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "vst_plugin" => {
                            artifacts::vst_plugin::install_vst_plugin(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        "vst3_plugin" => {
                            artifacts::vst3_plugin::install_vst3_plugin(cask, &stage_path, &cask_version_install_path, config)
                        }
                    
                        other => {
                            warn!("Artifact type '{}' not supported yet — skipping.", other);
                            Ok(vec![])
                        }
                    };
                    // Handle result of artifact installation
                    match result {
                        Ok(installed) => all_installed_artifacts.extend(installed),
                        Err(e) => artifact_install_errors.push(e),
                    }
                    break; // Assume one key per artifact object
                }
            } else { warn!("Unexpected non-object artifact found in list: {:?}", artifact_value); }
        }
    } else {
        error!("Cask {} definition has no 'artifacts' array. Cannot determine what to install.", cask.token);
        return Err(SapphireError::InstallError(format!("Cask '{}' has no artifacts defined.", cask.token)));
    }

    // --- Finalize and Write Manifest ---
    if !artifact_install_errors.is_empty() {
         error!("Encountered errors installing artifacts for cask '{}'. Installation incomplete.", cask.token);
         // Attempt to clean up caskroom version directory on failure
         let _ = fs::remove_dir_all(&cask_version_install_path);
         // Return the first error encountered
         return Err(artifact_install_errors.remove(0));
    }

    if all_installed_artifacts.is_empty() {
        error!("No declared artifacts were found or installed for cask '{}' from the staged content.", cask.token);
        // Attempt cleanup
         let _ = fs::remove_dir_all(&cask_version_install_path);
        return Err(SapphireError::InstallError(format!("Installation failed for cask '{}': No artifacts installed.", cask.token)));
    } else {
        debug!("Writing cask installation manifest...");
        // Use the new function with the detailed artifact list
        write_cask_manifest(cask, &cask_version_install_path, all_installed_artifacts)?;
    }

    // Staging directory cleanup happens automatically when `stage_dir` goes out of scope

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

/// Writes the installation manifest for a cask.
pub fn write_cask_manifest(
    cask: &Cask,
    cask_version_install_path: &Path,
    artifacts: Vec<InstalledArtifact>, // Use the detailed enum
) -> Result<()> {
    // Use a more specific manifest filename
    let manifest_path = cask_version_install_path.join("CASK_INSTALL_MANIFEST.json");
    debug!("Writing cask manifest: {}", manifest_path.display());

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e: SystemTimeError| {
            SapphireError::Generic(format!("System time error: {}", e))
        })?
        .as_secs();

    let manifest_data = CaskInstallManifest {
        manifest_format_version: "1.0".to_string(), // Add versioning
        token: cask.token.clone(),
        version: cask.version.clone().unwrap_or_else(|| "latest".to_string()),
        installed_at: timestamp,
        artifacts, // Store the detailed artifact list
    };

    // Ensure parent directory exists
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent).map_err(|e| SapphireError::Io(
           std::io::Error::new(e.kind(), format!("Failed create parent dir {}: {}", parent.display(), e))
        ))?;
    }

    // Write the JSON data
    let file = fs::File::create(&manifest_path).map_err(|e| SapphireError::Io(
        std::io::Error::new(e.kind(), format!("Failed create manifest {}: {}", manifest_path.display(), e))
    ))?;
    // Use a buffered writer for potentially large manifests
    let writer = std::io::BufWriter::new(file);

    serde_json::to_writer_pretty(writer, &manifest_data).map_err(|e| {
        error!("Failed to serialize cask manifest JSON for {}: {}", cask.token, e);
        SapphireError::Json(e) // Convert serde error
    })?;

    debug!("Successfully wrote cask manifest with {} artifact entries.", manifest_data.artifacts.len());
    Ok(())
}
