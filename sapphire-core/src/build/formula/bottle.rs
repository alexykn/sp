// **File:** sapphire-core/src/build/formula/bottle.rs

use crate::utils::error::{SapphireError, Result};
use crate::model::formula::{Formula, BottleFileSpec};
use crate::utils::config::Config;
use crate::fetch::oci;
use std::path::{Path, PathBuf};
use std::fs;
use log::{debug, error, info, warn};
use reqwest::Client;
use std::fs::File;
use std::io::{copy, Read};
use sha2::{Sha256, Digest};
use hex;
use walkdir::WalkDir;
use std::os::unix::fs::PermissionsExt; // <-- Import PermissionsExt for chmod

/// Downloads and verifies a bottle for the given formula.
pub async fn download_bottle(formula: &Formula, config: &Config, client: &Client) -> Result<PathBuf> {
    info!("Attempting to download bottle for {}", formula.name);

    // 1. Determine the correct platform and bottle spec
    let (platform_tag, bottle_file_spec) = get_bottle_for_platform(formula)?;
    debug!("Selected bottle spec for platform '{}': URL={}, SHA256={}",
           platform_tag, bottle_file_spec.url, bottle_file_spec.sha256);

    if bottle_file_spec.url.is_empty() {
        return Err(SapphireError::DownloadError( formula.name.clone(), "".to_string(), "Bottle spec has an empty URL.".to_string()));
    }
    if bottle_file_spec.sha256.is_empty() {
        warn!("Bottle spec for {} ({}) is missing SHA256 checksum!", formula.name, platform_tag);
    }

    // 2. Determine the target cache path
    let filename = generate_bottle_filename(formula, &platform_tag);
    let cache_dir = config.cache_dir.join("bottles");
    fs::create_dir_all(&cache_dir).map_err(SapphireError::Io)?;
    let bottle_cache_path = cache_dir.join(&filename);

    // 3. Check cache first
    if bottle_cache_path.is_file() {
        debug!("Bottle found in cache: {}", bottle_cache_path.display());
        if !bottle_file_spec.sha256.is_empty() {
            match verify_bottle_checksum(&bottle_cache_path, &bottle_file_spec.sha256) {
                Ok(_) => { info!("Using valid cached bottle: {}", bottle_cache_path.display()); return Ok(bottle_cache_path); }
                Err(e) => {
                    warn!("Cached bottle checksum mismatch ({}): {}. Redownloading.", bottle_cache_path.display(), e);
                    if let Err(remove_err) = fs::remove_file(&bottle_cache_path) { warn!("Failed to remove corrupted cached bottle {}: {}", bottle_cache_path.display(), remove_err); }
                }
            }
        } else {
             info!("Using cached bottle (checksum not specified): {}", bottle_cache_path.display());
             return Ok(bottle_cache_path);
        }
    } else {
        debug!("Bottle not found in cache.");
    }

    // 4. Determine if the URL is a direct OCI blob URL
    let bottle_url_str = &bottle_file_spec.url;
    let registry_domain = config.artifact_domain.as_deref().unwrap_or(oci::DEFAULT_GHCR_DOMAIN);
    let is_oci_blob_url = (bottle_url_str.contains("://ghcr.io/") || bottle_url_str.contains(registry_domain))
                          && bottle_url_str.contains("/blobs/sha256:");
    debug!("Checking URL type: '{}'. Is OCI Blob URL? {}", bottle_url_str, is_oci_blob_url);

    // 5. Conditional Download Logic
    if is_oci_blob_url {
        info!("Detected OCI blob URL, initiating direct blob download: {}", bottle_url_str);
        match oci::download_oci_blob(bottle_url_str, &bottle_cache_path, config, client).await {
            Ok(_) => { info!("Successfully downloaded OCI blob to {}", bottle_cache_path.display()); }
            Err(e) => {
                error!("Failed to download OCI blob from {}: {}", bottle_url_str, e);
                let _ = fs::remove_file(&bottle_cache_path);
                 return Err(SapphireError::DownloadError(formula.name.clone(), bottle_url_str.to_string(), format!("Failed to download OCI blob: {}", e) ));
            }
        }
    } else {
        info!("Detected standard HTTPS URL, using direct download for: {}", bottle_url_str);
        match download_direct_url(client, bottle_url_str, &bottle_cache_path).await {
             Ok(_) => { info!("Successfully downloaded directly to {}", bottle_cache_path.display()); }
             Err(e) => {
                 error!("Failed to download directly from {}: {}", bottle_url_str, e);
                 let _ = fs::remove_file(&bottle_cache_path);
                  return Err(SapphireError::DownloadError(formula.name.clone(), bottle_url_str.to_string(), format!("Direct download failed: {}", e) ));
             }
        }
    }

    // 6. Verify checksum (if provided) after successful download
    if !bottle_file_spec.sha256.is_empty() {
        verify_bottle_checksum(&bottle_cache_path, &bottle_file_spec.sha256)?;
    } else {
         warn!("Skipping checksum verification for {} as none was provided in the spec.", formula.name);
    }

    info!("Bottle download successful: {}", bottle_cache_path.display());
    Ok(bottle_cache_path)
}

