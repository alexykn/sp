use std::collections::HashMap;
use std::sync::Arc;

use tracing::debug;

use super::cache::Cache;
use super::config::Config;
use super::error::{Result, SpsError};
use super::model::formula::Formula;

#[derive()]
pub struct Formulary {
    cache: Cache,
    parsed_cache: std::sync::Mutex<HashMap<String, std::sync::Arc<Formula>>>,
}

impl Formulary {
    pub fn new(config: Config) -> Self {
        let cache = Cache::new(&config).unwrap_or_else(|e| {
            panic!("Failed to initialize cache in Formulary: {e}");
        });
        Self {
            cache,
            parsed_cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn load_formula(&self, name: &str) -> Result<Formula> {
        let mut parsed_cache_guard = self.parsed_cache.lock().unwrap();
        if let Some(formula_arc) = parsed_cache_guard.get(name) {
            debug!("Loaded formula '{}' from parsed cache.", name);
            return Ok(Arc::clone(formula_arc).as_ref().clone());
        }
        drop(parsed_cache_guard);

        let raw_data = self.cache.load_raw("formula.json")?;
        let all_formulas: Vec<Formula> = serde_json::from_str(&raw_data)
            .map_err(|e| SpsError::Cache(format!("Failed to parse cached formula data: {e}")))?;
        debug!("Parsed {} formulas.", all_formulas.len());

        let mut found_formula: Option<Formula> = None;
        parsed_cache_guard = self.parsed_cache.lock().unwrap();
        for formula in all_formulas {
            let formula_name = formula.name.clone();
            let formula_arc = std::sync::Arc::new(formula);

            if formula_name == name {
                found_formula = Some(Arc::clone(&formula_arc).as_ref().clone());
            }

            parsed_cache_guard
                .entry(formula_name)
                .or_insert(formula_arc);
        }

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
                Err(SpsError::Generic(format!(
                    "Formula '{name}' not found in cache."
                )))
            }
        }
    }
}
