// **File:** sapphire-core/src/fetch/oci.rs (Corrected with Config access for retry)
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use reqwest::header::{ACCEPT, AUTHORIZATION};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
//use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
//use std::io::copy;
use std::path::Path;
use std::time::Duration;
use url::Url;
use log::{debug, error, info, warn};
use async_recursion::async_recursion; // Ensure this is in Cargo.toml
use futures::StreamExt; // For processing response stream

const OCI_MANIFEST_V1_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const OCI_LAYER_V1_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const DEFAULT_GHCR_TOKEN_ENDPOINT: &str = "https://ghcr.io/token";
pub const DEFAULT_GHCR_DOMAIN: &str = "ghcr.io";
const CONNECT_TIMEOUT_SECS: u64 = 30;
const REQUEST_TIMEOUT_SECS: u64 = 300;
const USER_AGENT_STRING: &str = "Sapphire Package Manager (Rust; +https://github.com/your/sapphire)"; // Replace with your repo link

#[derive(Deserialize, Debug)]
struct OciTokenResponse {
    token: String,
    // access_token: Option<String>, // Some registries use access_token
    // expires_in: Option<u64>,
    // issued_at: Option<String>,
}

// Simplified OCI Manifest Index structure
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OciManifestIndex {
    pub schema_version: u32,
    pub media_type: Option<String>, // Optional as it might be implied by header
    pub manifests: Vec<OciManifestDescriptor>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OciManifestDescriptor {
    pub media_type: String,
    pub digest: String,
    pub size: u64,
    pub platform: Option<OciPlatform>,
    pub annotations: Option<HashMap<String, String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OciPlatform {
    pub architecture: String,
    pub os: String,
    #[serde(rename = "os.version")]
    pub os_version: Option<String>,
    #[serde(default)] // Use default Vec::new() if missing
    pub features: Vec<String>,
    pub variant: Option<String>,
}

/// Represents authentication details for OCI registry access.
#[derive(Debug, Clone)]
enum OciAuth {
    None, // For registries that truly allow anonymous access (rare)
    AnonymousBearer { token: String },
    ExplicitBearer { token: String },
    Basic { encoded: String },
}

/// Fetches an OCI resource (manifest or blob), handling authentication.
async fn fetch_oci_resource<T: serde::de::DeserializeOwned>(
    resource_url: &str,
    accept_header: &str,
    config: &Config, // Pass config
    client: &Client,
) -> Result<T> {
    let url = Url::parse(resource_url)
        .map_err(|e| SapphireError::Generic(format!("Invalid OCI resource URL '{}': {}", resource_url, e)))?;
    let registry_domain = url.host_str().unwrap_or(DEFAULT_GHCR_DOMAIN);
    let repo_path = extract_repo_path_from_url(&url).unwrap_or("");

    // Pass config to determine_auth
    let auth = determine_auth(config, client, registry_domain, repo_path).await?;

    // Pass config to execute_oci_request
    let response = execute_oci_request(config, client, resource_url, accept_header, &auth, 1).await?; // Allow 1 retry on 401

    let response_text = response.text()
        .await
        .map_err(|e| SapphireError::Http(e))?;

    debug!("OCI Resource Response Body ({}): [Body Length: {}]", resource_url, response_text.len());

    serde_json::from_str::<T>(&response_text)
        .map_err(|e| {
            error!("Failed to parse OCI resource from {}: {}", resource_url, e);
             let snippet_len = response_text.chars().take(200).collect::<String>().len();
             error!("Body Snippet (first {} chars): {}", snippet_len, response_text.chars().take(200).collect::<String>());
            SapphireError::Json(e)
        })
}

/// Downloads an OCI blob, handling authentication.
pub async fn download_oci_blob(
    blob_url: &str,
    destination_path: &Path,
    config: &Config, // Pass config
    client: &Client,
) -> Result<()> {
    info!("Attempting to download OCI blob: {}", blob_url);
    let url = Url::parse(blob_url)
        .map_err(|e| SapphireError::Generic(format!("Invalid OCI blob URL '{}': {}", blob_url, e)))?;
    let registry_domain = url.host_str().unwrap_or(DEFAULT_GHCR_DOMAIN);
    let repo_path = extract_repo_path_from_url(&url).unwrap_or("");

    // Pass config to determine_auth
    let auth = determine_auth(config, client, registry_domain, repo_path).await?;

    // Pass config to execute_oci_request
    let response = execute_oci_request(config, client, blob_url, OCI_LAYER_V1_TYPE, &auth, 1).await?; // Allow 1 retry

    // Write the downloaded content to the destination file
    let temp_filename = format!(
        ".{}.download",
        destination_path.file_name().unwrap_or_default().to_string_lossy()
    );
    let temp_path = destination_path.with_file_name(temp_filename);

    debug!("Downloading blob to temporary path: {}", temp_path.display());
    if temp_path.exists() {
        if let Err(e) = std::fs::remove_file(&temp_path) {
            warn!("Could not remove existing temporary blob file {}: {}", temp_path.display(), e);
        }
    }

    { // Scope for file lock
        let mut dest_file = File::create(&temp_path).map_err(|e| {
            SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to create temp blob file {}: {}", temp_path.display(), e),
            ))
        })?;

        let mut response_stream = response.bytes_stream();
        while let Some(chunk_result) = StreamExt::next(&mut response_stream).await {
             match chunk_result {
                 Ok(chunk) => {
                    std::io::Write::write_all(&mut dest_file, &chunk).map_err(|e| {
                         SapphireError::Io(std::io::Error::new(
                             e.kind(),
                             format!("Failed to write blob chunk to {}: {}", temp_path.display(), e),
                         ))
                     })?;
                 }
                 Err(e) => {
                     error!("Error reading blob download stream: {}", e);
                      drop(dest_file);
                      let _ = std::fs::remove_file(&temp_path);
                     return Err(SapphireError::Http(e));
                 }
             }
        }
        debug!("Finished writing blob stream to temp file.");
    } // File is closed here

    std::fs::rename(&temp_path, destination_path).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to move temp blob file {} to {}: {}", temp_path.display(), destination_path.display(), e),
        ))
    })?;

    info!("Successfully downloaded blob to {}", destination_path.display());
    Ok(())
}


