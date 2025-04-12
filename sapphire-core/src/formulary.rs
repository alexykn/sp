use crate::utils::config::Config;
use crate::model::formula::Formula;
use crate::utils::error::{Result, SapphireError};
use std::fs;
use std::path::PathBuf;

const DEFAULT_CORE_TAP: &str = "homebrew/core";

/// Responsible for finding and loading Formula definitions.
#[derive(Debug)]
pub struct Formulary {
    config: Config,
    // Optional: Add a cache for loaded formulas to avoid repeated file reads/parsing
    // cache: Mutex<HashMap<String, std::sync::Arc<Formula>>>,
}

impl Formulary {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            // cache: Mutex::new(HashMap::new()),
        }
    }

    /// Resolves a formula name (potentially unqualified) to a tap and formula file path.
    /// Returns (tap_name, formula_file_path)
    fn resolve_formula_path(&self, name: &str) -> Result<(String, PathBuf)> {
        println!("Resolving formula path for: {}", name);
        let (tap_name, formula_name) = self.parse_qualified_name(name);

        if let Some(path) = self.config.get_formula_path(&tap_name, &formula_name) {
            if path.is_file() {
                println!("Found '{}' at path: {}", name, path.display());
                return Ok((tap_name, path));
            } else {
                println!("Path exists but is not a file: {}", path.display());
            }
        } else {
            println!("Could not construct path for tap '{}'", tap_name);
        }

        // If unqualified and not found in assumed tap, check default core tap
        if !name.contains('/') {
            println!("'{}' not found in implicitly assumed tap, trying core tap: {}", name, DEFAULT_CORE_TAP);
            let core_tap_name = DEFAULT_CORE_TAP.to_string();
            if let Some(path) = self.config.get_formula_path(&core_tap_name, name) {
                if path.is_file() {
                    println!("Found '{}' in core tap at path: {}", name, path.display());
                    return Ok((core_tap_name, path));
                } else {
                    println!("Path in core tap not found or not a file: {}", path.display());
                }
            }
        }

        println!("Formula '{}' not found in any known tap locations.", name);
        Err(SapphireError::Generic(format!("Formula not found: {}", name)))
    }

    /// Parses a name like "user/repo/formula" or just "formula".
    /// Returns ("user/repo", "formula") or ("homebrew/core", "formula") etc.
    fn parse_qualified_name(&self, name: &str) -> (String, String) {
        let parts: Vec<&str> = name.split('/').collect();
        match parts.len() {
            3 => (format!("{}/{}", parts[0], parts[1]), parts[2].to_string()),
            2 => (format!("homebrew/{}", parts[0]), parts[1].to_string()),
            1 => (DEFAULT_CORE_TAP.to_string(), parts[0].to_string()),
            _ => (DEFAULT_CORE_TAP.to_string(), name.to_string()),
        }
    }

    /// Loads a formula definition by name.
    pub fn load_formula(&self, name: &str) -> Result<Formula> {
        // let mut cache = self.cache.lock().unwrap();
        // if let Some(formula_arc) = cache.get(name) {
        //     println!("Loaded formula '{}' from cache.", name);
        //     return Ok((*formula_arc).clone());
        // }
        // drop(cache);

        let (_tap_name, formula_path) = self.resolve_formula_path(name)?;

        println!("Loading formula definition from: {}", formula_path.display());
        let file_content = fs::read_to_string(&formula_path)
            .map_err(|e| SapphireError::Generic(format!("Failed to read formula file {}: {}", formula_path.display(), e)))?;

        let formula: Formula = serde_json::from_str(&file_content)
            .map_err(|e| SapphireError::Generic(format!("Failed to parse formula {} ({}): {}", name, formula_path.display(), e)))?;

        println!("Successfully parsed formula '{}' version {}", formula.name, formula.version_str_full());

        // let mut cache = self.cache.lock().unwrap();
        // cache.insert(name.to_string(), std::sync::Arc::new(formula.clone()));

        Ok(formula)
    }
}

// --- Logging Macros (assume these are defined elsewhere or use eprintln for now) ---
#[allow(unused_macros)]
macro_rules! debug {
    ($($arg:tt)*) => { eprintln!("DEBUG [formulary]: {}", format!($($arg)*)); };
}
#[allow(unused_macros)]
macro_rules! error {
    ($($arg:tt)*) => { eprintln!("ERROR [formulary]: {}", format!($($arg)*)); };
}