// src/cmd/search.rs
// Contains the logic for the `search` command.

use sapphire_core::fetch::api;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::Result;
use serde_json::Value;
use prettytable::{Table, format};

/// Represents the type of package to search for
pub enum SearchType {
    All,
    Formula,
    Cask,
}

/// Searches for packages matching the query
pub async fn run_search(query: &str, search_type: SearchType) -> Result<()> {
    log::debug!("Searching for packages matching: {}", query);
    // Spinner for searching
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::with_template("{spinner:.cyan} {msg}").unwrap());
    pb.set_message(format!("Searching for \"{}\"", query));
    pb.enable_steady_tick(Duration::from_millis(100));

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

    // Finished searching
    pb.finish_and_clear();
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
        log::debug!("Formula cache not found, fetching from API...");
        let all_formulas = api::fetch_all_formulas().await?;
        let formulas: Vec<Value> = serde_json::from_str(&all_formulas)?;

        for formula in formulas {
            if is_formula_match(&formula, &query_lower) {
                matches.push(formula);
            }
        }
    }

    // Only include formulae that have bottles available
    matches.retain(|formula| is_bottle_available(formula));
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
        log::debug!("Cask cache not found, fetching from API...");
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

/// Check if a formula has a bottle available
fn is_bottle_available(formula: &Value) -> bool {
    if let Some(bottle) = formula.get("bottle").and_then(|b| b.as_object()) {
        if let Some(stable) = bottle.get("stable").and_then(|s| s.as_object()) {
            if let Some(files) = stable.get("files").and_then(|f| f.as_object()) {
                return !files.is_empty();
            }
        }
    }
    false
}

/// Drop‑in replacement for your existing `print_search_results`.
pub fn print_search_results(query: &str, formula_matches: &[Value], cask_matches: &[Value]) {
    let total = formula_matches.len() + cask_matches.len();
    if total == 0 {
        println!("{}", format!("No matches found for '{}'", query).yellow());
        return;
    }
    // Header
    println!("{}", format!("Found {} result(s) for '{}'", total, query).bold());

    // 1) Build table on *plain* text so widths are correct
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);
    table.set_titles(prettytable::row!["Type", "Name", "Description"]);

    for f in formula_matches {
        let name_plain = f.get("name").and_then(|n| n.as_str()).unwrap_or("Unknown");
        let desc       = f.get("desc").and_then(|d| d.as_str()).unwrap_or("");
        table.add_row(prettytable::row!["formula", name_plain, desc]);
    }
    for c in cask_matches {
        let token = c.get("token").and_then(|t| t.as_str()).unwrap_or("Unknown");
        let desc  = c.get("desc").and_then(|d| d.as_str()).unwrap_or("");
        table.add_row(prettytable::row!["cask", token, desc]);
    }

    // 2) Render to a String (layout based on un‐coloured text)
    let table_str = table.to_string();

    // 3) Post‐process: re‐inject ANSI colours around just the names/tokens
    let mut coloured = table_str.clone();
    for f in formula_matches {
        if let Some(name) = f.get("name").and_then(|n| n.as_str()) {
            let coloured_name = name.blue().bold().to_string();
            coloured = coloured.replace(name, &coloured_name);
        }
    }
    for c in cask_matches {
        if let Some(token) = c.get("token").and_then(|t| t.as_str()) {
            let coloured_token = token.blue().bold().to_string();
            coloured = coloured.replace(token, &coloured_token);
        }
    }

    // 4) Print the final, colourised table
    println!("{}", coloured);
}