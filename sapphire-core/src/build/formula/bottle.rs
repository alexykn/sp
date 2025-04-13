// **File:** sapphire-core/src/build/formula/bottle.rs

use crate::utils::error::{SapphireError, Result};
use crate::model::formula::{Formula, BottleFileSpec};
use crate::utils::config::Config;
use crate::fetch::oci::{self, OciManifestIndex, OciManifestDescriptor}; // Import OCI functions and structs
use std::path::{Path, PathBuf};
use std::fs;
use log::{debug, error, info, warn}; // Use log crate
use reqwest::Client; // Import reqwest client
use url::Url;
use std::fs::File;
use std::io::copy;


/// Downloads and verifies a bottle for the given formula using the OCI flow if necessary.
pub async fn download_bottle(formula: &Formula, config: &Config, client: &Client) -> Result<PathBuf> {
    info!("Attempting to download bottle for {}", formula.name);

    // 1. Determine the correct platform and bottle spec
    let (platform_tag, bottle_file_spec) = get_bottle_for_platform(formula)?;
    debug!("Selected bottle spec for platform '{}': URL={}, SHA256={}",
           platform_tag, bottle_file_spec.url, bottle_file_spec.sha256);

    if bottle_file_spec.url.is_empty() {
        return Err(SapphireError::DownloadError(
            formula.name.clone(),
            "".to_string(),
            "Bottle spec has an empty URL.".to_string(),
        ));
    }
    if bottle_file_spec.sha256.is_empty() {
        warn!("Bottle spec for {} ({}) is missing SHA256 checksum!", formula.name, platform_tag);
        // Proceed without checksum verification? Or error? Let's proceed with warning for now.
    }

    // 2. Determine the target cache path
    let filename = generate_bottle_filename(formula, &platform_tag);
    let cache_dir = config.cache_dir.join("bottles"); // Store bottles in a sub-directory
    fs::create_dir_all(&cache_dir).map_err(|e| SapphireError::Io(e))?;
    let bottle_cache_path = cache_dir.join(&filename);

    // 3. Check cache first
    if bottle_cache_path.is_file() {
        debug!("Bottle found in cache: {}", bottle_cache_path.display());
        if !bottle_file_spec.sha256.is_empty() {
            match verify_bottle_checksum(&bottle_cache_path, &bottle_file_spec.sha256) {
                Ok(_) => {
                    info!("Using valid cached bottle: {}", bottle_cache_path.display());
                    return Ok(bottle_cache_path);
                }
                Err(e) => {
                    warn!(
                        "Cached bottle checksum mismatch ({}): {}. Redownloading.",
                        bottle_cache_path.display(), e
                    );
                    if let Err(remove_err) = fs::remove_file(&bottle_cache_path) {
                        warn!("Failed to remove corrupted cached bottle {}: {}", bottle_cache_path.display(), remove_err);
                    }
                }
            }
        } else {
             info!("Using cached bottle (checksum not specified): {}", bottle_cache_path.display());
             return Ok(bottle_cache_path); // Use cached file if no checksum to verify against
        }
    } else {
        debug!("Bottle not found in cache.");
    }


    // 4. Determine the registry domain and if OCI flow is needed
    let bottle_url_str = &bottle_file_spec.url;
    let registry_domain = config.artifact_domain.as_deref().unwrap_or(oci::DEFAULT_GHCR_DOMAIN); // Use config or default

    // --- OCI Download Flow ---
    // Heuristic: Assume URLs containing "/v2/" and the registry domain are OCI URLs
    // More robust checking might involve parsing the URL structure more carefully.
    if bottle_url_str.contains("/v2/") && bottle_url_str.contains(registry_domain) {
        info!("Detected OCI URL, initiating OCI download flow for: {}", bottle_url_str);

        // The bottle_url_str from the formula is often the *manifest* URL.
        let manifest_url = bottle_url_str;

        // Fetch the manifest index
        let manifest_index = match oci::fetch_oci_manifest_index(manifest_url, config, client).await {
             Ok(index) => index,
             Err(e) => {
                 error!("Failed to fetch OCI manifest index from {}: {}", manifest_url, e);
                 return Err(SapphireError::DownloadError(
                     formula.name.clone(),
                     manifest_url.to_string(),
                     format!("Failed to fetch OCI manifest: {}", e)
                 ));
             }
        };
        debug!("Fetched OCI Manifest Index: {:?}", manifest_index);

        // Find the correct blob descriptor from the manifest
        // We need to match the platform tag derived earlier.
        let blob_descriptor = find_blob_for_platform(&manifest_index, &platform_tag)
             .ok_or_else(|| SapphireError::DownloadError(
                 formula.name.clone(),
                 manifest_url.to_string(),
                 format!("No compatible blob found in OCI manifest for platform '{}'", platform_tag)
             ))?;
        debug!("Found blob descriptor for platform '{}': Digest={}, Size={}",
               platform_tag, blob_descriptor.digest, blob_descriptor.size);


        // Construct the blob URL (usually based on manifest URL and digest)
        // Example: https://ghcr.io/v2/homebrew/core/foo/blobs/sha256:<digest>
        let blob_url = construct_blob_url(manifest_url, &blob_descriptor.digest)?;
        info!("Constructed blob URL: {}", blob_url);

        // Download the blob using the OCI module
        match oci::download_oci_blob(&blob_url, &bottle_cache_path, config, client).await {
            Ok(_) => {
                 info!("Successfully downloaded OCI blob to {}", bottle_cache_path.display());
                 // Fall through to checksum verification
            }
            Err(e) => {
                error!("Failed to download OCI blob from {}: {}", blob_url, e);
                // Attempt to remove potentially partial download
                let _ = fs::remove_file(&bottle_cache_path);
                 return Err(SapphireError::DownloadError(
                     formula.name.clone(),
                     blob_url, // Report blob URL in error
                     format!("Failed to download OCI blob: {}", e)
                 ));
            }
        }

    } else {
        // --- Standard HTTPS Download Flow ---
        info!("Detected standard HTTPS URL, using direct download for: {}", bottle_url_str);
        // Use the existing http module functionality (or inline reqwest here)
        // For simplicity, let's adapt the OCI download_oci_blob logic slightly
        // Note: This assumes direct download doesn't need special auth beyond what reqwest handles.
        match download_direct_url(client, bottle_url_str, &bottle_cache_path).await {
             Ok(_) => {
                 info!("Successfully downloaded directly to {}", bottle_cache_path.display());
                 // Fall through to checksum verification
             }
             Err(e) => {
                 error!("Failed to download directly from {}: {}", bottle_url_str, e);
                 // Attempt to remove potentially partial download
                 let _ = fs::remove_file(&bottle_cache_path);
                  return Err(SapphireError::DownloadError(
                      formula.name.clone(),
                      bottle_url_str.to_string(),
                      format!("Direct download failed: {}", e)
                  ));
             }
        }
    }

    // 5. Verify checksum (if provided) after successful download
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

    let current_platform = super::get_current_platform(); // Get platform identifier string (e.g., "arm64_sonoma")
    debug!("Determining bottle for current platform: {}", current_platform);
    debug!("Available bottle platforms in formula spec: {:?}", stable_spec.files.keys());

    // Try exact match first
    if let Some(spec) = stable_spec.files.get(&current_platform) {
        debug!("Found exact bottle match for platform: {}", current_platform);
        return Ok((current_platform, spec));
    }
    debug!("No exact match found for {}", current_platform);

    // Fallback for ARM Macs (e.g., arm64_sonoma might use sonoma bottle if arm64 specific isn't present)
    if current_platform.starts_with("arm64_") {
        if let Some(os_name) = current_platform.strip_prefix("arm64_") {
            if let Some(spec) = stable_spec.files.get(os_name) {
                warn!("No specific arm64 bottle found for {}. Falling back to '{}' bottle.", current_platform, os_name);
                return Ok((os_name.to_string(), spec)); // Return the fallback platform tag
            }
             debug!("No fallback bottle found for ARM OS: {}", os_name);
        }
    }

    // Fallback for Intel Macs (less common, but e.g. monterey might be listed as x86_64_monterey sometimes)
     if !current_platform.starts_with("arm64_") && !current_platform.contains('_') { // e.g. "sonoma"
         let intel_platform_name = format!("x86_64_{}", current_platform);
         if let Some(spec) = stable_spec.files.get(&intel_platform_name) {
            warn!("No specific non-arch bottle found for {}. Falling back to '{}' bottle.", current_platform, intel_platform_name);
             return Ok((intel_platform_name, spec)); // Return the fallback platform tag
         }
         debug!("No fallback bottle found for Intel platform: {}", intel_platform_name);
     }


    // Try finding an "all" platform bottle if available
    if let Some(spec) = stable_spec.files.get("all") {
        warn!("No platform-specific bottle found for {}. Using 'all' platform bottle.", current_platform);
        return Ok(("all".to_string(), spec));
    }
    debug!("No 'all' platform bottle found.");

    // If no match or fallback found, return an error
    Err(SapphireError::DownloadError(
        formula.name.clone(),
        "".to_string(), // No specific URL known at this stage
        format!("No compatible bottle found for platform '{}'. Available: {:?}", current_platform, stable_spec.files.keys().collect::<Vec<_>>())
    ))
}

