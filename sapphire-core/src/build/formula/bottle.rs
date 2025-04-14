// **File:** sapphire-core/src/build/formula/bottle.rs

use crate::utils::error::{SapphireError, Result};
use crate::model::formula::{Formula, BottleFileSpec, FormulaDependencies}; // Added FormulaDependencies
use crate::utils::config::Config;
use crate::fetch::oci;
use std::path::{Path, PathBuf};
use std::fs;
use log::{debug, error, info, warn};
use reqwest::Client;
use std::fs::File;
use std::io::{copy, Read}; // Removed unused Write
use sha2::{Sha256, Digest};
use hex;
use walkdir::WalkDir;
use std::os::unix::fs::PermissionsExt;
use std::collections::HashMap;
use regex::Regex;

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
    // Allow empty SHA for now, just warn
    // if bottle_file_spec.sha256.is_empty() {
    //     warn!("Bottle spec for {} ({}) is missing SHA256 checksum!", formula.name, platform_tag);
    // }

    // 2. Determine the target cache path
    // Use the *standard* version string for the cache filename
    let standard_version_str = formula.version_str_full(); // e.g., 1.1_5
    let filename = format!( "{}-{}.{}.bottle.tar.gz", formula.name, standard_version_str, platform_tag );
    // let filename = generate_bottle_filename(formula, &platform_tag); // Keep if generate_bottle_filename uses version_str_full()
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
                // Attempt to remove partially downloaded file on error
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
                 // Attempt to remove partially downloaded file on error
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

    // Use the existing function, but maybe add more robust error handling if sw_vers fails
    let current_platform = crate::build::formula::get_current_platform();
    if current_platform == "unknown" || current_platform.contains("unknown") {
        warn!("Could not reliably determine macOS platform. Bottle selection might be incorrect.");
        // Depending on requirements, might want to error here instead of potentially getting the wrong bottle.
        // return Err(SapphireError::Generic("Failed to determine macOS platform for bottle selection.".to_string()));
    }
    debug!("Determining bottle for current platform: {}", current_platform);
    debug!("Available bottle platforms in formula spec: {:?}", stable_spec.files.keys());

    if let Some(spec) = stable_spec.files.get(&current_platform) {
        debug!("Found exact bottle match for platform: {}", current_platform);
        return Ok((current_platform.clone(), spec)); // Clone current_platform string
    }
    debug!("No exact match found for {}", current_platform);

    // Fallback logic (OS name only for ARM, or generic OS name for Intel/Linux)
    if current_platform.starts_with("arm64_") {
        if let Some(os_name) = current_platform.strip_prefix("arm64_") {
             if let Some(spec) = stable_spec.files.get(os_name) {
                 warn!("No specific arm64 bottle found for {}. Falling back to '{}' bottle.", current_platform, os_name);
                 return Ok((os_name.to_string(), spec));
             }
             debug!("No fallback bottle found for ARM OS: {}", os_name);
        }
    }
    // Check for OS name fallback for non-ARM architectures
    else if let Some(os_name) = current_platform.split('_').last() {
         if os_name != current_platform && stable_spec.files.contains_key(os_name) { // Ensure os_name is different and exists
            if let Some(spec) = stable_spec.files.get(os_name) {
                warn!("No specific arch bottle found for {}. Falling back to '{}' bottle.", current_platform, os_name);
                return Ok((os_name.to_string(), spec));
            }
            debug!("No fallback bottle found for OS: {}", os_name);
         }
         // Check x86_64 specific OS name if applicable (e.g., x86_64_linux -> linux)
         else if current_platform.starts_with("x86_64_") {
              if let Some(os_name_intel) = current_platform.strip_prefix("x86_64_") {
                  if let Some(spec) = stable_spec.files.get(os_name_intel) {
                      warn!("No specific x86_64 bottle found for {}. Falling back to '{}' bottle.", current_platform, os_name_intel);
                      return Ok((os_name_intel.to_string(), spec));
                  }
                  debug!("No fallback bottle found for Intel OS: {}", os_name_intel);
              }
         }
    }

    // Fallback to "all" platform
    if let Some(spec) = stable_spec.files.get("all") {
        warn!("No platform-specific bottle found for {}. Using 'all' platform bottle.", current_platform);
        return Ok(("all".to_string(), spec));
    }
    debug!("No 'all' platform bottle found.");


    Err(SapphireError::DownloadError( formula.name.clone(), "".to_string(), format!("No compatible bottle found for platform '{}'. Available: {:?}", current_platform, stable_spec.files.keys().collect::<Vec<_>>()) ))
}