/// Fetches and parses an OCI manifest index.
pub async fn fetch_oci_manifest_index(
    manifest_url: &str,
    config: &Config, // Pass config
    client: &Client,
) -> Result<OciManifestIndex> {
    info!("Fetching OCI Manifest Index from: {}", manifest_url);
    // Pass config to fetch_oci_resource
    fetch_oci_resource(manifest_url, OCI_MANIFEST_V1_TYPE, config, client).await
}

/// Builds the base HTTP client.
pub fn build_oci_client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT_STRING)
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::default())
        .build()
        .map_err(|e| SapphireError::Http(e))
}

// --- Internal Helper Functions ---

/// Extracts the repository path (e.g., homebrew/core/wget) from an OCI URL.
fn extract_repo_path_from_url(url: &Url) -> Option<&str> {
     url.path()
        .trim_start_matches('/')
        .trim_start_matches("v2/")
        .split("/manifests/") // Split at manifests first
        .next()
        .and_then(|s| s.split("/blobs/").next()) // Then split the result at blobs
        .filter(|s| !s.is_empty()) // Return None if the result is empty
}


/// Determines the appropriate authentication method based on config and registry requirements.
async fn determine_auth(
    config: &Config,
    client: &Client,
    registry_domain: &str,
    repo_path: &str,
) -> Result<OciAuth> {
    // 1. Check for explicit bearer token
    if let Some(token) = &config.docker_registry_token {
        info!("Using explicit bearer token from HOMEBREW_DOCKER_REGISTRY_TOKEN for {}", registry_domain);
        return Ok(OciAuth::ExplicitBearer { token: token.clone() });
    }

    // 2. Check for explicit basic auth
    if let Some(encoded_auth) = &config.docker_registry_basic_auth {
        info!("Using explicit basic auth from HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN for {}", registry_domain);
        return Ok(OciAuth::Basic { encoded: encoded_auth.clone() });
    }

    // 3. Attempt anonymous token fetch (only for known domains or if configured)
    if registry_domain.eq_ignore_ascii_case(DEFAULT_GHCR_DOMAIN) {
         if repo_path.is_empty() {
             warn!("Cannot determine repository scope for anonymous token fetch for domain {}. Proceeding without auth.", registry_domain);
             return Ok(OciAuth::None);
         }
        info!("No explicit token found, attempting anonymous token fetch for scope '{}' at {}", repo_path, registry_domain);
        match fetch_anonymous_token(client, registry_domain, repo_path).await {
            Ok(token) => Ok(OciAuth::AnonymousBearer { token }),
            Err(e) => {
                warn!("Failed to fetch anonymous token for {}: {}. Proceeding without auth (may fail).", registry_domain, e);
                Ok(OciAuth::None)
            }
        }
    } else {
        info!("Registry {} is not default ghcr.io and no explicit token provided. Proceeding without auth.", registry_domain);
        Ok(OciAuth::None)
    }
}

