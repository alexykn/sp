// Contains the logic for the `search` command.

use std::sync::Arc;

use clap::Args;
use colored::Colorize;
use prettytable::{format, Table, Cell, Row}; // Make sure this is imported
use serde_json::Value;
use spm_core::fetch::api;
use spm_core::utils::cache::Cache;
use spm_core::utils::config::Config;
use spm_core::utils::error::Result;
use terminal_size::{terminal_size, Width};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui;

#[derive(Args, Debug)]
pub struct Search {
    /// The search term to look for
    pub query: String,

    /// Search only formulae
    #[arg(long, conflicts_with = "cask")]
    pub formula: bool,

    /// Search only casks
    #[arg(long, conflicts_with = "formula")]
    pub cask: bool,
}

/// Represents the type of package to search for
pub enum SearchType {
    All,
    Formula,
    Cask,
}

impl Search {
    /// Runs the search command
    pub async fn run(&self, config: &Config, cache: Arc<Cache>) -> Result<()> {
        // Determine search type based on flags
        let search_type = if self.formula {
            SearchType::Formula
        } else if self.cask {
            SearchType::Cask
        } else {
            SearchType::All
        };

        // Run the search with the determined type
        run_search(&self.query, search_type, config, cache).await
    }
}

/// Searches for packages matching the query
pub async fn run_search(
    query: &str,
    search_type: SearchType,
    _config: &Config, // kept for potential future needs
    cache: Arc<Cache>,
) -> Result<()> {
    tracing::debug!("Searching for packages matching: {}", query);

    // Use the ui utility function to create the spinner
    let pb = ui::create_spinner(&format!("Searching for \"{query}\"")); // <-- CHANGED

    // Store search results
    let mut formula_matches = Vec::new();
    let mut cask_matches = Vec::new();
    let mut formula_err = None;
    let mut cask_err = None;

    // Search formulas if needed
    if matches!(search_type, SearchType::All | SearchType::Formula) {
        match search_formulas(Arc::clone(&cache), query).await {
            Ok(matches) => formula_matches = matches,
            Err(e) => {
                tracing::error!("Error searching formulas: {}", e);
                formula_err = Some(e); // Store error
            }
        }
    }

    // Search casks if needed
    if matches!(search_type, SearchType::All | SearchType::Cask) {
        match search_casks(Arc::clone(&cache), query).await {
            Ok(matches) => cask_matches = matches,
            Err(e) => {
                tracing::error!("Error searching casks: {}", e);
                cask_err = Some(e); // Store error
            }
        }
    }

    // Finished searching
    pb.finish_and_clear();

    // Handle potential errors after attempting searches
    if formula_matches.is_empty() && cask_matches.is_empty() {
        if let Some(e) = formula_err.or(cask_err) {
            // If both searches errored, return one of the errors
            return Err(e);
        }
        // If no errors but no matches, print message below
    }

    // Print results (even if empty, the function handles that)
    print_search_results(query, &formula_matches, &cask_matches);

    Ok(())
}

/// Search for formulas matching the query
async fn search_formulas(cache: Arc<Cache>, query: &str) -> Result<Vec<Value>> {
    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();
    let mut data_source_name = "cache"; // Assume cache initially

    // Try to load from cache
    let formula_data_result = cache.load_raw("formula.json");

    let formulas: Vec<Value> = match formula_data_result {
        Ok(formula_data) => serde_json::from_str(&formula_data)?,
        Err(e) => {
            // If cache fails, fetch from API
            tracing::debug!("Formula cache load failed ({}), fetching from API", e);
            data_source_name = "API";
            let all_formulas = api::fetch_all_formulas().await?; // This fetches String
                                                                 // Try to cache the fetched data
            if let Err(cache_err) = cache.store_raw("formula.json", &all_formulas) {
                tracing::warn!("Failed to cache formula data after fetching: {}", cache_err);
            }
            // Now parse the String fetched from API
            serde_json::from_str(&all_formulas)?
        }
    };

    // Find matching formulas from the loaded data (either cache or API)
    for formula in formulas {
        if is_formula_match(&formula, &query_lower) {
            matches.push(formula);
        }
    }

    tracing::debug!(
        "Found {} potential formula matches from {}",
        matches.len(),
        data_source_name
    );
    tracing::debug!(
        "Filtered down to {} formula matches with available bottles",
        matches.len()
    );

    Ok(matches)
}