/// Finds the bottle information for the current platform based on the formula data.
fn get_bottle_for_platform(formula: &Formula) -> Result<(String, &BottleFileSpec)> {
    let stable_spec = formula.bottle.stable.as_ref()
        .ok_or_else(|| SapphireError::Generic(format!("Formula '{}' has no stable bottle specification.", formula.name)))?;

    let current_platform = super::get_current_platform();
    debug!("Determining bottle for current platform: {}", current_platform);
    debug!("Available bottle platforms in formula spec: {:?}", stable_spec.files.keys());

    if let Some(spec) = stable_spec.files.get(&current_platform) {
        debug!("Found exact bottle match for platform: {}", current_platform);
        return Ok((current_platform, spec));
    }
    debug!("No exact match found for {}", current_platform);

    // Fallback logic remains the same...
    if current_platform.starts_with("arm64_") {
        if let Some(os_name) = current_platform.strip_prefix("arm64_") {
             if let Some(spec) = stable_spec.files.get(os_name) { warn!("No specific arm64 bottle found for {}. Falling back to '{}' bottle.", current_platform, os_name); return Ok((os_name.to_string(), spec)); }
             debug!("No fallback bottle found for ARM OS: {}", os_name);
        }
    }
     if !current_platform.starts_with("arm64_") && current_platform.contains('_') {
         if let Some(os_name) = current_platform.split('_').last() {
             if let Some(spec) = stable_spec.files.get(os_name) { warn!("No specific arch bottle found for {}. Falling back to '{}' bottle.", current_platform, os_name); return Ok((os_name.to_string(), spec)); }
              debug!("No fallback bottle found for OS: {}", os_name);
         }
     } else if !current_platform.starts_with("arm64_") && !current_platform.contains('_') {
         let intel_platform_name = format!("x86_64_{}", current_platform);
          if let Some(spec) = stable_spec.files.get(&intel_platform_name) { warn!("No specific non-arch bottle found for {}. Falling back to '{}' bottle.", current_platform, intel_platform_name); return Ok((intel_platform_name, spec)); }
         debug!("No fallback bottle found for Intel platform: {}", intel_platform_name);
     }
    if let Some(spec) = stable_spec.files.get("all") { warn!("No platform-specific bottle found for {}. Using 'all' platform bottle.", current_platform); return Ok(("all".to_string(), spec)); }
    debug!("No 'all' platform bottle found.");

    Err(SapphireError::DownloadError( formula.name.clone(), "".to_string(), format!("No compatible bottle found for platform '{}'. Available: {:?}", current_platform, stable_spec.files.keys().collect::<Vec<_>>()) ))
}

/// Generates a conventional filename for the bottle cache.
fn generate_bottle_filename(formula: &Formula, platform_tag: &str) -> String {
    format!( "{}-{}.{}.bottle.tar.gz", formula.name, formula.version_str_full(), platform_tag )
}

