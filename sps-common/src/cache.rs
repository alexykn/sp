// src/utils/cache.rs
// Handles caching of formula data and downloads

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::error::{Result, SpsError};
use crate::Config;

/// Define how long cache entries are considered valid
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60); // 24 hours

/// Cache struct to manage cache operations
pub struct Cache {
    cache_dir: PathBuf,
    _config: Config, // Keep a reference to config if needed for other paths or future use
}

impl Cache {
    /// Create a new Cache using the config's cache_dir
    pub fn new(config: &Config) -> Result<Self> {
        let cache_dir = config.cache_dir();
        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)?;
        }

        Ok(Self {
            cache_dir,
            _config: config.clone(),
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
            return Err(SpsError::Cache(format!(
                "Cache file {filename} does not exist"
            )));
        }

        fs::read_to_string(&path).map_err(|e| SpsError::Cache(format!("IO error: {e}")))
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
            .map_err(|e| SpsError::Cache(format!("System time error: {e}")))?;

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

    /// Gets a reference to the config
    pub fn config(&self) -> &Config {
        &self._config
    }
}