/// Generates a conventional filename for the bottle cache.
fn generate_bottle_filename(formula: &Formula, platform_tag: &str) -> String {
    format!(
        "{}-{}.{}.bottle.tar.gz",
        formula.name,
        formula.version_str_full(), // Includes revision if present
        platform_tag
    )
}

/// Finds the blob descriptor matching the target platform tag within the manifest index.
fn find_blob_for_platform<'a>(index: &'a OciManifestIndex, target_platform_tag: &str) -> Option<&'a OciManifestDescriptor> {
     // Homebrew platform tags often map directly to OCI platform fields, but require parsing.
     // Example: "arm64_sonoma" -> arch: "arm64", os: "darwin", os.version: "14.x" (needs mapping)
     // Example: "sonoma" -> arch: "amd64", os: "darwin", os.version: "14.x"

     // Simple matching first: check annotations used by brew
     for manifest in &index.manifests {
         if let Some(annotations) = &manifest.annotations {
             // Brew often uses "org.opencontainers.image.ref.name" for the tag
             if let Some(annotation_tag) = annotations.get("org.opencontainers.image.ref.name") {
                 if annotation_tag == target_platform_tag {
                      debug!("Found matching blob via annotation: {}", target_platform_tag);
                      return Some(manifest);
                 }
                 // Handle cases where annotation might have version suffix, e.g., "sonoma-1.2.3"
                 if let Some(base_tag) = annotation_tag.split('-').next() {
                      if base_tag == target_platform_tag {
                         debug!("Found matching blob via base annotation tag: {}", target_platform_tag);
                         return Some(manifest);
                     }
                 }
             }
         }
     }
     debug!("Could not find match based on 'org.opencontainers.image.ref.name' annotation.");

     // Fallback: More complex matching based on OCI platform fields (requires mapping target_platform_tag)
     // This needs a mapping from brew tags (like "arm64_sonoma") to OCI arch/os/version.
     // let (target_arch, target_os, target_os_version_prefix) = map_brew_tag_to_oci(target_platform_tag);
     // for manifest in &index.manifests {
     //     if let Some(platform) = &manifest.platform {
     //         if platform.architecture == target_arch && platform.os == target_os {
     //             // Check OS version prefix if needed
     //              if let Some(prefix) = target_os_version_prefix {
     //                  if platform.os_version.as_deref().unwrap_or("").starts_with(prefix) {
     //                      return Some(manifest)
     //                  }
     //              } else {
     //                  return Some(manifest) // Match if prefix is not specified
     //              }
     //         }
     //     }
     // }

     // If no match after all attempts
     warn!("Could not find suitable blob descriptor for platform tag: {}", target_platform_tag);
     None
}


