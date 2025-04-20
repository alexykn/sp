use crate::model::formula::Formula;
use crate::utils::cache::Cache;
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError}; // Import the Cache struct
                                                  // Removed: use std::fs;
                                                  // Removed: use std::path::PathBuf;
                                                  // Removed: const DEFAULT_CORE_TAP: &str = "homebrew/core";
use std::collections::HashMap; // For caching parsed formulas
use std::sync::Arc; // Import Arc for thread-safe shared ownership
use log::debug;

/// Responsible for finding and loading Formula definitions from the API cache.
#[derive()]
pub struct Formulary {
    // config: Config, // Keep config if needed for cache path, etc.
    cache: Cache,
    // Optional: Add a cache for *parsed* formulas to avoid repeated parsing of the large JSON
    parsed_cache: std::sync::Mutex<HashMap<String, std::sync::Arc<Formula>>>, // Using Arc for thread-safety
}

impl Formulary {
    pub fn new(config: Config) -> Self {
        // Initialize the cache helper using the directory from config
        let cache = Cache::new(&config.cache_dir).unwrap_or_else(|e| {
            // Handle error appropriately - maybe panic or return Result?
            // Using expect here for simplicity, but Result is better.
            panic!("Failed to initialize cache in Formulary: {}", e);
        });
        Self {
            // config,
            cache,
            parsed_cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    // Removed: resolve_formula_path
    // Removed: parse_qualified_name

    /// Loads a formula definition by name from the API cache.
    pub fn load_formula(&self, name: &str) -> Result<Formula> {
        // 1. Check parsed cache first
        let mut parsed_cache_guard = self.parsed_cache.lock().unwrap();
        if let Some(formula_arc) = parsed_cache_guard.get(name) {
            debug!("Loaded formula '{}' from parsed cache.", name);
            return Ok(Arc::clone(formula_arc).as_ref().clone());
        }
        // Release lock early if not found
        drop(parsed_cache_guard);

        // 2. Load the raw formula list from the main cache file
        debug!("Loading raw formula data from cache file 'formula.json'...");
        let raw_data = self.cache.load_raw("formula.json")?; // Assumes update stored it here

        // 3. Parse the entire JSON array
        // This could be expensive, hence the parsed_cache above.
        debug!("Parsing full formula list...");
        let all_formulas: Vec<Formula> = serde_json::from_str(&raw_data).map_err(|e| {
            SapphireError::Cache(format!("Failed to parse cached formula data: {}", e))
        })?;
        debug!("Parsed {} formulas.", all_formulas.len());

        // 4. Find the requested formula and populate the parsed cache
        let mut found_formula: Option<Formula> = None;
        // Lock again to update the parsed cache
        parsed_cache_guard = self.parsed_cache.lock().unwrap();
        // Use entry API to avoid redundant lookups if another thread populated it
        for formula in all_formulas {
            let formula_name = formula.name.clone(); // Clone name for insertion
            let formula_arc = std::sync::Arc::new(formula); // Create Arc once

            // If this is the formula we're looking for, store it for return value
            if formula_name == name {
                found_formula = Some(Arc::clone(&formula_arc).as_ref().clone());
                // Clone Formula out
            }

            // Insert into parsed cache using entry API
            parsed_cache_guard
                .entry(formula_name)
                .or_insert(formula_arc);
        }

        // 5. Return the found formula or an error
        match found_formula {
            Some(f) => {
                debug!(
                    "Successfully loaded formula '{}' version {}",
                    f.name,
                    f.version_str_full()
                );
                Ok(f)
            }
            None => {
                debug!(
                    "Formula '{}' not found within the cached formula data.",
                    name
                );
                Err(SapphireError::Generic(format!(
                    "Formula '{}' not found in cache.",
                    name
                )))
            }
        }
    }
}

