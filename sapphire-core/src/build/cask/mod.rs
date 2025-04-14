// src/build/cask/mod.rs
// Main module for cask installation functionality

pub mod app;
pub mod dmg;
pub mod pkg;
pub mod zip;

use crate::model::cask::Cask;
use crate::utils::cache::Cache;
use crate::utils::error::{Result, SapphireError};
use reqwest::Url;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

/// Get the Applications directory
pub fn get_applications_dir() -> PathBuf {
    // On macOS, applications are installed in /Applications
    PathBuf::from("/Applications")
}

/// Get the Caskroom directory
pub fn get_caskroom_dir() -> PathBuf {
    // On macOS, Homebrew Caskroom is typically at /opt/homebrew/Caskroom
    // or /usr/local/Caskroom
    if Path::new("/opt/homebrew/Caskroom").exists() {
        PathBuf::from("/opt/homebrew/Caskroom")
    } else {
        PathBuf::from("/usr/local/Caskroom")
    }
}

/// Get installation path for a cask
pub fn get_cask_path(cask: &Cask) -> PathBuf {
    let caskroom = get_caskroom_dir();
    let version = cask.version.clone().unwrap_or_else(|| "latest".to_string());

    caskroom.join(&cask.token).join(&version)
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

    // Check if the file is already in the cache
    if cache_path.exists() {
        // Optionally, add TTL check here using cache.is_cache_valid if needed
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

    // Ensure cache directory exists before writing
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = fs::File::create(&cache_path)?;
    file.write_all(&bytes)?;

    println!("==> Download completed: {}", cache_path.display());

    Ok(cache_path)
}

/// Install a cask from a downloaded file
pub fn install_cask(cask: &Cask, download_path: &Path) -> Result<()> {
    println!("==> Installing cask: {}", cask.token);

    // Create the caskroom directory for this cask
    let caskroom_path = get_cask_path(cask);
    if !caskroom_path.exists() {
        fs::create_dir_all(&caskroom_path)?;
    }

    // Determine the file type based on extension
    let extension = download_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();

    let result = match extension.as_str() {
        "dmg" => dmg::install_from_dmg(cask, download_path, &caskroom_path),
        "pkg" | "mpkg" => pkg::install_pkg_from_path(cask, download_path, &caskroom_path),
        "zip" => zip::install_from_zip(cask, download_path, &caskroom_path),
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

/// Check if a cask has app artifacts
pub fn has_app_artifact(cask: &Cask) -> bool {
    // A cask has app artifacts if it has URLs ending with .app
    // or explicitly defines app artifacts in its definition
    if let Some(ref name) = cask.name {
        if !name.is_empty() {
            return true;
        }
    }

    // Check URLs for app-like extensions
    if let Some(ref urls) = cask.url {
        for url in urls {
            if url.ends_with(".app") || url.contains(".app/") {
                return true;
            }
        }
    }

    // For simplicity, assume most casks have app artifacts unless
    // they're clearly something else (like a pkg)
    if let Some(ref urls) = cask.url {
        for url in urls {
            if url.ends_with(".pkg") || url.ends_with(".mpkg") {
                return false;
            }
        }
    }

    true
}

/// Write a receipt file for the cask installation
pub fn write_receipt(cask: &Cask, caskroom_path: &Path, artifacts: Vec<String>) -> Result<()> {
    let receipt_path = caskroom_path.join("receipt.json");

    // Create receipt data
    // Map SystemTimeError explicitly
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e: SystemTimeError| SapphireError::Generic(format!("System time error: {}", e)))?
        .as_secs();

    let receipt_data = json!({
        "token": cask.token,
        "version": cask.version.as_ref().unwrap_or(&"latest".to_string()),
        "installed_at": timestamp,
        "artifacts": artifacts
    });

    let receipt_file = fs::File::create(receipt_path)?;
    serde_json::to_writer_pretty(receipt_file, &receipt_data)?;

    Ok(())
}