/// Search for casks matching the query
async fn search_casks(cache: Arc<Cache>, query: &str) -> Result<Vec<Value>> {
    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();
    let mut data_source_name = "cache"; // Assume cache initially

    // Try to load from cache
    let cask_data_result = cache.load_raw("cask.json");

    let casks: Vec<Value> = match cask_data_result {
        Ok(cask_data) => serde_json::from_str(&cask_data)?,
        Err(e) => {
            // If cache fails, fetch from API
            tracing::debug!("Cask cache load failed ({}), fetching from API", e);
            data_source_name = "API";
            let all_casks = api::fetch_all_casks().await?; // Fetches String
                                                           // Try to cache the fetched data
            if let Err(cache_err) = cache.store_raw("cask.json", &all_casks) {
                tracing::warn!("Failed to cache cask data after fetching: {}", cache_err);
            }
            // Parse the String fetched from API
            serde_json::from_str(&all_casks)?
        }
    };

    // Find matching casks
    for cask in casks {
        if is_cask_match(&cask, &query_lower) {
            matches.push(cask);
        }
    }
    tracing::debug!(
        "Found {} cask matches from {}",
        matches.len(),
        data_source_name
    );
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

/// Truncates to max visible width, adding '…' if cut.
fn truncate_vis(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    let mut w = 0;
    let mut out = String::new();
    // Ensure max is at least 1 for the ellipsis
    let effective_max = if max > 0 { max } else { 1 };

    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        // Check if adding the next char *including* ellipsis fits
        if w + cw >= effective_max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// Width‑aware search results with Name:Desc = 1:2 truncation and Name coloured.
pub fn print_search_results(query: &str, formula_matches: &[Value], cask_matches: &[Value]) {
    let total = formula_matches.len() + cask_matches.len();
    if total == 0 {
        println!("{}", format!("No matches found for '{query}'").yellow());
        return;
    }
    println!(
        "{}",
        format!("Found {total} result(s) for '{query}'").bold()
    );

    // 1) Terminal width
    let term_cols = terminal_size()
        .map(|(Width(w), _)| w as usize)
        .unwrap_or(120);

    // Fixed columns
    let type_col = 7;
    let version_col = 10;
    let sep_width = 3 * 3; // 3 separators of " | "
    let total_fixed = type_col + version_col + sep_width;

    // Minimums
    let name_min_width = 10;
    let desc_min_width = 20;

    // Calculate widths
    let leftover = term_cols.saturating_sub(total_fixed + name_min_width + desc_min_width);
    let name_max = name_min_width + leftover / 3;
    let desc_max = desc_min_width + leftover - (leftover / 3);

    // Build table with header
    let mut tbl = Table::new();
    tbl.set_format(*format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);

        for formula in formula_matches {
        let raw_name = formula.get("name").and_then(|n| n.as_str()).unwrap_or("Unknown");
        let raw_desc = formula.get("desc").and_then(|d| d.as_str()).unwrap_or("");
        let desc = truncate_vis(raw_desc, desc_max);

    // Extract version (adjust if your JSON structure is different)
        let version = get_version(formula);

        tbl.add_row(Row::new(vec![
            Cell::new("Formula").style_spec("FgC"),
            Cell::new(raw_name).style_spec("Fb"),
            Cell::new(version),
            Cell::new(&desc),
        ]));
    }

    // Add a separation row if both formulas and casks exist
    if !formula_matches.is_empty() && !cask_matches.is_empty() {
        // Use a light horizontal line, spanning all columns
        tbl.add_row(Row::new(vec![
            Cell::new(" ").with_hspan(4)
        ]));
    }

    // Add cask rows
    for cask in cask_matches {
        let raw_name = cask.get("token").and_then(|t| t.as_str()).unwrap_or("Unknown");
        let raw_desc = cask.get("desc").and_then(|d| d.as_str()).unwrap_or("");
        let desc = truncate_vis(raw_desc, desc_max);

    // Extract version (adjust if your JSON structure is different)
        let version = get_version(cask);

        tbl.add_row(Row::new(vec![
            Cell::new("Cask").style_spec("FgG"),
            Cell::new(raw_name).style_spec("Fb"),
            Cell::new(version),
            Cell::new(&desc),
        ]));
    }

// 4) Print the table directly (coloring is done during row creation)
    tbl.printstd();
}

fn get_version(formula: &Value) -> &str {
    formula
        .get("versions")
        .and_then(|v| v.get("stable"))
        .and_then(|v| v.as_str())
        .unwrap_or("-")
}