/// Constructs the blob URL from the manifest URL and digest.
/// Assumes the standard OCI registry layout.
fn construct_blob_url(manifest_url_str: &str, digest: &str) -> Result<String> {
    let manifest_url = Url::parse(manifest_url_str)
        .map_err(|e| SapphireError::Generic(format!("Invalid manifest URL '{}': {}", manifest_url_str, e)))?;

    // Expect format like: https://ghcr.io/v2/homebrew/core/wget/manifests/1.2.3
    // Need to replace '/manifests/...' with '/blobs/<digest>'
    let mut path_segments: Vec<&str> = manifest_url.path_segments().ok_or_else(|| SapphireError::Generic("Manifest URL has no path segments".to_string()))?.collect();

    // Find "manifests" segment and replace it and subsequent parts
    if let Some(manifests_index) = path_segments.iter().position(|&s| s == "manifests") {
        path_segments.truncate(manifests_index); // Keep segments before "manifests"
    } else {
        // If "manifests" isn't found, maybe it's a base repo URL? Less common for bottles.
        // Or perhaps the URL structure is different. Assume standard for now.
        warn!("Could not find 'manifests' segment in URL '{}' to construct blob URL.", manifest_url_str);
        // Attempt a guess: append /blobs/<digest> directly? Unreliable.
        // Let's error if the structure is unexpected.
         return Err(SapphireError::Generic(format!("Unexpected manifest URL structure, cannot construct blob URL: {}", manifest_url_str)));
    }

    // Add "blobs" and the digest
    path_segments.push("blobs");
    path_segments.push(digest); // Digest already includes "sha256:" prefix usually

    // Reconstruct the URL
    let mut blob_url = manifest_url.clone();
    let new_path = format!("/{}", path_segments.join("/"));
    blob_url.set_path(&new_path);
    // Ensure query and fragment are cleared if they existed on manifest URL
    blob_url.set_query(None);
    blob_url.set_fragment(None);


    Ok(blob_url.to_string())
}


