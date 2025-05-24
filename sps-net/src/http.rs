use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{HeaderMap, ACCEPT, USER_AGENT};
use reqwest::{Client, StatusCode};
use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_common::model::formula::ResourceSpec;
use tokio::fs::File as TokioFile;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error};

use crate::validation::{validate_url, verify_checksum};

const DOWNLOAD_TIMEOUT_SECS: u64 = 300;
const CONNECT_TIMEOUT_SECS: u64 = 30;
const USER_AGENT_STRING: &str = "sps package manager (Rust; +https://github.com/alexykn/sp)";

pub async fn fetch_formula_source_or_bottle(
    formula_name: &str,
    url: &str,
    sha256_expected: &str,
    mirrors: &[String],
    config: &Config,
) -> Result<PathBuf> {
    let filename = url
        .split('/')
        .next_back()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{formula_name}-download"));
    let cache_path = config.cache_dir().join(&filename);

    tracing::debug!(
        "Preparing to fetch main resource for '{}' from URL: {}",
        formula_name,
        url
    );
    tracing::debug!("Target cache path: {}", cache_path.display());
    tracing::debug!("Expected SHA256: {}", sha256_expected);

    if cache_path.is_file() {
        tracing::debug!("File exists in cache: {}", cache_path.display());
        if !sha256_expected.is_empty() {
            match verify_checksum(&cache_path, sha256_expected) {
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

    fs::create_dir_all(config.cache_dir()).map_err(|e| {
        SpsError::IoError(format!(
            "Failed to create cache directory {}: {}",
            config.cache_dir().display(),
            e
        ))
    })?;
    // Validate primary URL
    validate_url(url)?;

    let client = build_http_client()?;

    let urls_to_try = std::iter::once(url).chain(mirrors.iter().map(|s| s.as_str()));
    let mut last_error: Option<SpsError> = None;

    for current_url in urls_to_try {
        // Validate mirror URL
        validate_url(current_url)?;
        tracing::debug!("Attempting download from: {}", current_url);
        match download_and_verify(&client, current_url, &cache_path, sha256_expected).await {
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
        SpsError::DownloadError(
            formula_name.to_string(),
            url.to_string(),
            "All download attempts failed.".to_string(),
        )
    }))
}

pub async fn fetch_resource(
    formula_name: &str,
    resource: &ResourceSpec,
    config: &Config,
) -> Result<PathBuf> {
    let resource_cache_dir = config.cache_dir().join("resources");
    fs::create_dir_all(&resource_cache_dir).map_err(|e| {
        SpsError::IoError(format!(
            "Failed to create resource cache directory {}: {}",
            resource_cache_dir.display(),
            e
        ))
    })?;
    // Validate resource URL
    validate_url(&resource.url)?;

    let url_filename = resource
        .url
        .split('/')
        .next_back()
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

    if cache_path.is_file() {
        tracing::debug!("Resource exists in cache: {}", cache_path.display());
        match verify_checksum(&cache_path, &resource.sha256) {
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
        Ok(path) => {
            tracing::debug!(
                "Successfully downloaded and verified resource: {}",
                path.display()
            );
            Ok(path)
        }
        Err(e) => {
            error!("Resource download failed from {}: {}", resource.url, e);
            let _ = fs::remove_file(&cache_path);
            Err(SpsError::DownloadError(
                resource.name.clone(),
                resource.url.clone(),
                format!("Download failed: {e}"),
            ))
        }
    }
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
        .map_err(|e| SpsError::HttpError(format!("Failed to build HTTP client: {e}")))
}

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

    let response = client.get(url).send().await.map_err(|e| {
        debug!("HTTP request failed for {url}: {e}");
        SpsError::HttpError(format!("HTTP request failed for {url}: {e}"))
    })?;
    let status = response.status();
    tracing::debug!("Received HTTP status: {} for {}", status, url);

    if !status.is_success() {
        let body_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Failed to read response body".to_string());
        tracing::error!("HTTP error {} for URL {}: {}", status, url, body_text);
        return match status {
            StatusCode::NOT_FOUND => Err(SpsError::DownloadError(
                final_path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                url.to_string(),
                "Resource not found (404)".to_string(),
            )),
            StatusCode::FORBIDDEN => Err(SpsError::DownloadError(
                final_path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                url.to_string(),
                "Access forbidden (403)".to_string(),
            )),
            _ => Err(SpsError::HttpError(format!(
                "HTTP error {status} for URL {url}: {body_text}"
            ))),
        };
    }

    let mut temp_file = TokioFile::create(&temp_path).await.map_err(|e| {
        SpsError::IoError(format!(
            "Failed to create temp file {}: {}",
            temp_path.display(),
            e
        ))
    })?;
    let content = response
        .bytes()
        .await
        .map_err(|e| SpsError::HttpError(format!("Failed to read response body bytes: {e}")))?;
    temp_file.write_all(&content).await.map_err(|e| {
        SpsError::IoError(format!(
            "Failed to write download stream to {}: {}",
            temp_path.display(),
            e
        ))
    })?;
    drop(temp_file);
    tracing::debug!("Finished writing download stream to temp file.");

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

    fs::rename(&temp_path, final_path).map_err(|e| {
        SpsError::IoError(format!(
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