/// Fetches an anonymous bearer token from the registry's token endpoint.
async fn fetch_anonymous_token(
    client: &Client,
    registry_domain: &str,
    repo_path: &str, // e.g., "homebrew/core/wget"
) -> Result<String> {
    let token_endpoint = if registry_domain.eq_ignore_ascii_case(DEFAULT_GHCR_DOMAIN) {
        DEFAULT_GHCR_TOKEN_ENDPOINT.to_string()
    } else {
        format!("https://{}/token", registry_domain)
    };

    let scope = format!("repository:{}:pull", repo_path);
    let token_url_str = format!(
        "{}?service={}&scope={}",
        token_endpoint, registry_domain, scope
    );
    info!("Fetching anonymous token from: {}", token_url_str);

    let response = client.get(&token_url_str)
        .send()
        .await
        .map_err(|e| SapphireError::Http(e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_else(|_| "Failed to read body".to_string());
        error!("Failed to fetch anonymous token: HTTP Status {} - {}", status, body);
        return Err(SapphireError::Api(format!(
            "Failed to get OCI token ({}): Status {}",
            token_url_str, status
        )));
    }

    match response.json::<OciTokenResponse>().await {
         Ok(token_response) => {
             debug!("Successfully obtained anonymous OCI token.");
             Ok(token_response.token)
         }
         Err(e) => {
             error!("Failed to parse OCI token response: {}", e);
             Err(SapphireError::ApiRequestError(format!("Failed to parse OCI token response: {}", e)))
         }
    }
}


/// Executes an OCI request with appropriate headers and handles retries on 401 for anonymous tokens.
/// Uses async recursion to handle retries cleanly.
#[async_recursion]
async fn execute_oci_request(
    config: &Config, // Accept config
    client: &Client,
    url: &str,
    accept_header: &str,
    auth: &OciAuth, // Use the initially determined auth for the *first* attempt
    retries_left: u8,
) -> Result<Response> {
    debug!("Executing OCI request to: {}", url);
    debug!("  Accept: {}", accept_header);
    match auth {
         OciAuth::None => debug!("  Auth method: None"),
         OciAuth::AnonymousBearer{..} => debug!("  Auth method: AnonymousBearer"),
         OciAuth::ExplicitBearer{..} => debug!("  Auth method: ExplicitBearer"),
         OciAuth::Basic{..} => debug!("  Auth method: Basic"),
    }

    let mut request_builder = client.get(url);
    request_builder = request_builder.header(ACCEPT, accept_header);

    // Apply authentication header based on the passed 'auth' argument
    match auth {
        OciAuth::AnonymousBearer { token } | OciAuth::ExplicitBearer { token } => {
            request_builder = request_builder.header(AUTHORIZATION, format!("Bearer {}", token));
        }
        OciAuth::Basic { encoded } => {
            request_builder = request_builder.header(AUTHORIZATION, format!("Basic {}", encoded));
        }
        OciAuth::None => { /* No header added */ }
    }

    let response = request_builder.send().await.map_err(|e| {
         error!("OCI request to {} failed: {}", url, e);
         SapphireError::Http(e)
    })?;

    let status = response.status();
    debug!("Received status {} for {}", status, url);

    if status.is_success() {
        Ok(response)
    } else if status == StatusCode::UNAUTHORIZED && matches!(auth, OciAuth::AnonymousBearer {..}) && retries_left > 0 {
        // --- Retry Logic with Token Refresh ---
        warn!("Received 401 Unauthorized for {} with anonymous token. Attempting token refresh ({} retries left).", url, retries_left - 1);

        // Re-determine auth, which requires config and client access to fetch a *new* token.
        let url_obj = match Url::parse(url) {
            Ok(obj) => obj,
            Err(e) => return Err(SapphireError::Generic(format!("Failed to parse URL '{}': {}", url, e))),
        };
        let registry_domain = url_obj.host_str().unwrap_or(DEFAULT_GHCR_DOMAIN);
        let repo_path = extract_repo_path_from_url(&url_obj).unwrap_or("");

        // Call determine_auth again with the passed config to get fresh auth details
        let refreshed_auth = match determine_auth(config, client, registry_domain, repo_path).await {
            Ok(new_auth) => {
                 // Check if we actually got a new anonymous token, or if it fell back to None/Errored
                 if matches!(new_auth, OciAuth::AnonymousBearer{..}) {
                      info!("Successfully refreshed anonymous token for retry.");
                      new_auth
                 } else {
                     warn!("Token refresh attempt did not result in a new anonymous token (got {:?}). Retrying with original auth details.", new_auth);
                      // Fallback to retrying with the original auth details if refresh failed
                      auth.clone()
                 }
            }
            Err(e) => {
                 warn!("Error during token refresh attempt: {}. Retrying with original auth details.", e);
                 // Fallback to retrying with the original auth details on error
                 auth.clone()
            }
        };

        // Recursively call with the (potentially) refreshed auth details and decremented retry count
        execute_oci_request(config, client, url, accept_header, &refreshed_auth, retries_left - 1).await

    } else {
        // Handle other errors (403, 404, 5xx, or 401 with explicit token/no retries)
        let error_body = response.text().await.unwrap_or_else(|_| "Failed to read error response body".to_string());
        error!("OCI request failed for {}: Status {} - {}", url, status, error_body);
        Err(SapphireError::Api(format!(
            "OCI request failed for {}: Status {} - {}", url, status, error_body
        )))
    }
}

