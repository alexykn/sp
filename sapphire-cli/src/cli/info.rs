//! Contains the logic for the `info` command.

use std::sync::Arc;

use clap::Args;
use colored::Colorize;
use sapphire_core::fetch::api;
use sapphire_core::model::formula::Formula;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::{Result, SapphireError};
use serde_json::Value;

use crate::ui;

#[derive(Args, Debug)]
pub struct Info {
    /// Name of the formula or cask
    pub name: String,

    /// Show information for a cask, not a formula
    #[arg(long)]
    pub cask: bool,
}

impl Info {
    /// Displays detailed information about a formula or cask.
    pub async fn run(&self, _config: &Config, cache: Arc<Cache>) -> Result<()> {
        let name = &self.name;
        let is_cask = self.cask;
        tracing::debug!("Getting info for package: {name}, is_cask: {is_cask}",);

        // Use the ui utility function to create the spinner
        let pb = ui::create_spinner(&format!("Loading info for {}", name)); // <-- CHANGED

        if self.cask {
            match get_cask_info(Arc::clone(&cache), name).await {
                Ok(info) => {
                    pb.finish_and_clear();
                    print_cask_info(name, &info);
                    return Ok(());
                }
                Err(e) => {
                    pb.finish_and_clear(); // Ensure spinner is cleared on error
                    return Err(e);
                }
            }
        } else {
            match get_formula_info_raw(Arc::clone(&cache), name).await {
                Ok(info) => {
                    // Removed bottle check logic here as it was complex and potentially racy.
                    // We'll try formula first, then cask if formula fails.
                    pb.finish_and_clear(); // Clear spinner after successful fetch
                    print_formula_info(name, &info);
                    return Ok(());
                }
                Err(SapphireError::NotFound(_)) | Err(SapphireError::Generic(_)) => {
                    // If formula lookup failed (not found or generic error), try cask.
                    tracing::debug!("Formula '{}' info failed, trying cask.", name);
                }
                Err(e) => {
                    pb.finish_and_clear(); // Ensure spinner is cleared on other errors
                    return Err(e); // Propagate other errors (API, JSON, etc.)
                }
            }
            // --- Cask Fallback ---
            match get_cask_info(Arc::clone(&cache), name).await {
                Ok(info) => {
                    pb.finish_and_clear();
                    print_cask_info(name, &info);
                    return Ok(());
                }
                Err(e) => {
                    pb.finish_and_clear(); // Clear spinner on cask error too
                    return Err(e); // Return the cask error if both formula and cask fail
                }
            }
        }
    }
}

/// Public function that retrieves formula information and returns the Formula model
pub async fn get_formula_info(name: &str, _config: &Config, cache: Arc<Cache>) -> Result<Formula> {
    let raw_info = get_formula_info_raw(Arc::clone(&cache), name).await?;
    // Replace map_err closure with direct conversion since SapphireError implements
    // From<serde_json::Error>
    let formula: Formula = serde_json::from_value(raw_info).map_err(SapphireError::Json)?;
    Ok(formula)
}

/// Retrieves formula information from the cache or API as raw JSON
async fn get_formula_info_raw(cache: Arc<Cache>, name: &str) -> Result<Value> {
    match cache.load_raw("formula.json") {
        Ok(formula_data) => {
            let formulas: Vec<Value> =
                serde_json::from_str(&formula_data).map_err(SapphireError::from)?;
            for formula in formulas {
                if let Some(fname) = formula.get("name").and_then(Value::as_str) {
                    if fname == name {
                        return Ok(formula);
                    }
                }
                // Also check aliases if needed
                if let Some(aliases) = formula.get("aliases").and_then(|a| a.as_array()) {
                    if aliases.iter().any(|a| a.as_str() == Some(name)) {
                        return Ok(formula);
                    }
                }
            }
            tracing::debug!("Formula '{}' not found within cached 'formula.json'.", name);
            // Explicitly return NotFound if not in cache
            return Err(SapphireError::NotFound(format!(
                "Formula '{}' not found in cache",
                name
            )));
        }
        Err(e) => tracing::debug!(
            "Cache file 'formula.json' not found or failed to load ({}). Fetching from API.",
            e
        ),
    }
    tracing::debug!("Fetching formula '{}' directly from API...", name);
    // api::fetch_formula returns Value directly now
    let value = api::fetch_formula(name).await?;
    // Store in cache if fetched successfully
    // Note: This might overwrite the full list cache, consider storing individual files or a map
    // cache.store_raw(&format!("formula/{}.json", name), &value.to_string())?; // Example of
    // storing individually
    Ok(value)
}

/// Retrieves cask information from the cache or API
async fn get_cask_info(cache: Arc<Cache>, name: &str) -> Result<Value> {
    match cache.load_raw("cask.json") {
        Ok(cask_data) => {
            let casks: Vec<Value> =
                serde_json::from_str(&cask_data).map_err(SapphireError::from)?;
            for cask in casks {
                if let Some(token) = cask.get("token").and_then(Value::as_str) {
                    if token == name {
                        return Ok(cask);
                    }
                }
                // Check aliases if needed
                if let Some(aliases) = cask.get("aliases").and_then(|a| a.as_array()) {
                    if aliases.iter().any(|a| a.as_str() == Some(name)) {
                        return Ok(cask);
                    }
                }
            }
            tracing::debug!("Cask '{}' not found within cached 'cask.json'.", name);
            // Explicitly return NotFound if not in cache
            return Err(SapphireError::NotFound(format!(
                "Cask '{}' not found in cache",
                name
            )));
        }
        Err(e) => tracing::debug!(
            "Cache file 'cask.json' not found or failed to load ({}). Fetching from API.",
            e
        ),
    }
    tracing::debug!("Fetching cask '{}' directly from API...", name);
    // api::fetch_cask returns Value directly now
    let value = api::fetch_cask(name).await?;
    // Store in cache if fetched successfully
    // cache.store_raw(&format!("cask/{}.json", name), &value.to_string())?; // Example of storing
    // individually
    Ok(value)
}

