// sp-aio/src/json_io.rs
use sps_common::error::{Result, SpsError};
use serde::{Serialize, de::DeserializeOwned};
use std::fs::{self, File};
use std::path::Path;
use std::io::{BufWriter, BufReader}; // Import BufReader
use std::sync::Arc;

pub fn write_json_sync<T: Serialize>(path: &Path, data: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?; // Ensure parent dir exists
    }
    let file = File::create(path)?;
    let writer = BufWriter::new(file); // Use buffered writer
    serde_json::to_writer_pretty(writer, data)
        .map_err(|e| SpsError::Json(Arc::new(e))) // Wrap serde error
}

pub fn read_json_sync<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let file = File::open(path)?;
    let reader = BufReader::new(file); // Use buffered reader
    serde_json::from_reader(reader)
        .map_err(|e| SpsError::Json(Arc::new(e))) // Wrap serde error
}