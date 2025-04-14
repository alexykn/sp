// src/cmd/info.rs
// Contains the logic for the `info` command.

use sapphire_core::fetch::api;
use sapphire_core::model::formula::Formula;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::{Result, SapphireError};
use serde_json::Value;

/// Displays detailed information about a formula or cask.
pub async fn run_info(name: &str, is_cask: bool) -> Result<()> {
    log::info!("Getting info for package: {}, is_cask: {}", name, is_cask);

    // Initialize config and cache
    let config = Config::load()?;
    let cache = Cache::new(&config.cache_dir)?;

    if is_cask {
        // Try as cask first when the flag is set
        if let Ok(info) = get_cask_info(&cache, name).await {
            print_cask_info(name, &info);
            return Ok(());
        }
        // If specified as a cask but not found, return an error
        return Err(SapphireError::NotFound(format!(
            "Cask '{}' not found",
            name
        )));
    } else {
        // Try as formula first
        if let Ok(info) = get_formula_info_raw(&cache, name).await {
            print_formula_info(name, &info);
            return Ok(());
        }

        // If not found as formula, try as cask
        if let Ok(info) = get_cask_info(&cache, name).await {
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
    log::info!("Formula not found in cache, fetching from API...");
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
    log::info!("Cask not found in cache, fetching from API...");
    api::fetch_cask(name).await
}

/// Prints formula information in a human-readable format
fn print_formula_info(_name: &str, formula: &Value) {
    println!("Formula Info:");

    // Print basic information
    let full_name = formula
        .get("full_name")
        .and_then(|f| f.as_str())
        .unwrap_or("N/A");
    let version = formula
        .get("versions")
        .and_then(|v| v.get("stable"))
        .and_then(|s| s.as_str())
        .unwrap_or("N/A");
    let license = formula
        .get("license")
        .and_then(|l| l.as_str())
        .unwrap_or("N/A");
    println!("{} ({}), license: {}", full_name, version, license);

    // Print homepage if available
    if let Some(homepage) = formula.get("homepage").and_then(|h| h.as_str()) {
        println!("Homepage: {}", homepage);
    } else {
        println!("Homepage: N/A");
    }

    // Print description if available
    if let Some(desc) = formula.get("desc").and_then(|d| d.as_str()) {
        println!("\nDescription:");
        println!("  {}", desc);
    }

    // Print caveats if available
    if let Some(caveats) = formula.get("caveats").and_then(|c| c.as_str()) {
        if !caveats.is_empty() {
            println!("\nCaveats:");
            println!("  {}", caveats);
        }
    }

    // Print dependencies if available
    if let Some(dependencies) = formula.get("dependencies").and_then(|d| d.as_array()) {
        if !dependencies.is_empty() {
            println!("\nDependencies:");
            for dep in dependencies {
                if let Some(dep_name) = dep.as_str() {
                    println!("  - {}", dep_name);
                }
            }
        }
    }

    // Optionally print other metadata like bottle information, installation options, etc.
    // TODO: Print bottle information when needed
}

/// Prints cask information in a human-readable format
fn print_cask_info(name: &str, cask: &Value) {
    println!("=== Cask: {} ===", name);

    // Print basic information
    if let Some(name) = cask
        .get("name")
        .and_then(|n| n.as_array())
        .and_then(|a| a.first())
        .and_then(|s| s.as_str())
    {
        println!("Name: {}", name);
    }

    if let Some(desc) = cask.get("desc").and_then(|d| d.as_str()) {
        println!("Description: {}", desc);
    }

    if let Some(homepage) = cask.get("homepage").and_then(|h| h.as_str()) {
        println!("Homepage: {}", homepage);
    }

    if let Some(version) = cask.get("version").and_then(|v| v.as_str()) {
        println!("Version: {}", version);
    }

    // Print download URL
    if let Some(url) = cask.get("url").and_then(|u| u.as_str()) {
        println!("Download URL: {}", url);
    }

    // Print installation instructions
    println!("\nInstallation:");
    println!("  brew install --cask {}", name);
}