/// Prints formula information in a formatted table
fn print_formula_info(_name: &str, formula: &Value) {
    // Basic info extraction
    let full_name = formula
        .get("full_name")
        .and_then(|f| f.as_str())
        .unwrap_or("N/A");
    let version = formula
        .get("versions")
        .and_then(|v| v.get("stable"))
        .and_then(|s| s.as_str())
        .unwrap_or("N/A");
    let revision = formula
        .get("revision")
        .and_then(|r| r.as_u64())
        .unwrap_or(0);
    let version_str = if revision > 0 {
        format!("{}_{}", version, revision)
    } else {
        version.to_string()
    };
    let license = formula
        .get("license")
        .and_then(|l| l.as_str())
        .unwrap_or("N/A");
    let homepage = formula
        .get("homepage")
        .and_then(|h| h.as_str())
        .unwrap_or("N/A");

    // Header
    println!("{}", format!("Formula: {}", full_name).green().bold());

    // Summary table
    let mut table = prettytable::Table::new();
    table.set_format(*prettytable::format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);
    table.add_row(prettytable::row!["Version", version_str]);
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

    // Combined Dependencies Section
    let mut dep_table = prettytable::Table::new();
    dep_table.set_format(*prettytable::format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);
    let mut has_deps = false;

    let mut add_deps = |title: &str, key: &str, tag: &str| {
        if let Some(deps) = formula.get(key).and_then(|d| d.as_array()) {
            let dep_list: Vec<&str> = deps.iter().filter_map(|d| d.as_str()).collect();
            if !dep_list.is_empty() {
                has_deps = true;
                for (i, d) in dep_list.iter().enumerate() {
                    let display_title = if i == 0 { title } else { "" };
                    let display_tag = if i == 0 {
                        format!("({})", tag)
                    } else {
                        "".to_string()
                    };
                    dep_table.add_row(prettytable::row![display_title, d, display_tag]);
                }
            }
        }
    };

    add_deps("Required", "dependencies", "runtime");
    add_deps(
        "Recommended",
        "recommended_dependencies",
        "runtime, recommended",
    );
    add_deps("Optional", "optional_dependencies", "runtime, optional");
    add_deps("Build", "build_dependencies", "build");
    add_deps("Test", "test_dependencies", "test");

    if has_deps {
        println!("\n{}", "Dependencies".blue().bold());
        dep_table.printstd();
    }

    // Installation hint
    println!("\n{}", "Installation".blue().bold());
    println!(
        "  {} install {}",
        "sapphire".cyan(),
        formula
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or(full_name) // Use short name if available
    );
}

/// Prints cask information in a formatted table
fn print_cask_info(name: &str, cask: &Value) {
    // Header
    println!("{}", format!("Cask: {}", name).green().bold());

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
    // Add SHA if present
    if let Some(sha) = cask.get("sha256").and_then(|s| s.as_str()) {
        if !sha.is_empty() {
            table.add_row(prettytable::row!["SHA256", sha]);
        }
    }
    table.printstd();

    // Dependencies Section
    if let Some(deps) = cask.get("depends_on").and_then(|d| d.as_object()) {
        let mut dep_table = prettytable::Table::new();
        dep_table.set_format(*prettytable::format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);
        let mut has_deps = false;

        if let Some(formulas) = deps.get("formula").and_then(|f| f.as_array()) {
            if !formulas.is_empty() {
                has_deps = true;
                dep_table.add_row(prettytable::row![
                    "Formula".yellow(),
                    formulas
                        .iter()
                        .map(|v| v.as_str().unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ]);
            }
        }
        if let Some(casks) = deps.get("cask").and_then(|c| c.as_array()) {
            if !casks.is_empty() {
                has_deps = true;
                dep_table.add_row(prettytable::row![
                    "Cask".yellow(),
                    casks
                        .iter()
                        .map(|v| v.as_str().unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ]);
            }
        }
        if let Some(macos) = deps.get("macos") {
            has_deps = true;
            let macos_str = match macos {
                Value::String(s) => s.clone(),
                Value::Array(arr) => arr
                    .iter()
                    .map(|v| v.as_str().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join(" or "),
                _ => "Unknown".to_string(),
            };
            dep_table.add_row(prettytable::row!["macOS".yellow(), macos_str]);
        }

        if has_deps {
            println!("\n{}", "Dependencies".blue().bold());
            dep_table.printstd();
        }
    }

    // Installation hint
    println!("\n{}", "Installation".blue().bold());
    println!(
        "  {} install --cask {}", // Always use --cask for clarity
        "sapphire".cyan(),
        name // Use the token 'name' passed to the function
    );
}
// Removed is_bottle_available check