/// Helper for direct URL download (non-OCI).
async fn download_direct_url(client: &Client, url: &str, destination_path: &Path) -> Result<()> {
    debug!("Downloading directly from URL: {}", url);
    let response = client.get(url).send().await.map_err(SapphireError::Http)?;
     if !response.status().is_success() {
         let status = response.status();
         let body_text = response.text().await.unwrap_or_else(|_| "Failed to read error response body".to_string());
         error!("Direct download failed for {}: Status {} - {}", url, status, body_text);
         return Err(SapphireError::Api(format!("HTTP error {} for URL {}: {}", status, url, body_text )));
     }
     let temp_filename = format!(".{}.download", destination_path.file_name().unwrap_or_default().to_string_lossy());
     let temp_path = destination_path.with_file_name(temp_filename);
     if temp_path.exists() { if let Err(e) = fs::remove_file(&temp_path) { warn!("Could not remove existing temp file {}: {}", temp_path.display(), e); }}
     {
         let mut dest_file = File::create(&temp_path).map_err(SapphireError::Io)?;
         let content = response.bytes().await.map_err(SapphireError::Http)?;
         copy(&mut content.as_ref(), &mut dest_file).map_err(SapphireError::Io)?;
     }
     fs::rename(&temp_path, destination_path).map_err(SapphireError::Io)?;
     debug!("Direct download complete: {}", destination_path.display());
     Ok(())
}

/// Verifies the SHA256 checksum of a downloaded bottle file.
pub fn verify_bottle_checksum(bottle_path: &Path, expected_sha256: &str) -> Result<()> {
    // (Checksum logic remains the same)
    if expected_sha256.is_empty() { warn!("Skipping checksum verification for {} - no expected checksum provided.", bottle_path.display()); return Ok(()); }
    info!("Verifying checksum for bottle: {}", bottle_path.display());
    let mut file = File::open(bottle_path).map_err(SapphireError::Io)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = file.read(&mut buffer).map_err(SapphireError::Io)?;
        if bytes_read == 0 { break; }
        hasher.update(&buffer[..bytes_read]);
    }
    let hash_result = hasher.finalize();
    let actual_sha256 = hex::encode(hash_result);
    debug!("Expected SHA256: {}", expected_sha256);
    debug!("Actual SHA256:   {}", actual_sha256);
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        error!("Checksum mismatch for {}. Expected: {}, Got: {}", bottle_path.display(), expected_sha256, actual_sha256);
        if let Err(e) = fs::remove_file(bottle_path) { warn!("Failed to remove corrupted bottle file {}: {}", bottle_path.display(), e); }
        return Err(SapphireError::ChecksumError(format!( "Checksum mismatch for bottle {}. Expected: {}, got: {}", bottle_path.display(), expected_sha256, actual_sha256 )));
    }
    info!("Bottle checksum verified successfully.");
    Ok(())
}

/// Install a bottle by extracting it, performing relocation, and writing receipt.
pub fn install_bottle(bottle_path: &Path, formula: &Formula, config: &Config) -> Result<PathBuf> {
    let install_dir = super::get_formula_cellar_path(formula);

    if let Some(parent_dir) = install_dir.parent() { fs::create_dir_all(parent_dir).map_err(SapphireError::Io)?; }
    else { warn!("Could not determine parent directory for install path: {}", install_dir.display()); }
     fs::create_dir_all(&install_dir).map_err(SapphireError::Io)?;

    println!("==> Installing {} from bottle", formula.name());
    println!("==> Pouring {} into {}", bottle_path.file_name().unwrap_or_default().to_string_lossy(), install_dir.display());

    // 1. Extract with strip_components=1
    crate::build::extract_archive_strip_components(bottle_path, &install_dir, 1)?;

    // *** 2. Ensure write permissions before relocation ***
    info!("==> Ensuring write permissions for extracted files");
    ensure_write_permissions(&install_dir)?;

    // 3. Perform relocation
    info!("==> Performing bottle relocation");
    perform_bottle_relocation(formula, &install_dir, config)?;

    // 4. Write the receipt *after* relocation
    crate::build::write_receipt(formula, &install_dir)?;

    info!("Bottle installation complete for {} at {}", formula.name, install_dir.display());
    Ok(install_dir)
}

// *** New Function: ensure_write_permissions ***
/// Recursively ensures owner write permissions for files and directories.
fn ensure_write_permissions(path: &Path) -> Result<()> {
    for entry_result in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        let entry_path = entry_result.path();
        match fs::metadata(entry_path) {
            Ok(metadata) => {
                let mut perms = metadata.permissions();
                let current_mode = perms.mode();
                // Add owner write permission (u+w -> 0o200)
                let new_mode = current_mode | 0o200;
                if new_mode != current_mode {
                    perms.set_mode(new_mode);
                    if let Err(e) = fs::set_permissions(entry_path, perms) {
                        warn!("Failed to set write permission on {}: {}", entry_path.display(), e);
                        // Continue anyway, maybe relocation will still work for some files
                    } else {
                         debug!("Set write permission on: {}", entry_path.display());
                    }
                }
            }
            Err(e) => {
                warn!("Failed to get metadata for {}: {}", entry_path.display(), e);
            }
        }
    }
    Ok(())
}


