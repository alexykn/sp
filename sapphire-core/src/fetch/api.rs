// **File:** sapphire-core/src/fetch/api.rs

use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT}; // Import headers
use reqwest::Client;
use serde_json::Value;
use tracing::{debug, error};

//use serde::de::DeserializeOwned; // Import DeserializeOwned - might be used later
use crate::model::cask::{Cask, CaskList};
use crate::model::formula::Formula;
use crate::utils::config::Config; // Import Config
use crate::utils::error::{Result, SapphireError}; // Use log crate

/// Base URL for the Homebrew API (formulae.brew.sh)
const FORMULAE_API_BASE_URL: &str = "https://formulae.brew.sh/api";
/// Base URL for the GitHub API (api.github.com)
const GITHUB_API_BASE_URL: &str = "https://api.github.com";
const USER_AGENT_STRING: &str =
    "Sapphire Package Manager (Rust; +https://github.com/your/sapphire)"; // Replace with your repo link

/// Builds a reqwest client, potentially adding GitHub API token.
fn build_api_client(config: &Config) -> Result<Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(USER_AGENT, USER_AGENT_STRING.parse().unwrap());
    headers.insert(ACCEPT, "application/vnd.github+json".parse().unwrap()); // Standard Accept for GitHub API

    // Add GitHub API token if present in config
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
        // Older APIs might use "token <PAT>"
        // match format!("token {}", token).parse() { ... }
    } else {
        debug!("No GitHub API token found in config.");
    }

    Client::builder()
        .default_headers(headers)
        .build()
        .map_err(SapphireError::Http)
}

// --- Functions targeting formulae.brew.sh (remain largely unchanged, use default client) ---

/// Fetch raw JSON data from the Homebrew Formulae API (formulae.brew.sh).
/// This does *not* typically require GitHub API token authentication.
pub async fn fetch_raw_formulae_json(endpoint: &str) -> Result<String> {
    let url = format!("{FORMULAE_API_BASE_URL}/{endpoint}");
    debug!("Fetching data from Homebrew Formulae API: {}", url);

    // Use a default client for formulae.brew.sh, usually no auth needed
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT_STRING)
        .build()
        .map_err(SapphireError::Http)?;

    let response = client.get(&url).send().await.map_err(|e| {
        error!("HTTP request failed for {}: {}", url, e);
        SapphireError::Http(e)
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|e| format!("(Failed to read response body: {e})"));
        error!(
            "HTTP request to {} returned non-success status: {}. Body: {}",
            url, status, body
        );
        return Err(SapphireError::Api(format!(
            "HTTP status {status} from {url}. Response body: {body}"
        )));
    }

    let body = response.text().await.map_err(SapphireError::Http)?;

    if body.trim().is_empty() {
        error!("Response body for {} was empty.", url);
        // Consider returning an error or warning based on endpoint expectations
        // For formula.json/cask.json, empty is an error.
        return Err(SapphireError::Api(format!(
            "Empty response body received from {url}"
        )));
    }
    Ok(body)
}

/// Fetch all formulas from the Homebrew Formulae API.
pub async fn fetch_all_formulas() -> Result<String> {
    fetch_raw_formulae_json("formula.json").await
}

/// Fetch all casks from the Homebrew Formulae API.
pub async fn fetch_all_casks() -> Result<String> {
    fetch_raw_formulae_json("cask.json").await
}

/// Fetch a specific formula by name from the Homebrew Formulae API.
pub async fn fetch_formula(name: &str) -> Result<serde_json::Value> {
    let direct_fetch_result = fetch_raw_formulae_json(&format!("formula/{name}.json")).await;

    if let Ok(body) = direct_fetch_result {
        let formula: serde_json::Value =
            serde_json::from_str(&body).map_err(SapphireError::Json)?;
        Ok(formula)
    } else {
        // Fallback might be less useful if the single endpoint fails, but keep for now
        debug!(
            "Direct fetch for formula '{}' failed. Fetching full list as fallback.",
            name
        );
        let all_formulas_body = fetch_all_formulas().await?;
        let formulas: Vec<serde_json::Value> = serde_json::from_str(&all_formulas_body)?;
        for formula in formulas {
            if formula.get("name").and_then(Value::as_str) == Some(name) {
                return Ok(formula);
            }
            // Add check for full_name or aliases if needed
            if formula.get("full_name").and_then(Value::as_str) == Some(name) {
                return Ok(formula);
            }
        }
        Err(SapphireError::NotFound(format!(
            "Formula '{name}' not found in API list"
        )))
    }
}

