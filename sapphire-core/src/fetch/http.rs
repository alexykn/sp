// sapphire-core/src/fetch/http.rs
// *** No changes from previous correction - kept async ***

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{HeaderMap, ACCEPT, USER_AGENT};
use reqwest::{Client, StatusCode}; // Use async Client
use sha2::{Digest, Sha256};
use tokio::fs::File as TokioFile; // Use tokio's async File
use tokio::io::AsyncWriteExt;
use tracing::{debug, error};

use crate::model::formula::ResourceSpec;
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError}; // For async write operations

const DOWNLOAD_TIMEOUT_SECS: u64 = 300;
const CONNECT_TIMEOUT_SECS: u64 = 30;
const USER_AGENT_STRING: &str =
    "Sapphire Package Manager (Rust; +https://github.com/your/sapphire)";

/// Fetches a formula's primary source or bottle asynchronously.
pub async fn fetch_formula_source_or_bottle(
    formula_name: &str,
    url: &str,
    sha256_expected: &str,
    mirrors: &[String],
    config: &Config,
) -> Result<PathBuf> {
    let filename = url
        .split('/')
        .next_back() // Use next_back()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{formula_name}-download"));
    let cache_path = config.cache_dir.join(&filename);

    tracing::debug!(
        "Preparing to fetch main resource for '{}' from URL: {}",
        formula_name,
        url
    );
    tracing::debug!("Target cache path: {}", cache_path.display());
    tracing::debug!("Expected SHA256: {}", sha256_expected);

    // Check cache first (blocking IO is okay for quick checks)
    if cache_path.is_file() {
        tracing::debug!("File exists in cache: {}", cache_path.display());
        if !sha256_expected.is_empty() {
            match verify_checksum(&cache_path, sha256_expected) {
                // Checksum verification is sync
                Ok(_) => {
                    tracing::debug!("Using valid cached file: {}", cache_path.display());
                    return Ok(cache_path);
                }
                Err(e) => {
                    debug!(
                        "Cached file checksum mismatch ({}): {}. Redownloading.",
                        cache_path.display(),
                        e
                    );
                    if let Err(remove_err) = fs::remove_file(&cache_path) {
                        debug!(
                            "Failed to remove corrupted cached file {}: {}",
                            cache_path.display(),
                            remove_err
                        );
                    }
                }
            }
        } else {
            tracing::debug!(
                "Using cached file (no checksum provided): {}",
                cache_path.display()
            );
            return Ok(cache_path);
        }
    } else {
        tracing::debug!("File not found in cache.");
    }

    // Create cache dir (sync is fine)
    fs::create_dir_all(&config.cache_dir).map_err(|e| {
        SapphireError::IoError(format!(
            "Failed to create cache directory {}: {}",
            config.cache_dir.display(),
            e
        ))
    })?;

    let client = build_http_client()?; // Builds async client

    let urls_to_try = std::iter::once(url).chain(mirrors.iter().map(|s| s.as_str()));
    let mut last_error: Option<SapphireError> = None;

    for current_url in urls_to_try {
        tracing::debug!("Attempting download from: {}", current_url);
        match download_and_verify(&client, current_url, &cache_path, sha256_expected).await {
            // Await async download
            Ok(path) => {
                tracing::debug!("Successfully downloaded and verified: {}", path.display());
                return Ok(path);
            }
            Err(e) => {
                error!("Download attempt failed from {}: {}", current_url, e);
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

/// Fetches a formula's resource dependency asynchronously.
pub async fn fetch_resource(
    formula_name: &str,
    resource: &ResourceSpec,
    config: &Config,
) -> Result<PathBuf> {
    let resource_cache_dir = config.cache_dir.join("resources");
    fs::create_dir_all(&resource_cache_dir).map_err(|e| {
        SapphireError::IoError(format!(
            "Failed to create resource cache directory {}: {}",
            resource_cache_dir.display(),
            e
        ))
    })?;

    let url_filename = resource
        .url
        .split('/')
        .next_back() // Use next_back()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}-download", resource.name));
    let cache_filename = format!("{}-{}", resource.name, url_filename);
    let cache_path = resource_cache_dir.join(&cache_filename);

    tracing::debug!(
        "Preparing to fetch resource '{}' for formula '{}' from URL: {}",
        resource.name,
        formula_name,
        resource.url
    );
    tracing::debug!("Target resource cache path: {}", cache_path.display());
    tracing::debug!("Expected SHA256: {}", resource.sha256);

    // Check resource cache (sync is fine)
    if cache_path.is_file() {
        tracing::debug!("Resource exists in cache: {}", cache_path.display());
        match verify_checksum(&cache_path, &resource.sha256) {
            // Checksum is sync
            Ok(_) => {
                tracing::debug!("Using cached resource: {}", cache_path.display());
                return Ok(cache_path);
            }
            Err(e) => {
                debug!(
                    "Cached resource checksum mismatch ({}): {}. Redownloading.",
                    cache_path.display(),
                    e
                );
                if let Err(remove_err) = fs::remove_file(&cache_path) {
                    debug!(
                        "Failed to remove corrupted cached resource file {}: {}",
                        cache_path.display(),
                        remove_err
                    );
                }
            }
        }
    } else {
        tracing::debug!("Resource not found in cache.");
    }

    let client = build_http_client()?;
    match download_and_verify(&client, &resource.url, &cache_path, &resource.sha256).await {
        // Await async download
        Ok(path) => {
            tracing::debug!(
                "Successfully downloaded and verified resource: {}",
                path.display()
            );
            Ok(path)
        }
        Err(e) => {
            error!("Resource download failed from {}: {}", resource.url, e);
            let _ = fs::remove_file(&cache_path); // Attempt cleanup
            Err(SapphireError::DownloadError(
                resource.name.clone(),
                resource.url.clone(),
                format!("Download failed: {e}"),
            ))
        }
    }
}

// --- Internal Helpers ---

// Builds the async reqwest::Client
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
        .map_err(|e| SapphireError::HttpError(format!("Failed to build HTTP client: {e}")))
}

// Performs download and verification asynchronously
async fn download_and_verify(
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
    tracing::debug!("Downloading to temporary path: {}", temp_path.display());
    if temp_path.exists() {
        if let Err(e) = fs::remove_file(&temp_path) {
            tracing::warn!(
                "Could not remove existing temporary file {}: {}",
                temp_path.display(),
                e
            );
        }
    }

    let response = client
        .get(url)
        .send()
        .await // Await send
        .map_err(|e| SapphireError::HttpError(format!("HTTP request failed for {url}: {e}")))?;
    let status = response.status();
    tracing::debug!("Received HTTP status: {} for {}", status, url);

    if !status.is_success() {
        let body_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Failed to read response body".to_string()); // Await text
        tracing::error!("HTTP error {} for URL {}: {}", status, url, body_text);
        return match status {
            StatusCode::NOT_FOUND => Err(SapphireError::DownloadError(
                final_path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                url.to_string(),
                "Resource not found (404)".to_string(),
            )),
            StatusCode::FORBIDDEN => Err(SapphireError::DownloadError(
                final_path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                url.to_string(),
                "Access forbidden (403)".to_string(),
            )),
            _ => Err(SapphireError::HttpError(format!(
                "HTTP error {status} for URL {url}: {body_text}"
            ))),
        };
    }

    // Use tokio async file operations
    let mut temp_file = TokioFile::create(&temp_path).await.map_err(|e| {
        SapphireError::IoError(format!(
            "Failed to create temp file {}: {}",
            temp_path.display(),
            e
        ))
    })?;
    let content = response
        .bytes()
        .await // Await bytes
        .map_err(|e| {
            SapphireError::HttpError(format!("Failed to read response body bytes: {e}"))
        })?;
    temp_file
        .write_all(&content)
        .await // Await write
        .map_err(|e| {
            SapphireError::IoError(format!(
                "Failed to write download stream to {}: {}",
                temp_path.display(),
                e
            ))
        })?;
    drop(temp_file); // Close file
    tracing::debug!("Finished writing download stream to temp file.");

    // Checksum verification is synchronous (CPU bound)
    if !sha256_expected.is_empty() {
        verify_checksum(&temp_path, sha256_expected)?;
        tracing::debug!(
            "Checksum verified for temporary file: {}",
            temp_path.display()
        );
    } else {
        tracing::warn!(
            "Skipping checksum verification for {} - none provided.",
            temp_path.display()
        );
    }

    // Rename is synchronous
    fs::rename(&temp_path, final_path).map_err(|e| {
        SapphireError::IoError(format!(
            "Failed to move temp file {} to {}: {}",
            temp_path.display(),
            final_path.display(),
            e
        ))
    })?;
    tracing::debug!(
        "Moved verified file to final location: {}",
        final_path.display()
    );
    Ok(final_path.to_path_buf())
}

// verify_checksum remains synchronous
pub fn verify_checksum(file_path: &Path, expected_sha256: &str) -> Result<()> {
    tracing::debug!("Verifying checksum for: {}", file_path.display());
    let mut file = match fs::File::open(file_path) {
        Ok(f) => f,
        Err(e) => {
            return Err(SapphireError::IoError(format!(
                "Failed to open file for checksum {}: {}",
                file_path.display(),
                e
            )))
        }
    };
    let mut hasher = Sha256::new();
    let bytes_copied = match std::io::copy(&mut file, &mut hasher) {
        Ok(b) => b,
        Err(e) => {
            return Err(SapphireError::IoError(format!(
                "Failed read file for checksum {}: {}",
                file_path.display(),
                e
            )))
        }
    };
    let hash_bytes = hasher.finalize();
    let actual_sha256 = hex::encode(hash_bytes);
    tracing::debug!(
        "Calculated SHA256: {} ({} bytes read)",
        actual_sha256,
        bytes_copied
    );
    tracing::debug!("Expected SHA256:   {}", expected_sha256);
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
