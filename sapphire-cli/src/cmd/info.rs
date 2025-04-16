// src/cmd/info.rs
// Contains the logic for the `info` command.

use sapphire_core::fetch::api;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;
use sapphire_core::model::formula::Formula;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::{Result, SapphireError};
use serde_json::Value;
// Removed unused prettytable imports; using fully qualified paths for table and row macros

/// Displays detailed information about a formula or cask.
pub async fn run_info(name: &str, is_cask: bool) -> Result<()> {
    log::debug!("Getting info for package: {}, is_cask: {}", name, is_cask);
    // Spinner for info loading
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::with_template("{spinner:.magenta} {msg}").unwrap());
    pb.set_message(format!("Loading info for {}", name));
    pb.enable_steady_tick(Duration::from_millis(100));

    // Initialize config and cache
    let config = Config::load()?;
    let cache = Cache::new(&config.cache_dir)?;

    if is_cask {
        // Try as cask first when the flag is set
        if let Ok(info) = get_cask_info(&cache, name).await {
            pb.finish_and_clear();
            print_cask_info(name, &info);
            return Ok(());
        }
        // If specified as a cask but not found, return an error
        return Err(SapphireError::NotFound(format!(
            "Cask '{}' not found",
            name
        )));
    } else {
        // Try as formula first (only if a bottle is available)
        if let Ok(info) = get_formula_info_raw(&cache, name).await {
            if is_bottle_available(&info) {
                pb.finish_and_clear();
                print_formula_info(name, &info);
                return Ok(());
            }
            // Skip formulas without bottles
        }

        // If not found as formula, try as cask
        if let Ok(info) = get_cask_info(&cache, name).await {
            pb.finish_and_clear();
            print_cask_info(name, &info);
            return Ok(());
        }
    }

    // If we get here, the package was not found
    Err(SapphireError::NotFound(format!(
        "Package '{}' not found",
        name
    )))
}

/// Public function that retrieves formula information and returns the Formula model
pub async fn get_formula_info(name: &str) -> Result<Formula> {
    // Initialize config and cache
    let config = Config::load()?;
    let cache = Cache::new(&config.cache_dir)?;

    // Get the raw JSON value
    let raw_info = get_formula_info_raw(&cache, name).await?;

    // Parse the JSON into a Formula struct
    let formula: Formula = serde_json::from_value(raw_info).map_err(|e| SapphireError::Json(e))?;

    Ok(formula)
}

/// Retrieves formula information from the cache or API as raw JSON
async fn get_formula_info_raw(cache: &Cache, name: &str) -> Result<Value> {
    // First try to load from cache
    if let Ok(formula_data) = cache.load_raw("formula.json") {
        // Parse the JSON
        let formulas: Vec<Value> = serde_json::from_str(&formula_data)?;

        // Find the formula with the matching name
        for formula in formulas {
            if let Some(formula_name) = formula.get("name").and_then(|n| n.as_str()) {
                if formula_name == name {
                    return Ok(formula);
                }
            }
        }
    }

    // If not found in cache, try the API
    log::debug!("Formula not found in cache, fetching from API...");
    api::fetch_formula(name).await
}

/// Retrieves cask information from the cache or API
async fn get_cask_info(cache: &Cache, name: &str) -> Result<Value> {
    // First try to load from cache
    if let Ok(cask_data) = cache.load_raw("cask.json") {
        // Parse the JSON
        let casks: Vec<Value> = serde_json::from_str(&cask_data)?;

        // Find the cask with the matching token or name
        for cask in casks {
            if let Some(cask_token) = cask.get("token").and_then(|t| t.as_str()) {
                if cask_token == name {
                    return Ok(cask);
                }
            }
        }
    }

    // If not found in cache, try the API
    log::debug!("Cask not found in cache, fetching from API...");
    api::fetch_cask(name).await
}

/// Prints formula information in a formatted table
fn print_formula_info(_name: &str, formula: &Value) {
    let full_name = formula.get("full_name").and_then(|f| f.as_str()).unwrap_or("N/A");
    let version = formula.get("versions").and_then(|v| v.get("stable")).and_then(|s| s.as_str()).unwrap_or("N/A");
    let license = formula.get("license").and_then(|l| l.as_str()).unwrap_or("N/A");
    let homepage = formula.get("homepage").and_then(|h| h.as_str()).unwrap_or("N/A");

    // Summary table
    let mut table = prettytable::Table::new();
    table.set_format(*prettytable::format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);
    table.add_row(prettytable::row!["Name", full_name]);
    table.add_row(prettytable::row!["Version", version]);
    table.add_row(prettytable::row!["License", license]);
    table.add_row(prettytable::row!["Homepage", homepage]);
    table.printstd();

    // Detailed sections
    if let Some(desc) = formula.get("desc").and_then(|d| d.as_str()) {
        if !desc.is_empty() {
            println!("\n{}", "Description".blue().bold());
            println!("  {}", desc);
        }
    }
    if let Some(caveats) = formula.get("caveats").and_then(|c| c.as_str()) {
        if !caveats.is_empty() {
            println!("\n{}", "Caveats".blue().bold());
            println!("  {}", caveats);
        }
    }
    if let Some(deps) = formula.get("dependencies").and_then(|d| d.as_array()) {
        let dep_list: Vec<&str> = deps.iter().filter_map(|d| d.as_str()).collect();
        if !dep_list.is_empty() {
            println!("\n{}", "Dependencies".blue().bold());
            for d in dep_list {
                println!("  - {}", d);
            }
        }
    }
}

/// Prints cask information in a formatted table
fn print_cask_info(name: &str, cask: &Value) {
    // Header
    println!("{}",
        format!("Cask: {}", name).green().bold()
    );

    // Summary table
    let mut table = prettytable::Table::new();
    table.set_format(*prettytable::format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);
    if let Some(names) = cask.get("name").and_then(|n| n.as_array()) {
        if let Some(first) = names.first().and_then(|s| s.as_str()) {
            table.add_row(prettytable::row!["Name", first]);
        }
    }
    if let Some(desc) = cask.get("desc").and_then(|d| d.as_str()) {
        table.add_row(prettytable::row!["Description", desc]);
    }
    if let Some(homepage) = cask.get("homepage").and_then(|h| h.as_str()) {
        table.add_row(prettytable::row!["Homepage", homepage]);
    }
    if let Some(version) = cask.get("version").and_then(|v| v.as_str()) {
        table.add_row(prettytable::row!["Version", version]);
    }
    if let Some(url) = cask.get("url").and_then(|u| u.as_str()) {
        table.add_row(prettytable::row!["Download URL", url]);
    }
    table.printstd();
    
    // Installation hint
    println!("\n{}", "Installation".blue().bold());
    println!("  {} install {}{}",
        "sapphire".cyan(),
        if name.contains(":") { "--cask " } else { "" },
        name
    );
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