/// Performs text file relocation within the installed keg directory.
fn perform_bottle_relocation(formula: &Formula, install_dir: &Path, config: &Config) -> Result<()> {
    let cellar_placeholder = "@@HOMEBREW_CELLAR@@";
    let prefix_placeholder = "@@HOMEBREW_PREFIX@@";
    let cellar_path_str = config.cellar.to_string_lossy();
    let prefix_path_str = config.prefix.to_string_lossy();
    let formula_opt_path = config.prefix.join("opt").join(formula.name());
    let formula_opt_path_str = formula_opt_path.to_string_lossy();

    debug!("Starting relocation in: {}", install_dir.display());
    debug!("  Replacing '{}' with '{}'", cellar_placeholder, cellar_path_str);
    debug!("  Replacing '{}' with '{}'", prefix_placeholder, prefix_path_str);
    debug!("  Replacing formula opt placeholder with '{}'", formula_opt_path_str);

    let mut replaced_count = 0;
    let mut permission_errors = 0; // Count permission errors during write

    for entry_result in WalkDir::new(install_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry_result.path();
        if path.is_file() {
            // Check if file is writable before attempting read/write
            let metadata = match fs::metadata(path) {
                Ok(m) => m,
                Err(e) => { warn!("Relocation: Could not get metadata for {}: {}", path.display(), e); continue; } // Skip if metadata fails
            };
             if metadata.permissions().readonly() {
                 // This check might be too simple if group/other write is needed,
                 // but owner write is the most common requirement.
                 // The ensure_write_permissions step should have handled this,
                 // but double-check might catch edge cases or if ensure step failed silently.
                 debug!("  Skipping relocation for readonly file: {}", path.display());
                 continue;
             }


            // Simple text check (as before)
            let mut is_likely_text = false;
            if let Ok(mut file) = File::open(path) {
                let mut buffer = [0; 1024];
                if let Ok(n) = file.read(&mut buffer) { if !buffer[..n].contains(&0) { is_likely_text = true; } }
            }

            if is_likely_text {
                match fs::read_to_string(path) {
                     Ok(mut content) => {
                         let mut modified = false;

                         if content.contains(cellar_placeholder) { content = content.replace(cellar_placeholder, &cellar_path_str); modified = true; }
                         if content.contains(prefix_placeholder) { content = content.replace(prefix_placeholder, &prefix_path_str); modified = true; }
                         let formula_opt_placeholder = format!("@@HOMEBREW_OPT_{}@@", formula.name().to_uppercase().replace('-', "_"));
                         if content.contains(&formula_opt_placeholder) { content = content.replace(&formula_opt_placeholder, &formula_opt_path_str); modified = true; }

                         if modified {
                             // Attempt write
                             match fs::write(path, content) {
                                 Ok(_) => {
                                     debug!("  Relocated placeholders in: {}", path.display());
                                     replaced_count += 1;
                                 }
                                 Err(e) => {
                                     // *** Explicitly check for Permission Denied ***
                                     if e.kind() == std::io::ErrorKind::PermissionDenied {
                                         error!("  Failed to write relocated file {}: Permission denied (os error 13)", path.display());
                                         permission_errors += 1;
                                     } else {
                                         // Log other write errors as warnings
                                         warn!("  Failed to write relocated file {}: {}", path.display(), e);
                                     }
                                 }
                             }
                         }
                     }
                     Err(e) => { debug!("  Skipping relocation for {} (read failed or likely binary): {}", path.display(), e); }
                }
            } else {
                 debug!("  Skipping relocation for {} (likely binary)", path.display());
            }
        }
    }

    if permission_errors > 0 {
        error!("Relocation failed for {} files due to permission errors. The installation might be broken.", permission_errors);
        // Return a specific error if permissions caused failures
        return Err(SapphireError::InstallError(format!(
             "Bottle relocation failed for {} files due to permissions. Check ownership/permissions of {}",
             permission_errors, install_dir.display()
        )));
    } else if replaced_count > 0 {
        info!("Relocation complete. {} text files modified.", replaced_count);
    } else {
        info!("Relocation complete. No text file modifications needed.");
    }
    Ok(())
}