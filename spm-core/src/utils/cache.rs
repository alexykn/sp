// src/utils/cache.rs
// Handles caching of formula data and downloads

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::utils::error::{Result, SpmError};

// TODO: Define cache directory structure (e.g., ~/.cache/brew-rs-client)
// TODO: Implement functions for storing, retrieving, and clearing cached data.

const CACHE_SUBDIR: &str = "brew-rs-client";
// Define how long cache entries are considered valid
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60); // 24 hours

/// Cache struct to manage cache operations
pub struct Cache {
    cache_dir: PathBuf,
}

impl Cache {
    pub fn new(cache_dir: &Path) -> Result<Self> {
        if !cache_dir.exists() {
            fs::create_dir_all(cache_dir)?;
        }

        Ok(Self {
            cache_dir: cache_dir.to_path_buf(),
        })
    }

    /// Gets the cache directory path
    pub fn get_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Stores raw string data in the cache
    pub fn store_raw(&self, filename: &str, data: &str) -> Result<()> {
        let path = self.cache_dir.join(filename);
        tracing::debug!("Saving raw data to cache file: {:?}", path);
        fs::write(&path, data)?;
        Ok(())
    }

    /// Loads raw string data from the cache
    pub fn load_raw(&self, filename: &str) -> Result<String> {
        let path = self.cache_dir.join(filename);
        tracing::debug!("Loading raw data from cache file: {:?}", path);

        if !path.exists() {
            return Err(SpmError::Cache(format!(
                "Cache file {filename} does not exist"
            )));
        }

        fs::read_to_string(&path).map_err(|e| SpmError::Cache(format!("IO error: {e}")))
    }

    /// Checks if a cache file exists and is valid (within TTL)
    pub fn is_cache_valid(&self, filename: &str) -> Result<bool> {
        let path = self.cache_dir.join(filename);
        if !path.exists() {
            return Ok(false);
        }

        let metadata = fs::metadata(&path)?;
        let modified_time = metadata.modified()?;
        let age = SystemTime::now()
            .duration_since(modified_time)
            .map_err(|e| SpmError::Cache(format!("System time error: {e}")))?;

        Ok(age <= CACHE_TTL)
    }

    /// Clears a specific cache file
    pub fn clear_file(&self, filename: &str) -> Result<()> {
        let path = self.cache_dir.join(filename);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Clears all cache files
    pub fn clear_all(&self) -> Result<()> {
        if self.cache_dir.exists() {
            fs::remove_dir_all(&self.cache_dir)?;
            fs::create_dir_all(&self.cache_dir)?;
        }
        Ok(())
    }
}

/// Gets the path to the application's cache directory, creating it if necessary.
/// Uses dirs::cache_dir() to find the appropriate system cache location.
pub fn get_cache_dir() -> Result<PathBuf> {
    let base_cache_dir = dirs::cache_dir()
        .ok_or_else(|| SpmError::Cache("Could not determine system cache directory".to_string()))?;
    let app_cache_dir = base_cache_dir.join(CACHE_SUBDIR);

    if !app_cache_dir.exists() {
        tracing::debug!("Creating cache directory at {:?}", app_cache_dir);
        fs::create_dir_all(&app_cache_dir)?;
    }
    Ok(app_cache_dir)
}

/// Constructs the full path for a given cache filename.
fn get_cache_path(filename: &str) -> Result<PathBuf> {
    Ok(get_cache_dir()?.join(filename))
}

/// Saves serializable data to a file in the cache directory.
/// The data is serialized as JSON.
pub fn save_to_cache<T: Serialize>(filename: &str, data: &T) -> Result<()> {
    let path = get_cache_path(filename)?;
    tracing::debug!("Saving data to cache file: {:?}", path);
    let file = fs::File::create(&path)?;
    // Use serde_json::to_writer_pretty for readable cache files (optional)
    serde_json::to_writer_pretty(file, data)?;
    Ok(())
}

/// Loads and deserializes data from a file in the cache directory.
/// Checks if the cache file exists and is within the TTL (Time To Live).
pub fn load_from_cache<T: DeserializeOwned>(filename: &str) -> Result<T> {
    let path = get_cache_path(filename)?;
    tracing::debug!("Attempting to load from cache file: {:?}", path);

    if !path.exists() {
        tracing::debug!("Cache file not found.");
        return Err(SpmError::Cache("Cache file does not exist".to_string()));
    }

    // Check cache file age
    let metadata = fs::metadata(&path)?;
    let modified_time = metadata.modified()?;
    let age = SystemTime::now()
        .duration_since(modified_time)
        .map_err(|e| SpmError::Cache(format!("System time error: {e}")))?;

    if age > CACHE_TTL {
        tracing::debug!("Cache file expired (age: {:?}, TTL: {:?}).", age, CACHE_TTL);
        return Err(SpmError::Cache(format!(
            "Cache file expired ({} > {})",
            humantime::format_duration(age),
            humantime::format_duration(CACHE_TTL)
        )));
    }

    tracing::debug!("Cache file is valid. Loading");
    let file = fs::File::open(&path)?;
    let data: T = serde_json::from_reader(file)?;
    Ok(data)
}

/// Clears the entire application cache directory.
pub fn clear_cache() -> Result<()> {
    let path = get_cache_dir()?;
    tracing::debug!("Clearing cache directory: {:?}", path);
    if path.exists() {
        fs::remove_dir_all(&path)?;
    }
    Ok(())
}

/// Checks if a specific cache file exists and is valid (within TTL).
pub fn is_cache_valid(filename: &str) -> Result<bool> {
    let path = get_cache_path(filename)?;
    if !path.exists() {
        return Ok(false);
    }
    let metadata = fs::metadata(&path)?;
    let modified_time = metadata.modified()?;
    let age = SystemTime::now()
        .duration_since(modified_time)
        .map_err(|e| SpmError::Cache(format!("System time error: {e}")))?;
    Ok(age <= CACHE_TTL)
}
