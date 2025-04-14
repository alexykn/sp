// src/cmd/search.rs
// Contains the logic for the `search` command.

use sapphire_core::fetch::api;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::Result;
use serde_json::Value;

/// Represents the type of package to search for
pub enum SearchType {
    All,
    Formula,
    Cask,
}

/// Searches for packages matching the query
pub async fn run_search(query: &str, search_type: SearchType) -> Result<()> {
    log::info!("Searching for packages matching: {}", query);

    // Initialize config and cache
    let config = Config::load()?;
    let cache = Cache::new(&config.cache_dir)?;

    // Store search results
    let mut formula_matches = Vec::new();
    let mut cask_matches = Vec::new();

    // Search formulas if needed
    if matches!(search_type, SearchType::All | SearchType::Formula) {
        formula_matches = search_formulas(&cache, query).await?;
    }

    // Search casks if needed
    if matches!(search_type, SearchType::All | SearchType::Cask) {
        cask_matches = search_casks(&cache, query).await?;
    }

    // Print results
    print_search_results(query, &formula_matches, &cask_matches);

    Ok(())
}

/// Search for formulas matching the query
async fn search_formulas(cache: &Cache, query: &str) -> Result<Vec<Value>> {
    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();

    // Try to load from cache
    if let Ok(formula_data) = cache.load_raw("formula.json") {
        // Parse the JSON
        let formulas: Vec<Value> = serde_json::from_str(&formula_data)?;

        // Find matching formulas
        for formula in formulas {
            if is_formula_match(&formula, &query_lower) {
                matches.push(formula);
            }
        }
    } else {
        // If cache fails, fetch from API
        log::info!("Formula cache not found, fetching from API...");
        let all_formulas = api::fetch_all_formulas().await?;
        let formulas: Vec<Value> = serde_json::from_str(&all_formulas)?;

        for formula in formulas {
            if is_formula_match(&formula, &query_lower) {
                matches.push(formula);
            }
        }
    }

    Ok(matches)
}

/// Search for casks matching the query
async fn search_casks(cache: &Cache, query: &str) -> Result<Vec<Value>> {
    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();

    // Try to load from cache
    if let Ok(cask_data) = cache.load_raw("cask.json") {
        // Parse the JSON
        let casks: Vec<Value> = serde_json::from_str(&cask_data)?;

        // Find matching casks
        for cask in casks {
            if is_cask_match(&cask, &query_lower) {
                matches.push(cask);
            }
        }
    } else {
        // If cache fails, fetch from API
        log::info!("Cask cache not found, fetching from API...");
        let all_casks = api::fetch_all_casks().await?;
        let casks: Vec<Value> = serde_json::from_str(&all_casks)?;

        for cask in casks {
            if is_cask_match(&cask, &query_lower) {
                matches.push(cask);
            }
        }
    }

    Ok(matches)
}

/// Check if a formula matches the search query
fn is_formula_match(formula: &Value, query: &str) -> bool {
    // Check name
    if let Some(name) = formula.get("name").and_then(|n| n.as_str()) {
        if name.to_lowercase().contains(query) {
            return true;
        }
    }

    // Check full_name
    if let Some(full_name) = formula.get("full_name").and_then(|n| n.as_str()) {
        if full_name.to_lowercase().contains(query) {
            return true;
        }
    }

    // Check description
    if let Some(desc) = formula.get("desc").and_then(|d| d.as_str()) {
        if desc.to_lowercase().contains(query) {
            return true;
        }
    }

    // Check aliases
    if let Some(aliases) = formula.get("aliases").and_then(|a| a.as_array()) {
        for alias in aliases {
            if let Some(alias_str) = alias.as_str() {
                if alias_str.to_lowercase().contains(query) {
                    return true;
                }
            }
        }
    }

    false
}

/// Check if a cask matches the search query
fn is_cask_match(cask: &Value, query: &str) -> bool {
    // Check token
    if let Some(token) = cask.get("token").and_then(|t| t.as_str()) {
        if token.to_lowercase().contains(query) {
            return true;
        }
    }

    // Check name array
    if let Some(names) = cask.get("name").and_then(|n| n.as_array()) {
        for name in names {
            if let Some(name_str) = name.as_str() {
                if name_str.to_lowercase().contains(query) {
                    return true;
                }
            }
        }
    }

    // Check description
    if let Some(desc) = cask.get("desc").and_then(|d| d.as_str()) {
        if desc.to_lowercase().contains(query) {
            return true;
        }
    }

    false
}

/// Print search results in a formatted way
fn print_search_results(query: &str, formula_matches: &[Value], cask_matches: &[Value]) {
    let formula_count = formula_matches.len();
    let cask_count = cask_matches.len();
    let total_count = formula_count + cask_count;

    if total_count == 0 {
        println!("No matches found for \"{}\"", query);
        return;
    }

    // Print formula matches
    if !formula_matches.is_empty() {
        println!("==> Formulae");
        for formula in formula_matches {
            let name = formula
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("Unknown");
            let desc = formula.get("desc").and_then(|d| d.as_str()).unwrap_or("");
            println!("{}: {}", name, desc);
        }
    }

    // Add a separator if both types of matches exist
    if !formula_matches.is_empty() && !cask_matches.is_empty() {
        println!();
    }

    // Print cask matches
    if !cask_matches.is_empty() {
        println!("==> Casks");
        for cask in cask_matches {
            let token = cask
                .get("token")
                .and_then(|t| t.as_str())
                .unwrap_or("Unknown");
            let desc = cask.get("desc").and_then(|d| d.as_str()).unwrap_or("");
            println!("{}: {}", token, desc);
        }
    }
}
