// sps-aio/src/checksum.rs
use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use sps_common::error::{Result, SpsError};
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tracing::debug; // Use tracing

/// Asynchronously verifies the SHA256 checksum of a file.
/// Reads the file asynchronously but performs hashing synchronously.
pub async fn verify_checksum_async(path: &Path, expected: &str) -> Result<()> {
    debug!("Async Verifying checksum for: {}", path.display());
    let file = File::open(path).await;
    let mut file = match file {
        Ok(f) => f,
        Err(e) => {
            return Err(SpsError::Io(Arc::new(e))); // Wrap IO error
        }
    };

    let mut hasher = Sha256::new();
    let mut buffer = Vec::with_capacity(8192); // Use a Vec as buffer for read_buf
    let mut total_bytes_read = 0;

    loop {
        buffer.clear();
        match file.read_buf(&mut buffer).await {
            Ok(0) => break, // End of file
            Ok(n) => {
                hasher.update(&buffer[..n]);
                total_bytes_read += n as u64;
            }
            Err(e) => {
                return Err(SpsError::Io(Arc::new(e))); // Wrap IO error
            }
        }
    }

    let hash_bytes = hasher.finalize();
    let actual = hex::encode(hash_bytes);

    debug!(
        "Async Calculated SHA256: {} ({} bytes read)",
        actual, total_bytes_read
    );
    debug!("Expected SHA256:   {}", expected);

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

// Keep the synchronous version for now if needed elsewhere or for comparison
pub fn verify_checksum_sync(path: &Path, expected: &str) -> Result<()> {
    debug!("Sync Verifying checksum for: {}", path.display());
    let std_file = std::fs::File::open(path).map_err(|e| SpsError::Io(Arc::new(e)))?;
    let mut std_reader = std::io::BufReader::new(std_file); // Use buffered reader
    let mut hasher = Sha256::new();
    let bytes_copied = std::io::copy(&mut std_reader, &mut hasher)?;
    let hash_bytes = hasher.finalize();
    let actual = hex::encode(hash_bytes);

    debug!(
        "Sync Calculated SHA256: {} ({} bytes read)",
        actual, bytes_copied
    );
    debug!("Expected SHA256:   {}", expected);

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
