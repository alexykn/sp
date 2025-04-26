use std::sync::Arc;

use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::Client;
use serde_json::Value;
use tracing::{debug, error};

use crate::model::cask::{Cask, CaskList};
use crate::model::formula::Formula;
use crate::utils::config::Config;
use crate::utils::error::{Result, SpError};

const FORMULAE_API_BASE_URL: &str = "https://formulae.brew.sh/api";
const GITHUB_API_BASE_URL: &str = "https://api.github.com";
const USER_AGENT_STRING: &str = "sp Package Manager (Rust; +https://github.com/your/sp)";

fn build_api_client(config: &Config) -> Result<Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(USER_AGENT, USER_AGENT_STRING.parse().unwrap());
    headers.insert(ACCEPT, "application/vnd.github+json".parse().unwrap());
    if let Some(token) = &config.github_api_token {
        debug!("Adding GitHub API token to request headers.");
        match format!("Bearer {token}").parse() {
            Ok(val) => {
                headers.insert(AUTHORIZATION, val);
            }
            Err(e) => {
                error!("Failed to parse GitHub API token into header value: {}", e);
            }
        }
    } else {
        debug!("No GitHub API token found in config.");
    }
    Ok(Client::builder().default_headers(headers).build()?)
}

pub async fn fetch_raw_formulae_json(endpoint: &str) -> Result<String> {
    let url = format!("{FORMULAE_API_BASE_URL}/{endpoint}");
    debug!("Fetching data from Homebrew Formulae API: {}", url);
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT_STRING)
        .build()?;
    let response = client.get(&url).send().await.map_err(|e| {
        error!("HTTP request failed for {}: {}", url, e);
        SpError::Http(Arc::new(e))
    })?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|e| format!("(Failed to read response body: {e})"));
        debug!(
            "HTTP request to {} returned non-success status: {}",
            url, status
        );
        debug!("Response body for failed request to {}: {}", url, body);
        return Err(SpError::Api(format!("HTTP status {status} from {url}")));
    }
    let body = response.text().await?;
    if body.trim().is_empty() {
        error!("Response body for {} was empty.", url);
        return Err(SpError::Api(format!(
            "Empty response body received from {url}"
        )));
    }
    Ok(body)
}

pub async fn fetch_all_formulas() -> Result<String> {
    fetch_raw_formulae_json("formula.json").await
}

pub async fn fetch_all_casks() -> Result<String> {
    fetch_raw_formulae_json("cask.json").await
}

pub async fn fetch_formula(name: &str) -> Result<serde_json::Value> {
    let direct_fetch_result = fetch_raw_formulae_json(&format!("formula/{name}.json")).await;
    if let Ok(body) = direct_fetch_result {
        let formula: serde_json::Value = serde_json::from_str(&body)?;
        Ok(formula)
    } else {
        debug!(
            "Direct fetch for formula '{}' failed ({:?}). Fetching full list as fallback.",
            name,
            direct_fetch_result.err()
        );
        let all_formulas_body = fetch_all_formulas().await?;
        let formulas: Vec<serde_json::Value> = serde_json::from_str(&all_formulas_body)?;
        for formula in formulas {
            if formula.get("name").and_then(Value::as_str) == Some(name) {
                return Ok(formula);
            }
            if formula.get("full_name").and_then(Value::as_str) == Some(name) {
                return Ok(formula);
            }
        }
        Err(SpError::NotFound(format!(
            "Formula '{name}' not found in API list"
        )))
    }
}

pub async fn fetch_cask(token: &str) -> Result<serde_json::Value> {
    let direct_fetch_result = fetch_raw_formulae_json(&format!("cask/{token}.json")).await;
    if let Ok(body) = direct_fetch_result {
        let cask: serde_json::Value = serde_json::from_str(&body)?;
        Ok(cask)
    } else {
        debug!(
            "Direct fetch for cask '{}' failed ({:?}). Fetching full list as fallback.",
            token,
            direct_fetch_result.err()
        );
        let all_casks_body = fetch_all_casks().await?;
        let casks: Vec<serde_json::Value> = serde_json::from_str(&all_casks_body)?;
        for cask in casks {
            if cask.get("token").and_then(Value::as_str) == Some(token) {
                return Ok(cask);
            }
        }
        Err(SpError::NotFound(format!(
            "Cask '{token}' not found in API list"
        )))
    }
}

