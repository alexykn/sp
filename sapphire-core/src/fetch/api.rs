// src/fetch/api.rs
// This module handles API interactions with the Homebrew API.

use crate::utils::error::{BrewRsError, Result};
use reqwest;
use serde_json::Value;
use reqwest::Client;
use crate::model::formula::Formula;
use crate::model::cask::{Cask, CaskList};

/// Homebrew API base URL
pub const API_BASE_URL: &str = "https://formulae.brew.sh/api";

/// Fetch raw JSON data from the Homebrew API
pub async fn fetch_raw_json(endpoint: &str) -> Result<String> {
    let url = format!("{}/{}", API_BASE_URL, endpoint);
    log::info!("Fetching data from {}", url);

    let client = reqwest::Client::new();
    let response = client.get(&url)
        .send()
        .await
        .map_err(|e| {
            log::error!("HTTP request failed: {}", e);
            BrewRsError::Http(e)
        })?;

    if !response.status().is_success() {
        log::error!("HTTP request returned non-success status: {}", response.status());
        return Err(BrewRsError::Api(format!("HTTP status: {}", response.status())));
    }

    let body = response.text().await.map_err(|e| {
        log::error!("Failed to read response body: {}", e);
        BrewRsError::Http(e)
    })?;

    Ok(body)
}

/// Fetch all formulas from the Homebrew API
pub async fn fetch_all_formulas() -> Result<String> {
    fetch_raw_json("formula.json").await
}

/// Fetch all casks from the Homebrew API
pub async fn fetch_all_casks() -> Result<String> {
    fetch_raw_json("cask.json").await
}

/// Fetch a formula by name
pub async fn fetch_formula(name: &str) -> Result<serde_json::Value> {
    // First, try to load the formula from the API directly
    let direct_fetch_result = fetch_raw_json(&format!("formula/{}.json", name)).await;

    if let Ok(body) = direct_fetch_result {
        // Successfully fetched directly
        let formula: serde_json::Value = serde_json::from_str(&body)?;
        return Ok(formula);
    } else {
        // Direct fetch failed, fetch all formulas and find the matching one
        let all_formulas_body = fetch_all_formulas().await?; // Get the String body
        let formulas: Vec<serde_json::Value> = serde_json::from_str(&all_formulas_body)?;

        for formula in formulas {
            if let Some(formula_name) = formula.get("name").and_then(|n| n.as_str()) {
                if formula_name == name {
                    return Ok(formula);
                }
            }
        }
        // If not found after checking all
        Err(BrewRsError::NotFound(format!("Formula '{}' not found", name)))
    }
}

/// Fetch a cask by token
pub async fn fetch_cask(token: &str) -> Result<serde_json::Value> {
    // First, try to load the cask from the API directly
    let direct_fetch_result = fetch_raw_json(&format!("cask/{}.json", token)).await;

    if let Ok(body) = direct_fetch_result {
        // Successfully fetched directly
        let cask: serde_json::Value = serde_json::from_str(&body)?;
        return Ok(cask);
    } else {
        // Direct fetch failed, fetch all casks and find the matching one
        let all_casks_body = fetch_all_casks().await?; // Get the String body
        let casks: Vec<serde_json::Value> = serde_json::from_str(&all_casks_body)?;

        for cask in casks {
            if let Some(cask_token) = cask.get("token").and_then(|t| t.as_str()) {
                if cask_token == token {
                    return Ok(cask);
                }
            }
        }
        // If not found after checking all
        Err(BrewRsError::NotFound(format!("Cask '{}' not found", token)))
    }
}

/// Get data for a specific formula
pub async fn get_formula(name: &str) -> Result<Formula> {
    let url = format!("{}/formula/{}.json", API_BASE_URL, name);
    fetch_and_parse_json(&url).await
}

/// Get data for all formulas
pub async fn get_all_formulas() -> Result<Vec<Formula>> {
    let url = format!("{}/formula.json", API_BASE_URL);
    let data: Vec<Formula> = fetch_and_parse_json(&url).await?;
    Ok(data)
}

/// Get data for a specific cask
pub async fn get_cask(name: &str) -> Result<Cask> {
    let url = format!("{}/cask/{}.json", API_BASE_URL, name);
    let response = fetch_raw_json(&url).await?;

    // Parse the response as a single Cask object
    let cask: Cask = serde_json::from_str(&response).map_err(|e| {
        log::error!("Failed to parse cask JSON: {}", e);
        BrewRsError::ParseError("Cask", e.to_string())
    })?;

    Ok(cask)
}

/// Get data for all casks
pub async fn get_all_casks() -> Result<CaskList> {
    let url = format!("{}/cask.json", API_BASE_URL);
    let data: CaskList = fetch_and_parse_json(&url).await?;
    Ok(data)
}

/// Fetch raw JSON from a URL
#[allow(dead_code)]
async fn fetch_json(url: &str) -> Result<Value> {
    let client = Client::new();
    let response = client.get(url)
        .send()
        .await
        .map_err(|e| BrewRsError::Http(e))?;

    if !response.status().is_success() {
        return Err(BrewRsError::Generic(format!(
            "Failed to fetch data: HTTP status {}", response.status()
        )));
    }

    let value = response.json::<Value>()
        .await
        .map_err(|e| BrewRsError::Http(e))?;

    Ok(value)
}

/// Fetch and parse JSON from a URL
async fn fetch_and_parse_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    let client = Client::new();
    let response = client.get(url)
        .send()
        .await
        .map_err(|e| BrewRsError::Http(e))?;

    if !response.status().is_success() {
        return Err(BrewRsError::Generic(format!(
            "Failed to fetch data: HTTP status {}", response.status()
        )));
    }

    let parsed = response.json::<T>()
        .await
        .map_err(|e| BrewRsError::Http(e))?;

    Ok(parsed)
}