/// Helper for direct URL download (non-OCI).
async fn download_direct_url(client: &Client, url: &str, destination_path: &Path) -> Result<()> {
    debug!("Downloading directly from URL: {}", url);
    let response = client.get(url)
        .send()
        .await
        .map_err(|e| SapphireError::Http(e))?;

     if !response.status().is_success() {
         let status = response.status();
         let body_text = response.text().await.unwrap_or_else(|_| "Failed to read error response body".to_string());
         error!("Direct download failed for {}: Status {} - {}", url, status, body_text);
         return Err(SapphireError::Api(format!(
             "HTTP error {} for URL {}: {}",
             status, url, body_text
         )));
     }

     // Write to destination (consider temp file)
     let temp_filename = format!(
         ".{}.download",
         destination_path.file_name().unwrap_or_default().to_string_lossy()
     );
     let temp_path = destination_path.with_file_name(temp_filename);

     if temp_path.exists() { if let Err(e) = fs::remove_file(&temp_path) { warn!("Could not remove existing temp file {}: {}", temp_path.display(), e); }}

     {
         let mut dest_file = File::create(&temp_path).map_err(|e| SapphireError::Io(e))?;
         let content = response.bytes().await.map_err(|e| SapphireError::Http(e))?;
         copy(&mut content.as_ref(), &mut dest_file).map_err(|e| SapphireError::Io(e))?;
     }

     fs::rename(&temp_path, destination_path).map_err(|e| SapphireError::Io(e))?;
     debug!("Direct download complete: {}", destination_path.display());
     Ok(())
}


/// Verifies the SHA256 checksum of a downloaded bottle file.
pub fn verify_bottle_checksum(bottle_path: &Path, expected_sha256: &str) -> Result<()> {
    use sha2::{Sha256, Digest};
    use std::io::Read; // Import Read trait
    use hex; // Import hex crate

    if expected_sha256.is_empty() {
        warn!("Skipping checksum verification for {} - no expected checksum provided.", bottle_path.display());
        return Ok(());
    }

    info!("Verifying checksum for bottle: {}", bottle_path.display());

    let mut file = File::open(bottle_path).map_err(|e| SapphireError::Io(e))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192]; // Read in chunks

    loop {
        let bytes_read = file.read(&mut buffer).map_err(|e| SapphireError::Io(e))?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let hash_result = hasher.finalize();
    let actual_sha256 = hex::encode(hash_result);

    debug!("Expected SHA256: {}", expected_sha256);
    debug!("Actual SHA256:   {}", actual_sha256);

    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        error!("Checksum mismatch for {}. Expected: {}, Got: {}", bottle_path.display(), expected_sha256, actual_sha256);
        // Remove the bad file? Decide on policy. Let's remove it.
        if let Err(e) = fs::remove_file(bottle_path) {
             warn!("Failed to remove corrupted bottle file {}: {}", bottle_path.display(), e);
        }
        // Consider a more specific ChecksumMismatch error variant
        return Err(SapphireError::Generic(format!(
            "Checksum mismatch for bottle {}. Expected: {}, got: {}",
            bottle_path.display(), expected_sha256, actual_sha256
        )));
    }
    info!("Bottle checksum verified successfully.");
    Ok(())
}


/// Install a bottle by extracting it to the Cellar. (Keep existing implementation)
pub fn install_bottle(bottle_path: &Path, formula: &Formula) -> Result<PathBuf> {
    // Get the installation directory (using the formula's specific version)
    let install_dir = super::get_formula_cellar_path(formula); // Assumes this uses Formula struct

    // Create the target directory if it doesn't exist
    // Use fs::create_dir_all to create parent directories if necessary
    if let Some(parent_dir) = install_dir.parent() {
        fs::create_dir_all(parent_dir).map_err(|e| SapphireError::Io(e))?;
    } else {
        // Handle the case where install_dir has no parent (e.g., installing to "/")
        // This shouldn't happen for standard cellar paths, but handle defensively.
        warn!("Could not determine parent directory for install path: {}", install_dir.display());
    }
     // Create the final versioned directory itself too
     fs::create_dir_all(&install_dir).map_err(|e| SapphireError::Io(e))?;

    println!("==> Installing {} from bottle", formula.name);
    println!("==> Pouring {} into {}", bottle_path.file_name().unwrap_or_default().to_string_lossy(), install_dir.display());

    // Extract the bottle tarball.
    // Homebrew bottles usually contain a top-level directory named after the formula
    // and version (e.g., `wget/1.2.3/`). We want to extract the *contents* of that
    // directory into our target `install_dir`. Use strip_components=1.
    // Ensure the extract function is available (it's in build/mod.rs)
    crate::build::extract_archive_strip_components(bottle_path, &install_dir, 1)?;

    // Write the receipt
    // Ensure write_receipt is available (it's in build/mod.rs or build/formula/mod.rs)
    crate::build::write_receipt(formula, &install_dir)?;

    info!("Bottle installation complete for {} at {}", formula.name, install_dir.display());
    Ok(install_dir)
}