async fn fetch_github_api_json(endpoint: &str, config: &Config) -> Result<Value> {
    let url = format!("{GITHUB_API_BASE_URL}{endpoint}");
    debug!("Fetching data from GitHub API: {}", url);
    let client = build_api_client(config)?;
    let response = client.get(&url).send().await.map_err(|e| {
        error!("GitHub API request failed for {}: {}", url, e);
        SpError::Http(Arc::new(e))
    })?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|e| format!("(Failed to read response body: {e})"));
        error!(
            "GitHub API request to {} returned non-success status: {}",
            url, status
        );
        debug!(
            "Response body for failed GitHub API request to {}: {}",
            url, body
        );
        return Err(SpError::Api(format!("HTTP status {status} from {url}")));
    }
    let value: Value = response.json::<Value>().await.map_err(|e| {
        error!("Failed to parse JSON response from {}: {}", url, e);
        SpError::ApiRequestError(e.to_string())
    })?;
    Ok(value)
}

#[allow(dead_code)]
async fn fetch_github_repo_info(owner: &str, repo: &str, config: &Config) -> Result<Value> {
    let endpoint = format!("/repos/{owner}/{repo}");
    fetch_github_api_json(&endpoint, config).await
}

pub async fn get_formula(name: &str) -> Result<Formula> {
    let url = format!("{FORMULAE_API_BASE_URL}/formula/{name}.json");
    debug!(
        "Fetching and parsing formula data for '{}' from {}",
        name, url
    );
    let client = reqwest::Client::new();
    let response = client.get(&url).send().await.map_err(|e| {
        error!("HTTP request failed when fetching formula {}: {}", name, e);
        SpError::Http(Arc::new(e))
    })?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        error!("Failed to fetch formula {} (Status {})", name, status);
        debug!("Response body for failed formula fetch {}: {}", name, text);
        return Err(SpError::Api(format!(
            "Failed to fetch formula {name}: Status {status}"
        )));
    }
    if text.trim().is_empty() {
        error!("Received empty body when fetching formula {}", name);
        return Err(SpError::Api(format!(
            "Empty response body for formula {name}"
        )));
    }
    match serde_json::from_str::<Formula>(&text) {
        Ok(formula) => Ok(formula),
        Err(_) => {
            match serde_json::from_str::<Vec<Formula>>(&text) {
                Ok(mut formulas) if !formulas.is_empty() => {
                    debug!(
                        "Parsed formula {} from a single-element array response.",
                        name
                    );
                    Ok(formulas.remove(0))
                }
                Ok(_) => {
                    error!("Received empty array when fetching formula {}", name);
                    Err(SpError::NotFound(format!(
                        "Formula '{name}' not found (empty array returned)"
                    )))
                }
                Err(e_vec) => {
                    error!("Failed to parse formula {} as object or array. Error: {}. Body (sample): {}", name, e_vec, text.chars().take(500).collect::<String>());
                    Err(SpError::Json(Arc::new(e_vec)))
                }
            }
        }
    }
}

pub async fn get_all_formulas() -> Result<Vec<Formula>> {
    let raw_data = fetch_all_formulas().await?;
    serde_json::from_str(&raw_data).map_err(|e| {
        error!("Failed to parse all_formulas response: {}", e);
        SpError::Json(Arc::new(e))
    })
}

pub async fn get_cask(name: &str) -> Result<Cask> {
    let raw_json_result = fetch_cask(name).await;
    let raw_json = match raw_json_result {
        Ok(json_val) => json_val,
        Err(e) => {
            error!("Failed to fetch raw JSON for cask {}: {}", name, e);
            return Err(e);
        }
    };
    match serde_json::from_value::<Cask>(raw_json.clone()) {
        Ok(cask) => Ok(cask),
        Err(e) => {
            error!("Failed to parse cask {} JSON: {}", name, e);
            match serde_json::to_string_pretty(&raw_json) {
                Ok(json_str) => {
                    tracing::debug!("Problematic JSON for cask '{}':\n{}", name, json_str);
                }
                Err(fmt_err) => {
                    tracing::debug!(
                        "Could not pretty-print problematic JSON for cask {}: {}",
                        name,
                        fmt_err
                    );
                    tracing::debug!("Raw problematic value: {:?}", raw_json);
                }
            }
            Err(SpError::Json(Arc::new(e)))
        }
    }
}

pub async fn get_all_casks() -> Result<CaskList> {
    let raw_data = fetch_all_casks().await?;
    let casks: Vec<Cask> = serde_json::from_str(&raw_data).map_err(|e| {
        error!("Failed to parse all_casks response: {}", e);
        SpError::Json(Arc::new(e))
    })?;
    Ok(CaskList { casks })
}