/// Fetch a specific cask by token from the Homebrew Formulae API.
pub async fn fetch_cask(token: &str) -> Result<serde_json::Value> {
    let direct_fetch_result = fetch_raw_formulae_json(&format!("cask/{token}.json")).await;

    if let Ok(body) = direct_fetch_result {
        let cask: serde_json::Value = serde_json::from_str(&body).map_err(SapphireError::Json)?;
        Ok(cask)
    } else {
        debug!(
            "Direct fetch for cask '{}' failed. Fetching full list as fallback.",
            token
        );
        let all_casks_body = fetch_all_casks().await?;
        let casks: Vec<serde_json::Value> = serde_json::from_str(&all_casks_body)?;
        for cask in casks {
            if cask.get("token").and_then(Value::as_str) == Some(token) {
                return Ok(cask);
            }
            // Check aliases or names if needed
        }
        Err(SapphireError::NotFound(format!(
            "Cask '{token}' not found in API list"
        )))
    }
}

// --- Functions targeting GitHub API (api.github.com) ---
// These *should* use the client with the GitHub API token if available.

/// Fetches JSON data from a specified GitHub API endpoint.
/// Uses the client configured with HOMEBREW_GITHUB_API_TOKEN if available.
async fn fetch_github_api_json(endpoint: &str, config: &Config) -> Result<Value> {
    let url = format!("{GITHUB_API_BASE_URL}{endpoint}"); // Endpoint should start with /
    debug!("Fetching data from GitHub API: {}", url);
    let client = build_api_client(config)?; // Build client with potential auth token

    let response = client.get(&url).send().await.map_err(|e| {
        error!("GitHub API request failed for {}: {}", url, e);
        SapphireError::Http(e)
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|e| format!("(Failed to read response body: {e})"));
        error!(
            "GitHub API request to {} returned non-success status: {}. Body: {}",
            url, status, body
        );
        return Err(SapphireError::Api(format!(
            "HTTP status {status} from {url}. Response body: {body}"
        )));
    }

    let value: Value = response.json::<Value>().await.map_err(|e| {
        error!("Failed to parse JSON response from {}: {}", url, e);
        SapphireError::ApiRequestError(e.to_string())
    })?;

    Ok(value)
}

/// Example: Fetches repository details from GitHub API.
/// (Adapt or add functions as needed for specific GitHub interactions)
#[allow(dead_code)]
async fn fetch_github_repo_info(owner: &str, repo: &str, config: &Config) -> Result<Value> {
    let endpoint = format!("/repos/{owner}/{repo}");
    fetch_github_api_json(&endpoint, config).await
}

// --- Convenience functions combining API fetches and parsing ---
// These generally target formulae.brew.sh, so they don't need the GitHub token client.

/// Get data for a specific formula, parsed into the Formula struct.
pub async fn get_formula(name: &str) -> Result<Formula> {
    let url = format!("{FORMULAE_API_BASE_URL}/formula/{name}.json");
    debug!(
        "Fetching and parsing formula data for '{}' from {}",
        name, url
    );

    let client = reqwest::Client::new(); // Default client
    let response_result = client.get(&url).send().await;

    match response_result {
        Ok(response) => {
            let status = response.status();
            let text = response
                .text()
                .await
                .unwrap_or_else(|e| format!("(Failed to read body: {e})"));

            if !status.is_success() {
                error!(
                    "Failed to fetch formula {} (Status {}): {}",
                    name, status, text
                );
                return Err(SapphireError::Api(format!(
                    "Failed to fetch formula {name}: Status {status}"
                )));
            }
            if text.trim().is_empty() {
                error!("Received empty body when fetching formula {}", name);
                return Err(SapphireError::Api(format!(
                    "Empty response body for formula {name}"
                )));
            }

            // Homebrew API sometimes returns an array with one element
            match serde_json::from_str::<Formula>(&text) {
                Ok(formula) => Ok(formula), // Parsed as single object
                Err(_) => {
                    // Try parsing as Vec<Formula>
                    match serde_json::from_str::<Vec<Formula>>(&text) {
                        Ok(mut formulas) if !formulas.is_empty() => {
                            debug!(
                                "Parsed formula {} from a single-element array response.",
                                name
                            );
                            Ok(formulas.remove(0))
                        }
                        Ok(_) => {
                            // Empty array
                            error!("Received empty array when fetching formula {}", name);
                            Err(SapphireError::NotFound(format!(
                                "Formula '{name}' not found (empty array returned)"
                            )))
                        }
                        Err(e_vec) => {
                            // Failed to parse as array too
                            error!("Failed to parse formula {} as object or array. Error: {}. Body: {}", name, e_vec, text);
                            Err(SapphireError::Json(e_vec))
                        }
                    }
                }
            }
        }
        Err(e) => {
            error!("HTTP request failed when fetching formula {}: {}", name, e);
            Err(SapphireError::Http(e))
        }
    }
}

