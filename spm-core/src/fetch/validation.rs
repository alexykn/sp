use std::fs::File;
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};
use url::Url;
use {hex, infer};

use crate::utils::error::{Result, SpmError};

pub fn verify_checksum(path: &Path, expected: &str) -> Result<()> {
    tracing::debug!("Verifying checksum for: {}", path.display());
    let mut file = File::open(path).map_err(|e| {
        SpmError::Io(io::Error::new(
            e.kind(),
            format!("Failed to open file for checksum {}: {}", path.display(), e),
        ))
    })?;

    let mut hasher = Sha256::new();
    let bytes_copied = io::copy(&mut file, &mut hasher).map_err(|e| {
        SpmError::Io(io::Error::new(
            e.kind(),
            format!("Failed read file for checksum {}: {}", path.display(), e),
        ))
    })?;

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
        Err(SpmError::ChecksumError(format!(
            "Checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            actual
        )))
    }
}

/// Verifies that the detected content type of the file matches the expected extension.
pub fn verify_content_type(path: &Path, expected_ext: &str) -> Result<()> {
    match infer::get_from_path(path) {
        Ok(Some(kind)) => {
            let actual_ext = kind.extension();
            // Normalize extensions (e.g., tgz -> gz) if necessary based on `infer` behavior
            // For now, direct comparison:
            if actual_ext.eq_ignore_ascii_case(expected_ext) {
                tracing::debug!(
                    "Content type verified: {} matches expected {}",
                    actual_ext,
                    expected_ext
                );
                Ok(())
            } else {
                Err(SpmError::Generic(format!(
                    // Consider a specific SpmError::ValidationError
                    "Content type mismatch for {}: expected extension '{}', but detected '{}'",
                    path.display(),
                    expected_ext,
                    actual_ext
                )))
            }
        }
        Ok(None) => Err(SpmError::Generic(format!(
            "Could not determine content type for {}",
            path.display()
        ))),
        Err(e) => Err(SpmError::Io(e)), // Propagate IO error
    }
}

/// Validates a URL, ensuring it uses the HTTPS scheme.
pub fn validate_url(url_str: &str) -> Result<()> {
    match Url::parse(url_str) {
        Ok(url) => {
            if url.scheme() == "https" {
                Ok(())
            } else {
                Err(SpmError::ValidationError(format!(
                    // Consider SpmError::ValidationError
                    "Invalid URL scheme for '{}': Must be https, but got '{}'",
                    url_str,
                    url.scheme()
                )))
            }
        }
        Err(e) => Err(SpmError::Generic(format!(
            // Consider SpmError::ValidationError
            "Failed to parse URL '{url_str}': {e}"
        ))),
    }
}
