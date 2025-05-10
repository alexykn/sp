use std::sync::Arc;

use clap::Args;
use colored::Colorize;
use prettytable::{format, Cell, Row, Table};
use serde_json::Value;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::Result;
use sps_net::api;
use terminal_size::{terminal_size, Width};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Args, Debug)]
pub struct Search {
    pub query: String,
    #[arg(long, conflicts_with = "cask")]
    pub formula: bool,
    #[arg(long, conflicts_with = "formula")]
    pub cask: bool,
}

pub enum SearchType {
    All,
    Formula,
    Cask,
}

impl Search {
    pub async fn run(&self, config: &Config, cache: Arc<Cache>) -> Result<()> {
        let search_type = if self.formula {
            SearchType::Formula
        } else if self.cask {
            SearchType::Cask
        } else {
            SearchType::All
        };
        run_search(&self.query, search_type, config, cache).await
    }
}

pub async fn run_search(
    query: &str,
    search_type: SearchType,
    _config: &Config,
    cache: Arc<Cache>,
) -> Result<()> {
    tracing::debug!("Searching for packages matching: {}", query);

    println!("Searching for \"{query}\"");

    let mut formula_matches = Vec::new();
    let mut cask_matches = Vec::new();
    let mut formula_err = None;
    let mut cask_err = None;

    if matches!(search_type, SearchType::All | SearchType::Formula) {
        match search_formulas(Arc::clone(&cache), query).await {
            Ok(matches) => formula_matches = matches,
            Err(e) => {
                tracing::error!("Error searching formulas: {}", e);
                formula_err = Some(e);
            }
        }
    }

    if matches!(search_type, SearchType::All | SearchType::Cask) {
        match search_casks(Arc::clone(&cache), query).await {
            Ok(matches) => cask_matches = matches,
            Err(e) => {
                tracing::error!("Error searching casks: {}", e);
                cask_err = Some(e);
            }
        }
    }

    if formula_matches.is_empty() && cask_matches.is_empty() {
        if let Some(e) = formula_err.or(cask_err) {
            return Err(e);
        }
    }

    print_search_results(query, &formula_matches, &cask_matches);

    Ok(())
}

async fn search_formulas(cache: Arc<Cache>, query: &str) -> Result<Vec<Value>> {
    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();
    let mut data_source_name = "cache";

    let formula_data_result = cache.load_raw("formula.json");

    let formulas: Vec<Value> = match formula_data_result {
        Ok(formula_data) => serde_json::from_str(&formula_data)?,
        Err(e) => {
            tracing::debug!("Formula cache load failed ({}), fetching from API", e);
            data_source_name = "API";
            let all_formulas = api::fetch_all_formulas().await?;

            if let Err(cache_err) = cache.store_raw("formula.json", &all_formulas) {
                tracing::warn!("Failed to cache formula data after fetching: {}", cache_err);
            }
            serde_json::from_str(&all_formulas)?
        }
    };

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

async fn search_casks(cache: Arc<Cache>, query: &str) -> Result<Vec<Value>> {
    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();
    let mut data_source_name = "cache";

    let cask_data_result = cache.load_raw("cask.json");

    let casks: Vec<Value> = match cask_data_result {
        Ok(cask_data) => serde_json::from_str(&cask_data)?,
        Err(e) => {
            tracing::debug!("Cask cache load failed ({}), fetching from API", e);
            data_source_name = "API";
            let all_casks = api::fetch_all_casks().await?;

            if let Err(cache_err) = cache.store_raw("cask.json", &all_casks) {
                tracing::warn!("Failed to cache cask data after fetching: {}", cache_err);
            }
            serde_json::from_str(&all_casks)?
        }
    };

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

fn is_formula_match(formula: &Value, query: &str) -> bool {
    if let Some(name) = formula.get("name").and_then(|n| n.as_str()) {
        if name.to_lowercase().contains(query) {
            return true;
        }
    }

    if let Some(full_name) = formula.get("full_name").and_then(|n| n.as_str()) {
        if full_name.to_lowercase().contains(query) {
            return true;
        }
    }

    if let Some(desc) = formula.get("desc").and_then(|d| d.as_str()) {
        if desc.to_lowercase().contains(query) {
            return true;
        }
    }

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

fn is_cask_match(cask: &Value, query: &str) -> bool {
    if let Some(token) = cask.get("token").and_then(|t| t.as_str()) {
        if token.to_lowercase().contains(query) {
            return true;
        }
    }

    if let Some(names) = cask.get("name").and_then(|n| n.as_array()) {
        for name in names {
            if let Some(name_str) = name.as_str() {
                if name_str.to_lowercase().contains(query) {
                    return true;
                }
            }
        }
    }

    if let Some(desc) = cask.get("desc").and_then(|d| d.as_str()) {
        if desc.to_lowercase().contains(query) {
            return true;
        }
    }

    false
}

fn truncate_vis(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    let mut w = 0;
    let mut out = String::new();
    let effective_max = if max > 0 { max } else { 1 };

    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw >= effective_max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

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

    let term_cols = terminal_size()
        .map(|(Width(w), _)| w as usize)
        .unwrap_or(120);

    let type_col = 7;
    let version_col = 10;
    let sep_width = 3 * 3;
    let total_fixed = type_col + version_col + sep_width;

    let name_min_width = 10;
    let desc_min_width = 20;

    let leftover = term_cols.saturating_sub(total_fixed);

    let name_prop_width = leftover / 3;

    let name_max = std::cmp::max(name_min_width, name_prop_width);
    let desc_max = std::cmp::max(desc_min_width, leftover.saturating_sub(name_max));

    let name_max = std::cmp::min(name_max, leftover.saturating_sub(desc_min_width));
    let desc_max = std::cmp::min(desc_max, leftover.saturating_sub(name_max));

    let mut tbl = Table::new();
    tbl.set_format(*format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);

    for formula in formula_matches {
        let raw_name = formula
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("Unknown");
        let raw_desc = formula.get("desc").and_then(|d| d.as_str()).unwrap_or("");
        let _name = truncate_vis(raw_name, name_max);
        let desc = truncate_vis(raw_desc, desc_max);

        let version = get_version(formula);

        tbl.add_row(Row::new(vec![
            Cell::new("Formula").style_spec("Fg"),
            Cell::new(&_name).style_spec("Fb"),
            Cell::new(version),
            Cell::new(&desc),
        ]));
    }

    if !formula_matches.is_empty() && !cask_matches.is_empty() {
        tbl.add_row(Row::new(vec![Cell::new(" ").with_hspan(4)]));
    }

    for cask in cask_matches {
        let raw_name = cask
            .get("token")
            .and_then(|t| t.as_str())
            .unwrap_or("Unknown");
        let raw_desc = cask.get("desc").and_then(|d| d.as_str()).unwrap_or("");
        let desc = truncate_vis(raw_desc, desc_max);

        let version = get_cask_version(cask);

        tbl.add_row(Row::new(vec![
            Cell::new("Cask").style_spec("Fy"),
            Cell::new(raw_name).style_spec("Fb"),
            Cell::new(version),
            Cell::new(&desc),
        ]));
    }

    tbl.printstd();
}

fn get_version(formula: &Value) -> &str {
    formula
        .get("versions")
        .and_then(|v| v.get("stable"))
        .and_then(|v| v.as_str())
        .unwrap_or("-")
}

fn get_cask_version(cask: &Value) -> &str {
    cask.get("version").and_then(|v| v.as_str()).unwrap_or("-")
}