// /// Generates a conventional filename for the bottle cache.
// fn generate_bottle_filename(formula: &Formula, platform_tag: &str) -> String {
//     format!( "{}-{}.{}.bottle.tar.gz", formula.name, formula.version_str_full(), platform_tag )
// }

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

     // Download to temporary file to avoid partial files on error
     let temp_filename = format!(".{}.download", destination_path.file_name().unwrap_or_default().to_string_lossy());
     let temp_path = destination_path.with_file_name(temp_filename);
     // Ensure temp file from previous failed attempts is removed
     if temp_path.exists() {
         if let Err(e) = fs::remove_file(&temp_path) {
             warn!("Could not remove existing temp file {}: {}", temp_path.display(), e);
         }
     }

     { // Scope for File object
         let mut dest_file = File::create(&temp_path).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed to create temp file {}: {}", temp_path.display(), e))))?;
         let content = response.bytes().await.map_err(SapphireError::Http)?;
         copy(&mut content.as_ref(), &mut dest_file).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed to write download to {}: {}", temp_path.display(), e))))?;
     } // dest_file is closed here

     // Rename temp file to final destination
     fs::rename(&temp_path, destination_path).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed to move temp file {} to {}: {}", temp_path.display(), destination_path.display(), e))))?;
     debug!("Direct download complete: {}", destination_path.display());
     Ok(())
}

/// Verifies the SHA256 checksum of a downloaded bottle file.
pub fn verify_bottle_checksum(bottle_path: &Path, expected_sha256: &str) -> Result<()> {
    if expected_sha256.is_empty() {
        warn!("Skipping checksum verification for {} - no expected checksum provided.", bottle_path.display());
        return Ok(());
    }
    info!("Verifying checksum for bottle: {}", bottle_path.display());
    let mut file = File::open(bottle_path).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed to open file for checksum {}: {}", bottle_path.display(), e))))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192]; // Read in chunks
    loop {
        let bytes_read = file.read(&mut buffer).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed to read file for checksum {}: {}", bottle_path.display(), e))))?;
        if bytes_read == 0 { break; }
        hasher.update(&buffer[..bytes_read]);
    }
    let hash_result = hasher.finalize();
    let actual_sha256 = hex::encode(hash_result);

    debug!("Expected SHA256: {}", expected_sha256);
    debug!("Actual SHA256:   {}", actual_sha256);

    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        error!("Checksum mismatch for {}. Expected: {}, Got: {}", bottle_path.display(), expected_sha256, actual_sha256);
        // Don't remove the file here, let the caller handle redownload decision
        return Err(SapphireError::ChecksumError(format!( "Checksum mismatch for bottle {}. Expected: {}, got: {}", bottle_path.display(), expected_sha256, actual_sha256 )));
    }
    info!("Bottle checksum verified successfully.");
    Ok(())
}


/// Install a bottle by extracting it, performing relocation, and writing receipt.
pub fn install_bottle(bottle_path: &Path, formula: &Formula, config: &Config) -> Result<PathBuf> {
    // *** FIX: Use the FormulaDependencies trait to get the install path ***
    let install_dir = match formula.install_prefix(&config.cellar) {
        Ok(path) => path,
        Err(e) => return Err(SapphireError::InstallError(format!("Could not determine install path for {}: {}", formula.name(), e))),
    };

    // Ensure keg directory is clean before installing
    if install_dir.exists() {
        info!("Removing existing keg directory before installing: {}", install_dir.display());
        fs::remove_dir_all(&install_dir).map_err(|e| SapphireError::InstallError(format!("Failed to remove existing keg {}: {}", install_dir.display(), e)))?;
    }

    // Create parent directory (e.g., /opt/homebrew/Cellar/asciiquarium)
    if let Some(parent_dir) = install_dir.parent() {
        fs::create_dir_all(parent_dir).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed to create parent dir {}: {}", parent_dir.display(), e))))?;
    } else {
        // This case should be rare/impossible for a cellar path, but handle it
        return Err(SapphireError::InstallError(format!("Could not determine parent directory for install path: {}", install_dir.display())));
    }
    // Create the specific versioned keg directory
    fs::create_dir_all(&install_dir).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed to create keg dir {}: {}", install_dir.display(), e))))?;

    println!("==> Installing {} from bottle", formula.name());
    println!("==> Pouring {} into {}", bottle_path.file_name().unwrap_or_default().to_string_lossy(), install_dir.display());

    // 1. Extract with strip_components=1 into the standard keg directory
    crate::build::extract_archive_strip_components(bottle_path, &install_dir, 1)?;

    // *** 2. Ensure write permissions before relocation ***
    info!("==> Ensuring write permissions for extracted files in {}", install_dir.display());
    ensure_write_permissions(&install_dir)?;

    // 3. Perform relocation
    info!("==> Performing bottle relocation in {}", install_dir.display());
    perform_bottle_relocation(formula, &install_dir, config)?;

    // 4. Write the receipt *after* relocation
    crate::build::write_receipt(formula, &install_dir)?;

    info!("Bottle installation complete for {} at {}", formula.name(), install_dir.display());
    Ok(install_dir) // Return the actual installation directory path
}

