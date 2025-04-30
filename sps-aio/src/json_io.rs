// sps-aio/src/json_io.rs
use std::path::Path;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use sps_common::error::{Result, SpsError};
use tokio::fs; // Use tokio::fs
use tracing::debug; // Use tracing

/// Asynchronously writes serializable data to a JSON file (pretty-printed).
pub async fn write_json_async<T: Serialize + Send + Sync + 'static>(
    // Add Send + Sync + 'static bounds for spawn_blocking
    path: &Path,
    data: &T,
) -> Result<()> {
    debug!("Async Writing JSON to: {}", path.display());
    // Ensure parent directory exists asynchronously
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    // Serialize synchronously (CPU-bound, usually fast)
    // If serialization becomes a bottleneck for huge data, wrap this in spawn_blocking
    let json_bytes = match serde_json::to_vec_pretty(data) {
        Ok(bytes) => bytes,
        Err(e) => return Err(SpsError::Json(Arc::new(e))),
    };

    // Write the bytes asynchronously using atomic_write_file_async
    crate::fs::atomic_write_file_async(path, &json_bytes).await
}

/// Asynchronously reads and deserializes data from a JSON file.
pub async fn read_json_async<T: DeserializeOwned>(path: &Path) -> Result<T> {
    debug!("Async Reading JSON from: {}", path.display());
    // Read bytes asynchronously
    let json_bytes = crate::fs::read_to_bytes_async(path).await?;

    // Deserialize synchronously (CPU-bound, usually fast)
    // If deserialization becomes a bottleneck for huge files, wrap this in spawn_blocking
    serde_json::from_slice(&json_bytes).map_err(|e| SpsError::Json(Arc::new(e)))
}

// --- Sync Versions (Kept for reference) ---

pub fn write_json_sync<T: Serialize>(path: &Path, data: &T) -> Result<()> {
    debug!("Sync Writing JSON to: {}", path.display());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(writer, data).map_err(|e| SpsError::Json(Arc::new(e)))
}

pub fn read_json_sync<T: DeserializeOwned>(path: &Path) -> Result<T> {
    debug!("Sync Reading JSON from: {}", path.display());
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    serde_json::from_reader(reader).map_err(|e| SpsError::Json(Arc::new(e)))
}
