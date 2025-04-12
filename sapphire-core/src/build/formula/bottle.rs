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

/// Download a bottle for the given formula
pub async fn download_bottle(formula: &Formula, config: &Config) -> Result<PathBuf> {
    // Get the bottle URL for the current platform
    let (platform, bottle_file) = get_bottle_for_platform(formula)?;

    // Adjust URL handling since BottleFileSpec.url is a String
    if bottle_file.url.is_empty() {
        return Err(SapphireError::Generic(format!("No URL for bottle on platform {}", platform)));
    }
    let url = &bottle_file.url;

    println!("==> Downloading bottle for {} ({})", formula.name, platform);

    // Create cache directory if it doesn't exist
    let cache_dir = PathBuf::from(&config.cache_dir).join("bottles");
    fs::create_dir_all(&cache_dir)?;

    // Generate a filename for the bottle
    let filename = format!("{}-{}.{}.bottle.tar.gz",
        formula.name,
        formula.version.to_string(),
        platform);

    let bottle_path = cache_dir.join(&filename);

    // Skip download if the file already exists
    if bottle_path.exists() {
        println!("Using cached bottle: {}", bottle_path.display());
        return Ok(bottle_path);
    }

    // Download the bottle
    let client = Client::new();
    let response = client.get(url)
        .send()
        .await
        .map_err(|e| SapphireError::Http(e))?;

    if !response.status().is_success() {
        return Err(SapphireError::Generic(format!("Failed to download: HTTP status {}", response.status())));
    }

    let content = response.bytes()
        .await
        .map_err(|e| SapphireError::Http(e))?;

    // Write the bottle to disk
    let mut file = File::create(&bottle_path)?;
    let mut content_cursor = Cursor::new(content);
    copy(&mut content_cursor, &mut file)?;

    println!("Downloaded bottle to {}", bottle_path.display());

    Ok(bottle_path)
}

/// Find the bottle information for the current platform
fn get_bottle_for_platform(formula: &Formula) -> Result<(String, &BottleFileSpec)> {
    if let Some(stable) = &formula.bottle.stable {
        // Iterate through all architecture-specific files for the stable bottle spec
        for (platform, bottle_file) in &stable.files {
            if platform == &super::get_current_platform() {
                return Ok((platform.clone(), bottle_file));
            }
            if std::env::consts::ARCH == "aarch64" && !platform.contains("arm64") {
                return Ok((platform.clone(), bottle_file));
            }
        }
        // If no direct match found, take the first available architecture
        if let Some((platform, bottle_file)) = stable.files.iter().next() {
            return Ok((platform.clone(), bottle_file));
        }
    }
    Err(SapphireError::Generic("No compatible bottle found".to_string()))
}

/// Verify the SHA256 checksum of a downloaded bottle
pub fn verify_bottle_checksum(bottle_path: &Path, expected_sha256: &str) -> Result<()> {
    use sha2::{Sha256, Digest};

    let mut file = File::open(bottle_path)?;
    let mut hasher = Sha256::new();

    std::io::copy(&mut file, &mut hasher)?;

    let hash_result = hasher.finalize();
    let hash_hex = format!("{:x}", hash_result);

    if hash_hex != expected_sha256 {
        return Err(SapphireError::Generic(format!(
            "Checksum mismatch for bottle. Expected: {}, got: {}",
            expected_sha256, hash_hex
        )));
    }

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

    // Extract the bottle
    super::extract_archive(bottle_path, &install_dir)?;

    // Write the receipt
    super::write_receipt(formula, &install_dir)?;

    Ok(install_dir)
}
