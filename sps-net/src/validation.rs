// sps-io/src/checksum.rs
//use std::sync::Arc;
use std::fs::File;
use std::io;
use std::path::Path;

use infer;
use sha2::{Digest, Sha256};
use sps_common::error::{Result, SpsError};
use url::Url;
//use tokio::fs::File;
//use tokio::io::AsyncReadExt;
//use tracing::debug; // Use tracing

///// Asynchronously verifies the SHA256 checksum of a file.
///// Reads the file asynchronously but performs hashing synchronously.
//pub async fn verify_checksum_async(path: &Path, expected: &str) -> Result<()> {
//debug!("Async Verifying checksum for: {}", path.display());
//    let file = File::open(path).await;
//    let mut file = match file {
//        Ok(f) => f,
//        Err(e) => {
//            return Err(SpsError::Io(Arc::new(e))); // Wrap IO error
//        }
//    };
//
//    let mut hasher = Sha256::new();
//    let mut buffer = Vec::with_capacity(8192); // Use a Vec as buffer for read_buf
//    let mut total_bytes_read = 0;
//
//    loop {
//        buffer.clear();
//        match file.read_buf(&mut buffer).await {
//            Ok(0) => break, // End of file
//            Ok(n) => {
//                hasher.update(&buffer[..n]);
//                total_bytes_read += n as u64;
//            }
//            Err(e) => {
//                return Err(SpsError::Io(Arc::new(e))); // Wrap IO error
//            }
//        }
//    }
//
//    let hash_bytes = hasher.finalize();
//    let actual = hex::encode(hash_bytes);
//
//    debug!(
//        "Async Calculated SHA256: {} ({} bytes read)",
//        actual, total_bytes_read
//    );
//    debug!("Expected SHA256:   {}", expected);
//
//    if actual.eq_ignore_ascii_case(expected) {
//        Ok(())
//    } else {
//        Err(SpsError::ChecksumError(format!(
//            "Checksum mismatch for {}: expected {}, got {}",
//            path.display(),
//            expected,
//            actual
//        )))
//    }
//}

// Keep the synchronous version for now if needed elsewhere or for comparison
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
        Err(SpsError::ChecksumError(format!(
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
            Err(SpsError::Generic(format!(
                "Content type mismatch for {}: expected extension '{}', but detected '{}'",
                path.display(),
                expected_ext,
                actual_ext
            )))
        }
    } else {
        Err(SpsError::Generic(format!(
            "Could not determine content type for {}",
            path.display()
        )))
    }
}

/// Validates a URL, ensuring it uses the HTTPS scheme.
pub fn validate_url(url_str: &str) -> Result<()> {
    let url = Url::parse(url_str)
        .map_err(|e| SpsError::Generic(format!("Failed to parse URL '{url_str}': {e}")))?;
    if url.scheme() == "https" {
        Ok(())
    } else {
        Err(SpsError::ValidationError(format!(
            "Invalid URL scheme for '{}': Must be https, but got '{}'",
            url_str,
            url.scheme()
        )))
    }
}