/// Get data for all formulas, parsed into Vec<Formula>.
pub async fn get_all_formulas() -> Result<Vec<Formula>> {
    let raw_data = fetch_all_formulas().await?;
    serde_json::from_str(&raw_data).map_err(|e| {
        error!("Failed to parse all_formulas response: {}", e);
        SapphireError::Json(e)
    })
}

/// Get data for a specific cask, parsed into the Cask struct.
pub async fn get_cask(name: &str) -> Result<Cask> {
    // Fetch the raw JSON value first
    let raw_json_result = fetch_cask(name).await; // This returns Result<serde_json::Value>

    // Handle potential errors during the raw fetch itself
    let raw_json = match raw_json_result {
        Ok(json_val) => json_val,
        Err(e) => {
            // Log fetch error and return immediately
            error!("Failed to fetch raw JSON for cask {}: {}", name, e);
            return Err(e); // Return the fetch error
        }
    };

    // Now, attempt to deserialize the raw JSON into the Cask struct
    match serde_json::from_value::<Cask>(raw_json.clone()) {
        // Clone raw_json in case we need to print it
        Ok(cask) => Ok(cask), // Deserialization successful
        Err(e) => {
            // Deserialization failed! Log the error and print the JSON.
            error!("Failed to parse cask {} JSON: {}", name, e); // Log the serde error

            // Attempt to pretty-print the JSON that caused the error
            match serde_json::to_string_pretty(&raw_json) {
                Ok(json_str) => {
                    // Print to stderr so it's visible even if logging is redirected
                    eprintln!("\n--- Problematic JSON for cask '{name}' ---");
                    eprintln!("{json_str}");
                    eprintln!("--- End of JSON ---");
                    // Also log it for persistence
                    tracing::error!("Problematic JSON for cask '{}':\n{}", name, json_str);
                }
                Err(fmt_err) => {
                    // Fallback if pretty-printing fails (less likely)
                    eprintln!("\n--- Problematic JSON (raw debug) for cask '{name}' ---");
                    eprintln!("{raw_json:?}"); // Print debug format
                    eprintln!("--- End of JSON ---");
                    tracing::error!(
                        "Could not pretty-print problematic JSON for cask {}: {}",
                        name,
                        fmt_err
                    );
                    tracing::error!("Raw problematic value: {:?}", raw_json);
                }
            }
            // Important: Return the original deserialization error
            Err(SapphireError::Json(e))
        }
    }
}

/// Get data for all casks, parsed into CaskList.
pub async fn get_all_casks() -> Result<CaskList> {
    let raw_data = fetch_all_casks().await?; // Fetches from formulae.brew.sh
                                             // The cask.json endpoint returns an array, not an object with a "casks" key.
    let casks: Vec<Cask> = serde_json::from_str(&raw_data).map_err(|e| {
        error!("Failed to parse all_casks response: {}", e);
        SapphireError::Json(e)
    })?;
    Ok(CaskList { casks }) // Wrap the Vec<Cask> in our CaskList struct
}

// --- Generic fetch_and_parse removed as specific implementations are preferred ---
// async fn fetch_and_parse_json<T: DeserializeOwned>(url: &str, client: &Client) -> Result<T> { ...
// }
