use std::fs::File;
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};
use url::Url;
use {hex, infer};

use crate::utils::error::{Result, SpError};

pub fn verify_checksum(path: &Path, expected: &str) -> Result<()> {
    tracing::debug!("Verifying checksum for: {}", path.display());
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let bytes_copied = io::copy(&mut file, &mut hasher)?;
    let hash_bytes = hasher.finalize();
    let actual = hex::encode(hash_bytes);
    tracing::debug!(
        "Calculated SHA256: {} ({} bytes read)",
        actual,
        bytes_copied
    );
    tracing::debug!("Expected SHA256:   {}", expected);
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(SpError::ChecksumError(format!(
            "Checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            actual
        )))
    }
}

/// Verifies that the detected content type of the file matches the expected extension.
pub fn verify_content_type(path: &Path, expected_ext: &str) -> Result<()> {
    let kind_opt = infer::get_from_path(path)?;
    if let Some(kind) = kind_opt {
        let actual_ext = kind.extension();
        if actual_ext.eq_ignore_ascii_case(expected_ext) {
            tracing::debug!(
                "Content type verified: {} matches expected {}",
                actual_ext,
                expected_ext
            );
            Ok(())
        } else {
            Err(SpError::Generic(format!(
                "Content type mismatch for {}: expected extension '{}', but detected '{}'",
                path.display(),
                expected_ext,
                actual_ext
            )))
        }
    } else {
        Err(SpError::Generic(format!(
            "Could not determine content type for {}",
            path.display()
        )))
    }
}

/// Validates a URL, ensuring it uses the HTTPS scheme.
pub fn validate_url(url_str: &str) -> Result<()> {
    let url = Url::parse(url_str)
        .map_err(|e| SpError::Generic(format!("Failed to parse URL '{url_str}': {e}")))?;
    if url.scheme() == "https" {
        Ok(())
    } else {
        Err(SpError::ValidationError(format!(
            "Invalid URL scheme for '{}': Must be https, but got '{}'",
            url_str,
            url.scheme()
        )))
    }
}