/// Recursively ensures owner write permissions for files and directories.
fn ensure_write_permissions(path: &Path) -> Result<()> {
    for entry_result in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        let entry_path = entry_result.path();
        match fs::metadata(entry_path) {
            Ok(metadata) => {
                let mut perms = metadata.permissions();
                let current_mode = perms.mode();
                // Add owner write permission (u+w -> 0o200) if it's not already set
                let new_mode = current_mode | 0o200;
                if new_mode != current_mode {
                    perms.set_mode(new_mode);
                    if let Err(e) = fs::set_permissions(entry_path, perms) {
                        // Log as warning, as relocation might still work for some files
                        warn!("Failed to set write permission on {}: {}", entry_path.display(), e);
                    } else {
                         debug!("Set write permission on: {}", entry_path.display());
                    }
                }
            }
            Err(e) => {
                // Log error getting metadata but continue
                warn!("Failed to get metadata for {}: {}", entry_path.display(), e);
            }
        }
    }
    Ok(())
}


/// Performs text file relocation within the installed keg directory.
fn perform_bottle_relocation(formula: &Formula, install_dir: &Path, config: &Config) -> Result<()> {
    // --- Define Placeholders and Replacements ---
    let mut replacements = HashMap::new();

    // *** FIX: Convert PathBuf results to owned String immediately ***
    let cellar_path_str = config.cellar.to_string_lossy().to_string();
    let prefix_path_str = config.prefix.to_string_lossy().to_string();
    // Assume standard core tap location for repository placeholder
    let repo_path = config.prefix.join("Library/Taps/homebrew/homebrew-core");
    let repo_path_str = repo_path.to_string_lossy().to_string();
    let library_path = config.prefix.join("Library");
    let library_path_str = library_path.to_string_lossy().to_string();

    // Standard placeholders
    replacements.insert("@@HOMEBREW_CELLAR@@".to_string(), cellar_path_str);
    replacements.insert("@@HOMEBREW_PREFIX@@".to_string(), prefix_path_str);
    replacements.insert("@@HOMEBREW_REPOSITORY@@".to_string(), repo_path_str);
    replacements.insert("@@HOMEBREW_LIBRARY@@".to_string(), library_path_str);

    // Formula opt path placeholder
    let formula_opt_path = config.prefix.join("opt").join(formula.name());
    let formula_opt_path_str = formula_opt_path.to_string_lossy().to_string();
    let formula_opt_placeholder = format!("@@HOMEBREW_OPT_{}@@", formula.name().to_uppercase().replace('-', "_"));
    replacements.insert(formula_opt_placeholder, formula_opt_path_str);

    // Perl placeholder
    // Determine the correct perl path. Check if perl is a direct dependency.
    let perl_path = formula.dependencies.iter().find(|d| d.name == "perl")
        .map(|_| config.prefix.join("opt").join("perl").join("bin").join("perl"))
        .or_else(|| {
            // Fallback logic (e.g., system perl on macOS if not a direct dependency)
            if cfg!(target_os = "macos") {
                debug!("Perl not a direct dependency, assuming system perl for @@HOMEBREW_PERL@@");
                Some(PathBuf::from("/usr/bin/perl"))
            } else {
                debug!("Perl not a direct dependency and not macOS, using brewed path as default for @@HOMEBREW_PERL@@");
                // Use the same opt path logic, even if Perl isn't explicitly listed as a dep here
                Some(config.prefix.join("opt").join("perl").join("bin").join("perl"))
            }
        })
        // Ensure the determined path exists before adding it to replacements
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string()) // Convert valid path to String
        .unwrap_or_else(|| {
             warn!("Could not determine a valid path for @@HOMEBREW_PERL@@ replacement. Placeholder might remain.");
             // Provide a default non-functional value or the placeholder itself?
             // Let's use the placeholder itself to make the failure obvious if replacement needed.
             "@@HOMEBREW_PERL@@".to_string()
        });

    // Only add the replacement if we found a valid path
    if perl_path != "@@HOMEBREW_PERL@@" {
        replacements.insert("@@HOMEBREW_PERL@@".to_string(), perl_path.clone());
    }


    // Compile regex for shebang handling (match #! followed by optional /usr/bin/env and then the placeholder)
    // Make it lazy (?) to match the shortest path containing the placeholder. Ensure it handles paths correctly.
    // Updated regex to be non-greedy and handle potential spaces/args after placeholder
    // This regex seems overly complex and might not capture all cases robustly.
    // A simpler approach might be needed if this still fails.
    let _shebang_regex = Regex::new(r"^(#!.*?(?:/usr/bin/env\s+)?)((?:/[^/\s]+)*?)(@@HOMEBREW_[A-Z_]+@@)((?:/[^/\s]+)*?.*?)\s*$").unwrap();


    debug!("Starting relocation in: {}", install_dir.display());
    for (placeholder, replacement) in &replacements {
        debug!("  Replacing '{}' with '{}'", placeholder, replacement);
    }

    let mut replaced_count = 0;
    let mut permission_errors = 0;

    for entry_result in WalkDir::new(install_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry_result.path();
        if path.is_file() {
            // Skip if metadata fails
            let metadata = match fs::metadata(path) {
                Ok(m) => m,
                Err(e) => { warn!("Relocation: Could not get metadata for {}: {}", path.display(), e); continue; }
            };
             // Skip if not writable (ensure_write_permissions should have handled this)
             if metadata.permissions().readonly() {
                 // This check is simplified; ideally, check effective user's write permission.
                 debug!("  Skipping relocation for readonly file: {}", path.display());
                 continue;
             }

            // Simple text check - read a small chunk to detect null bytes
            let mut is_likely_text = false;
            match File::open(path) {
                Ok(mut file) => {
                    let mut buffer = [0; 1024];
                    if let Ok(n) = file.read(&mut buffer) {
                        if !buffer[..n].contains(&0) {
                            is_likely_text = true;
                        }
                    } else {
                         warn!("  Could not read chunk from {} to check if text, assuming binary.", path.display());
                    }
                }
                Err(e) => {
                     warn!("  Could not open {} to check if text, skipping relocation: {}", path.display(), e);
                     continue; // Skip if we can't even open it
                }
            }


            if is_likely_text {
                // Read the whole file now we know it's likely text
                match fs::read_to_string(path) {
                     Ok(content) => {
                         let mut modified_content = content.clone();
                         let mut modified = false;

                         // Handle shebang replacement first
                         if content.starts_with("#!") {
                            // Find the end of the first line
                             let first_line_end_pos = content.find('\n').unwrap_or_else(|| content.len());
                             let first_line = &content[..first_line_end_pos];

                             let mut current_line_content = first_line.to_string();
                             let mut shebang_modified = false;

                             // Iterate through known placeholders for shebang replacement
                             for (placeholder, replacement) in &replacements {
                                 if placeholder.starts_with("@@HOMEBREW_") && current_line_content.contains(placeholder) {
                                     // More robust replacement: avoid replacing parts of other placeholders
                                     // Simple replace might be okay here if placeholders are distinct enough
                                     current_line_content = current_line_content.replace(placeholder, replacement);
                                     debug!("  Relocated SHEBANG placeholder '{}' in: {}", placeholder, path.display());
                                     shebang_modified = true;
                                 }
                             }

                             // If the shebang was modified, update modified_content
                             if shebang_modified {
                                modified_content = format!("{}{}", current_line_content, &content[first_line_end_pos..]);
                                modified = true;
                             }
                         }

                         // Perform standard replacements on the potentially modified content (avoiding double-replace of shebang)
                         for (placeholder, replacement) in &replacements {
                             // Check if the placeholder exists *outside* the potentially modified shebang line
                             let search_area = if modified && content.starts_with("#!") {
                                 let first_line_end_pos = modified_content.find('\n').unwrap_or_else(|| modified_content.len());
                                 &modified_content[first_line_end_pos..]
                             } else {
                                 &modified_content[..]
                             };

                             if search_area.contains(placeholder) {
                                 let original_len = modified_content.len();
                                 // Replace globally in the modified_content
                                 modified_content = modified_content.replace(placeholder, replacement);
                                 if !modified { // Only log first modification type if shebang wasn't modified
                                     debug!("  Relocated standard placeholder '{}' in: {}", placeholder, path.display());
                                 }
                                 modified = modified || (modified_content.len() != original_len); // Check if length changed
                             }
                         }

                         // Attempt write only if content actually changed from the original read
                         if modified && modified_content != content {
                             match fs::write(path, &modified_content) { // Pass content by reference
                                 Ok(_) => {
                                     replaced_count += 1;
                                 }
                                 Err(e) => {
                                     if e.kind() == std::io::ErrorKind::PermissionDenied {
                                         error!("  Failed to write relocated file {}: Permission denied", path.display());
                                         permission_errors += 1;
                                     } else {
                                         warn!("  Failed to write relocated file {}: {}", path.display(), e);
                                         // Optionally count other write errors?
                                     }
                                 }
                             }
                         }
                     }
                     Err(e) => { debug!("  Skipping relocation for {} (read failed): {}", path.display(), e); }
                }
            } else {
                 debug!("  Skipping relocation for {} (likely binary)", path.display());
                 // TODO: Add binary relocation logic here if needed (RPATH modification)
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