// ===== sapphire-core/src/build/cask/mod.rs =====
// Main module for cask installation functionality

pub mod app;
pub mod dmg;
pub mod pkg;
pub mod zip;

use crate::model::cask::Cask;
use crate::utils::cache::Cache;
use crate::utils::config::Config; // Import Config
use crate::utils::error::{Result, SapphireError};
use reqwest::Url;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

// REMOVED: get_applications_dir (now in Config)
// REMOVED: get_caskroom_dir (now in Config)

/// Get installation path for a cask's specific version
pub fn get_cask_version_path(cask: &Cask, config: &Config) -> PathBuf {
    let version = cask.version.clone().unwrap_or_else(|| "latest".to_string());
    // Use Config method
    config.cask_version_path(&cask.token, &version)
}

/// Download a cask
pub async fn download_cask(cask: &Cask, cache: &Cache) -> Result<PathBuf> {
    let urls = cask
        .url
        .as_ref()
        .ok_or_else(|| SapphireError::Generic(format!("Cask {} has no URL", cask.token)))?;

    if urls.is_empty() {
        return Err(SapphireError::Generic(format!(
            "Cask {} has empty URL list",
            cask.token
        )));
    }

    let url_str = &urls[0];
    println!("==> Downloading cask from {}", url_str);

    let url = Url::parse(url_str)
        .map_err(|e| SapphireError::Generic(format!("Invalid URL '{}': {}", url_str, e)))?;

    let file_name = url
        .path_segments()
        .and_then(|segments| segments.last())
        .unwrap_or("download.tmp");

    // Use cache instance's directory
    let cache_key = format!("cask-{}-{}", cask.token, file_name);
    let cache_path = cache.get_dir().join(&cache_key);

    if cache_path.exists() {
        println!("==> Using cached download at {}", cache_path.display());
        return Ok(cache_path);
    }

    let client = reqwest::Client::new();
    let response = client
        .get(url.clone())
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
        .map_err(|e| SapphireError::Generic(format!("Failed to read download response: {}", e)))?;

    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = fs::File::create(&cache_path)?;
    file.write_all(&bytes)?;

    println!("==> Download completed: {}", cache_path.display());

    Ok(cache_path)
}

/// Install a cask from a downloaded file
// Added Config parameter
pub fn install_cask(cask: &Cask, download_path: &Path, config: &Config) -> Result<()> {
    println!("==> Installing cask: {}", cask.token);

    // Use the function that takes Config
    let cask_version_install_path = get_cask_version_path(cask, config);
    if !cask_version_install_path.exists() {
        fs::create_dir_all(&cask_version_install_path)?;
    }

    let extension = download_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Pass Config down to specific installers
    let result = match extension.as_str() {
        "dmg" => dmg::install_from_dmg(cask, download_path, &cask_version_install_path, config),
        "pkg" | "mpkg" => pkg::install_pkg_from_path(cask, download_path, &cask_version_install_path, config),
        "zip" => zip::install_from_zip(cask, download_path, &cask_version_install_path, config),
        _ => Err(SapphireError::Generic(format!(
            "Unsupported file type: {}",
            extension
        ))),
    };

    if result.is_ok() {
        println!("==> Successfully installed cask: {}", cask.token);
    }

    result
}


/// Write a receipt file for the cask installation
// Parameter `caskroom_path` renamed to `cask_version_install_path` for clarity
pub fn write_receipt(cask: &Cask, cask_version_install_path: &Path, artifacts: Vec<String>) -> Result<()> {
    let receipt_path = cask_version_install_path.join("INSTALL_RECEIPT.json"); // Use consistent name

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e: SystemTimeError| SapphireError::Generic(format!("System time error: {}", e)))?
        .as_secs();

    let receipt_data = json!({
        "token": cask.token,
        "version": cask.version.as_ref().unwrap_or(&"latest".to_string()),
        "installed_at": timestamp,
        "artifacts": artifacts // Renamed field for consistency with manifest concept
    });

    let receipt_file = fs::File::create(&receipt_path)?;
    serde_json::to_writer_pretty(receipt_file, &receipt_data)?;

    Ok(())
}