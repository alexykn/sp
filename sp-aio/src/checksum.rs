// sp-aio/src/checksum.rs
// (Content moved from sp-net/src/validation.rs)
use std::fs::File;
use std::io::{self}; // Added Read
use std::path::Path;
use std::sync::Arc; // Keep Arc

use sha2::{Digest, Sha256};
use sp_common::error::{Result, SpError}; // <-- Use common error
use {hex, tracing}; // Use tracing

pub fn verify_checksum(path: &Path, expected: &str) -> Result<()> {
    tracing::debug!("Verifying checksum for: {}", path.display());
    let mut file = File::open(path).map_err(|e| SpError::Io(Arc::new(e)))?; // Wrap IO error
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
        Err(SpError::ChecksumError(format!( // Use specific error variant
            "Checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            actual
        )))
    }
}
