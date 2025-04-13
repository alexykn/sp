use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, ACCEPT, USER_AGENT};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DOWNLOAD_TIMEOUT_SECS: u64 = 300;
const CONNECT_TIMEOUT_SECS: u64 = 30;
const USER_AGENT_STRING: &str = "Sapphire Package Manager (Rust; +https://github.com/your/sapphire)";

/// Fetches a resource (source tarball or bottle) for a given formula.
/// Verifies the SHA256 checksum after download.
/// Downloads to the cache directory specified in the Config.
/// Returns the path to the downloaded file in the cache.
pub fn fetch_resource(
    formula_name: &str,
    url: &str,
    sha256_expected: &str,
    mirrors: &[String],
    config: &Config,
) -> Result<PathBuf> {
    let filename = url.split('/').last()
        .map(|s| s.to_string()) // Convert Option<&str> to Option<String>
        .unwrap_or_else(|| format!("{}-download", formula_name)); // Provide String fallback
    let cache_path = config.cache_dir.join(&filename); // Join with &String

    log::debug!(
        "Preparing to fetch resource for '{}' from URL: {}",
        formula_name, url
    );
    log::debug!("Target cache path: {}", cache_path.display());
    log::debug!("Expected SHA256: {}", sha256_expected);

    // Check cache first
    if cache_path.is_file() {
        log::debug!("File exists in cache: {}", cache_path.display());
        match verify_checksum(&cache_path, sha256_expected) {
            Ok(_) => {
                log::info!("Using cached file: {}", cache_path.display());
                return Ok(cache_path);
            }
            Err(e) => {
                log::warn!(
                    "Cached file checksum mismatch ({}): {}. Redownloading.",
                    cache_path.display(), e
                );
                if let Err(remove_err) = fs::remove_file(&cache_path) {
                    log::warn!("Failed to remove corrupted cached file {}: {}", cache_path.display(), remove_err);
                }
            }
        }
    } else {
        log::debug!("File not found in cache.");
    }

    fs::create_dir_all(&config.cache_dir).map_err(|e| {
        SapphireError::IoError(format!(
            "Failed to create cache directory {}: {}",
            config.cache_dir.display(), e
        ))
    })?;

    let client = build_http_client()?;

    let urls_to_try = std::iter::once(url).chain(mirrors.iter().map(|s| s.as_str()));

    let mut last_error: Option<SapphireError> = None;

    for current_url in urls_to_try {
        log::debug!("Attempting download from: {}", current_url);
        match download_and_verify(&client, current_url, &cache_path, sha256_expected) {
            Ok(path) => {
                log::info!("Successfully downloaded and verified: {}", path.display());
                return Ok(path);
            }
            Err(e) => {
                log::error!("Download attempt failed from {}: {}", current_url, e);
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        SapphireError::DownloadError(
            formula_name.to_string(),
            url.to_string(),
            "All download attempts failed.".to_string(),
        )
    }))
}

fn build_http_client() -> Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, USER_AGENT_STRING.parse().unwrap());
    headers.insert(ACCEPT, "*/*".parse().unwrap());

    Client::builder()
        .timeout(Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| SapphireError::HttpError(format!("Failed to build HTTP client: {}", e)))
}

fn download_and_verify(
    client: &Client,
    url: &str,
    final_path: &Path,
    sha256_expected: &str,
) -> Result<PathBuf> {
    let temp_filename = format!(
        ".{}.download",
        final_path.file_name().unwrap_or_default().to_string_lossy()
    );
    let temp_path = final_path.with_file_name(temp_filename);

    log::debug!("Downloading to temporary path: {}", temp_path.display());

    if temp_path.exists() {
        if let Err(e) = fs::remove_file(&temp_path) {
            log::warn!("Could not remove existing temporary file {}: {}", temp_path.display(), e);
        }
    }

    let mut response = client.get(url)
        .send()
        .map_err(|e| SapphireError::HttpError(format!("HTTP request failed for {}: {}", url, e)))?;

    let status = response.status();
    log::debug!("Received HTTP status: {} for {}", status, url);

    if !status.is_success() {
        let body_text = response.text().unwrap_or_else(|_| "Failed to read response body".to_string());
        log::error!("HTTP error {} for URL {}: {}", status, url, body_text);
        return match status {
            StatusCode::NOT_FOUND => Err(SapphireError::DownloadError(
                final_path.file_name().map(|s|s.to_string_lossy().to_string()).unwrap_or_default(),
                url.to_string(),
                "Resource not found (404)".to_string(),
            )),
            StatusCode::FORBIDDEN => Err(SapphireError::DownloadError(
                final_path.file_name().map(|s|s.to_string_lossy().to_string()).unwrap_or_default(),
                url.to_string(),
                "Access forbidden (403) - Check User-Agent or URL validity".to_string(),
            )),
            _ => Err(SapphireError::HttpError(format!(
                "HTTP error {} for URL {}: {}", status, url, body_text
            ))),
        };
    }

    let mut temp_file = File::create(&temp_path)
        .map_err(|e| SapphireError::IoError(format!("Failed to create temp file {}: {}", temp_path.display(), e)))?;

    response.copy_to(&mut temp_file)
        .map_err(|e| SapphireError::IoError(format!("Failed to write download stream to {}: {}", temp_path.display(), e)))?;

    log::debug!("Finished writing download stream to temp file.");
    drop(temp_file);

    verify_checksum(&temp_path, sha256_expected)?;
    log::debug!("Checksum verified for temporary file: {}", temp_path.display());

    fs::rename(&temp_path, final_path)
        .map_err(|e| SapphireError::IoError(format!("Failed to move temp file {} to {}: {}", temp_path.display(), final_path.display(), e)))?;

    log::debug!("Moved verified file to final location: {}", final_path.display());

    Ok(final_path.to_path_buf())
}

fn verify_checksum(file_path: &Path, expected_sha256: &str) -> Result<()> {
    log::debug!("Verifying checksum for: {}", file_path.display());
    let mut file = File::open(file_path)
        .map_err(|e| SapphireError::IoError(format!("Failed to open file for checksum {}: {}", file_path.display(), e)))?;

    let mut hasher = Sha256::new();
    let bytes_copied = std::io::copy(&mut file, &mut hasher)
        .map_err(|e| SapphireError::IoError(format!("Failed read file for checksum {}: {}", file_path.display(), e)))?;

    let hash_bytes = hasher.finalize();
    let actual_sha256 = hex::encode(hash_bytes);

    log::debug!("Calculated SHA256: {} ({} bytes read)", actual_sha256, bytes_copied);
    log::debug!("Expected SHA256:   {}", expected_sha256);

    if actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        Ok(())
    } else {
        Err(SapphireError::ChecksumError(format!(
            "Checksum mismatch for {}: expected {}, got {}",
            file_path.display(),
            expected_sha256,
            actual_sha256
        )))
    }
}

// --- Logging Macros ---
#[allow(unused_macros)]
macro_rules! debug {
    ($($arg:tt)*) => { eprintln!("DEBUG [fetch]: {}", format!($($arg)*)); };
}
#[allow(unused_macros)]
macro_rules! info {
    ($($arg:tt)*) => { eprintln!("INFO [fetch]: {}", format!($($arg)*)); };
}
#[allow(unused_macros)]
macro_rules! warn {
    ($($arg:tt)*) => { eprintln!("WARN [fetch]: {}", format!($($arg)*)); };
}
#[allow(unused_macros)]
macro_rules! error {
    ($($arg:tt)*) => { eprintln!("ERROR [fetch]: {}", format!($($arg)*)); };
}
