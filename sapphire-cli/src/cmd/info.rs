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
use std::sync::Arc; // <-- ADDED

/// Displays detailed information about a formula or cask.
pub async fn run_info(
    name: &str,
    is_cask: bool,
    _config: &Config,
    cache: &Arc<Cache>
) -> Result<()> {
    log::debug!("Getting info for package: {}, is_cask: {}", name, is_cask);
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::with_template("{spinner:.magenta} {msg}").unwrap());
    pb.set_message(format!("Loading info for {}", name));
    pb.enable_steady_tick(Duration::from_millis(100));

    if is_cask {
        match get_cask_info(Arc::clone(cache), name).await {
            Ok(info) => {
                pb.finish_and_clear();
                print_cask_info(name, &info);
                return Ok(());
            }
            Err(e) => {
                return Err(e);
            }
        }
    } else {
        match get_formula_info_raw(Arc::clone(cache), name).await {
            Ok(info) => {
                if is_bottle_available(&info) {
                    pb.finish_and_clear();
                    print_formula_info(name, &info);
                    return Ok(());
                }
                log::debug!("Formula '{}' found but no bottle available, checking casks.", name);
            }
            Err(_e) => { /* proceed to check cask */ }
        }
        match get_cask_info(Arc::clone(cache), name).await {
            Ok(info) => {
                pb.finish_and_clear();
                print_cask_info(name, &info);
                return Ok(());
            }
            Err(e) => {
                pb.finish_and_clear();
                return Err(e);
            }
        }
    }
}

/// Public function that retrieves formula information and returns the Formula model
pub async fn get_formula_info(name: &str, _config: &Config, cache: &Arc<Cache>) -> Result<Formula> {
    let raw_info = get_formula_info_raw(Arc::clone(cache), name).await?;
    // Replace map_err closure with direct conversion since SapphireError implements From<serde_json::Error>
    let formula: Formula = serde_json::from_value(raw_info)
        .map_err(SapphireError::Json)?;
    Ok(formula)
}

/// Retrieves formula information from the cache or API as raw JSON
async fn get_formula_info_raw(cache: Arc<Cache>, name: &str) -> Result<Value> {
    match cache.load_raw("formula.json") {
        Ok(formula_data) => {
            let formulas: Vec<Value> = serde_json::from_str(&formula_data)
                .map_err(SapphireError::from)?;
            for formula in formulas {
                if let Some(fname) = formula.get("name").and_then(Value::as_str) {
                    if fname == name {
                        return Ok(formula);
                    }
                }
            }
            log::debug!("Formula '{}' not found within cached 'formula.json'.", name);
        }
        Err(e) => log::debug!("Cache file 'formula.json' not found or failed to load ({}).", e)
    }
    log::debug!("Fetching formula '{}' directly from API...", name);
    let value = api::fetch_formula(name).await?;
    Ok(value)
}

/// Retrieves cask information from the cache or API
async fn get_cask_info(cache: Arc<Cache>, name: &str) -> Result<Value> {
    match cache.load_raw("cask.json") {
        Ok(cask_data) => {
            let casks: Vec<Value> = serde_json::from_str(&cask_data)
                .map_err(SapphireError::from)?;
            for cask in casks {
                if let Some(token) = cask.get("token").and_then(Value::as_str) {
                    if token == name {
                        return Ok(cask);
                    }
                }
            }
            log::debug!("Cask '{}' not found within cached 'cask.json'.", name);
        }
        Err(e) => log::debug!("Cache file 'cask.json' not found or failed to load ({}).", e)
    }
    log::debug!("Fetching cask '{}' directly from API...", name);
    let value = api::fetch_cask(name).await?;
    Ok(value)
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
