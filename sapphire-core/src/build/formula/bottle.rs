// src/build/formula/bottle.rs
// Contains logic for downloading and handling bottle files

use crate::utils::error::{SapphireError, Result};
use crate::model::formula::{Formula, BottleFileSpec};
use crate::utils::config::Config;
use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::copy;
use reqwest::Client;
use std::io::Cursor;
use log; // Import log crate

/// Download a bottle for the given formula
pub async fn download_bottle(formula: &Formula, config: &Config) -> Result<PathBuf> {
    // Get the bottle URL for the current platform
    let (platform, bottle_file) = get_bottle_for_platform(formula)?;

    // Adjust URL handling since BottleFileSpec.url is a String
    if bottle_file.url.is_empty() {
        // Return Generic error if no URL is defined in the spec
        return Err(SapphireError::Generic(format!("No URL defined in bottle spec for {} on platform {}", formula.name, platform)));
    }
    let url = &bottle_file.url;

    println!("==> Downloading bottle for {} ({}) from {}", formula.name, platform, url);
    log::info!("Attempting bottle download for {} from {}", formula.name, url);

    // Create cache directory if it doesn't exist
    let cache_dir = PathBuf::from(&config.cache_dir).join("bottles");
    fs::create_dir_all(&cache_dir)?;

    // Generate a filename for the bottle
    let filename = format!("{}-{}.{}.bottle.tar.gz",
        formula.name,
        formula.version_str_full(), // Use full version string
        platform);

    let bottle_path = cache_dir.join(&filename);

    // Skip download if the file already exists (Optional: Add SHA check here)
    if bottle_path.exists() {
        println!("Using cached bottle: {}", bottle_path.display());
        // Optionally verify checksum here before returning Ok
        // if verify_bottle_checksum(&bottle_path, &bottle_file.sha256).is_ok() {
             return Ok(bottle_path);
        // } else {
        //     log::warn!("Cached bottle {} checksum mismatch. Redownloading.", bottle_path.display());
        //     fs::remove_file(&bottle_path)?; // Remove corrupted cache
        // }
    }

    // Download the bottle
    let client = Client::new();
    let response = client.get(url)
        .send()
        .await
        .map_err(|e| SapphireError::Http(e))?; // Keep as Http error initially

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_else(|_| "Failed to read body".to_string());
        log::error!("Bottle download failed for {}: HTTP Status {} - {}", formula.name, status, body);
        // *** Return the specific DownloadError variant ***
        return Err(SapphireError::DownloadError(
            formula.name.clone(), // Use formula name as identifier
            url.to_string(),
            format!("HTTP status {} - {}", status, body)
        ));
    }

    let content = response.bytes()
        .await
        .map_err(|e| SapphireError::Http(e))?; // Keep as Http error

    // Write the bottle to disk
    let mut file = File::create(&bottle_path)?;
    let mut content_cursor = Cursor::new(content);
    copy(&mut content_cursor, &mut file)?;

    println!("Downloaded bottle to {}", bottle_path.display());

    // Optionally verify checksum after download
    // verify_bottle_checksum(&bottle_path, &bottle_file.sha256)?;

    Ok(bottle_path)
}

/// Find the bottle information for the current platform
fn get_bottle_for_platform(formula: &Formula) -> Result<(String, &BottleFileSpec)> {
    if let Some(stable) = &formula.bottle.stable {
        let current_platform = super::get_current_platform(); // Get platform identifier string
        log::debug!("Looking for bottle for platform: {}", current_platform);
        log::debug!("Available bottle platforms: {:?}", stable.files.keys());

        // Prefer exact match
        if let Some(bottle_file) = stable.files.get(&current_platform) {
             log::debug!("Found exact bottle match for platform: {}", current_platform);
             return Ok((current_platform, bottle_file));
        }

        // Fallback for ARM Macs (e.g., arm64_sonoma might use sonoma bottle if arm64 specific isn't present)
        if current_platform.starts_with("arm64_") {
            let os_name = current_platform.trim_start_matches("arm64_");
            if let Some(bottle_file) = stable.files.get(os_name) {
                log::warn!("No specific arm64 bottle found for {}. Falling back to {} bottle.", current_platform, os_name);
                return Ok((os_name.to_string(), bottle_file));
            }
        }

        // If no direct or fallback match found, take the first available one as a last resort?
        // This is less ideal as it might be incompatible. Homebrew might error here.
        // Let's return an error instead of guessing.
        // if let Some((platform, bottle_file)) = stable.files.iter().next() {
        //    log::warn!("No direct match for {}. Using first available bottle: {}", current_platform, platform);
        //    return Ok((platform.clone(), bottle_file));
        // }
    }
    Err(SapphireError::Generic(format!("No compatible bottle found for formula {} on platform {}", formula.name, super::get_current_platform())))
}


/// Verify the SHA256 checksum of a downloaded bottle
pub fn verify_bottle_checksum(bottle_path: &Path, expected_sha256: &str) -> Result<()> {
    use sha2::{Sha256, Digest};
    log::info!("Verifying checksum for bottle: {}", bottle_path.display());

    let mut file = File::open(bottle_path)?;
    let mut hasher = Sha256::new();

    std::io::copy(&mut file, &mut hasher)?;

    let hash_result = hasher.finalize();
    let actual_sha256 = format!("{:x}", hash_result);

    log::debug!("Expected SHA256: {}", expected_sha256);
    log::debug!("Actual SHA256:   {}", actual_sha256);

    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        log::error!("Checksum mismatch for {}. Expected: {}, Got: {}", bottle_path.display(), expected_sha256, actual_sha256);
        // Remove the bad file
        if let Err(e) = fs::remove_file(bottle_path) {
             log::warn!("Failed to remove corrupted bottle file {}: {}", bottle_path.display(), e);
        }
        return Err(SapphireError::Generic(format!( // Consider a ChecksumMismatch error variant
            "Checksum mismatch for bottle {}. Expected: {}, got: {}",
            bottle_path.display(), expected_sha256, actual_sha256
        )));
    }
    log::info!("Bottle checksum verified successfully.");
    Ok(())
}

/// Install a bottle by extracting it to the Cellar
pub fn install_bottle(bottle_path: &Path, formula: &Formula) -> Result<PathBuf> {
    // Get the installation directory
    let install_dir = super::get_formula_cellar_path(formula);

    // Create the directory if it doesn't exist
    fs::create_dir_all(&install_dir)?;

    println!("==> Installing {} from bottle", formula.name);
    println!("==> Pouring {} into {}", bottle_path.file_name().unwrap().to_string_lossy(), install_dir.display());

    // Extract the bottle (using the correct function from parent mod)
    // Assumes the bottle tarball structure is like: formula_name/version/files...
    // We need to strip the top two components (formula_name/version)
    // Let's use the non-stripping extract for now and adjust if needed based on bottle structure
    super::extract_archive(bottle_path, &install_dir)?;

    // Write the receipt
    super::write_receipt(formula, &install_dir)?;

    Ok(install_dir)
}