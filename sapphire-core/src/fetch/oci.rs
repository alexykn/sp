// File: sapphire-core/src/fetch/oci.rs

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

use futures::StreamExt;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use reqwest::header::{ACCEPT, AUTHORIZATION};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use tracing::{debug, error};
use url::Url;

use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
// ────────────────────────────────────────────────────────────────────────────────

const OCI_MANIFEST_V1_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const OCI_LAYER_V1_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const DEFAULT_GHCR_TOKEN_ENDPOINT: &str = "https://ghcr.io/token";
pub const DEFAULT_GHCR_DOMAIN: &str = "ghcr.io";

const CONNECT_TIMEOUT_SECS: u64 = 30;
const REQUEST_TIMEOUT_SECS: u64 = 300;
const USER_AGENT_STRING: &str =
    "Sapphire Package Manager (Rust; +https://github.com/your/sapphire)";

#[derive(Deserialize, Debug)]
struct OciTokenResponse {
    token: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OciManifestIndex {
    pub schema_version: u32,
    pub media_type: Option<String>,
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
    #[serde(default)]
    pub features: Vec<String>,
    pub variant: Option<String>,
}

#[derive(Debug, Clone)]
enum OciAuth {
    None,
    AnonymousBearer { token: String },
    ExplicitBearer { token: String },
    Basic { encoded: String },
}

async fn fetch_oci_resource<T: serde::de::DeserializeOwned>(
    resource_url: &str,
    accept_header: &str,
    config: &Config,
    client: &Client,
) -> Result<T> {
    let url = Url::parse(resource_url)
        .map_err(|e| SapphireError::Generic(format!("Invalid URL '{resource_url}': {e}")))?;
    let registry_domain = url.host_str().unwrap_or(DEFAULT_GHCR_DOMAIN);
    let repo_path = extract_repo_path_from_url(&url).unwrap_or("");

    let auth = determine_auth(config, client, registry_domain, repo_path).await?;
    let resp = execute_oci_request(client, resource_url, accept_header, &auth).await?;
    let txt = resp.text().await.map_err(SapphireError::Http)?;

    debug!("OCI response ({} bytes) from {}", txt.len(), resource_url);
    serde_json::from_str(&txt).map_err(|e| {
        error!("JSON parse error from {}: {}", resource_url, e);
        SapphireError::Json(e)
    })
}

pub async fn download_oci_blob(
    blob_url: &str,
    destination_path: &Path,
    config: &Config,
    client: &Client,
) -> Result<()> {
    debug!("Downloading OCI blob: {}", blob_url);
    let url = Url::parse(blob_url)
        .map_err(|e| SapphireError::Generic(format!("Invalid URL '{blob_url}': {e}")))?;
    let registry_domain = url.host_str().unwrap_or(DEFAULT_GHCR_DOMAIN);
    let repo_path = extract_repo_path_from_url(&url).unwrap_or("");

    let auth = determine_auth(config, client, registry_domain, repo_path).await?;
    let resp = execute_oci_request(client, blob_url, OCI_LAYER_V1_TYPE, &auth).await?;

    // Write to a temporary file, then rename
    let tmp = destination_path.with_file_name(format!(
        ".{}.download",
        destination_path.file_name().unwrap().to_string_lossy()
    ));
    let mut out = File::create(&tmp).map_err(SapphireError::Io)?;

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let b = chunk.map_err(SapphireError::Http)?;
        std::io::Write::write_all(&mut out, &b).map_err(SapphireError::Io)?;
    }
    std::fs::rename(&tmp, destination_path).map_err(SapphireError::Io)?;

    debug!("Blob saved to {}", destination_path.display());
    Ok(())
}

pub async fn fetch_oci_manifest_index(
    manifest_url: &str,
    config: &Config,
    client: &Client,
) -> Result<OciManifestIndex> {
    fetch_oci_resource(manifest_url, OCI_MANIFEST_V1_TYPE, config, client).await
}

pub fn build_oci_client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT_STRING)
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::default())
        .build()
        .map_err(SapphireError::Http)
}

fn extract_repo_path_from_url(url: &Url) -> Option<&str> {
    url.path()
        .trim_start_matches('/')
        .trim_start_matches("v2/")
        .split("/manifests/")
        .next()
        .and_then(|s| s.split("/blobs/").next())
        .filter(|s| !s.is_empty())
}

async fn determine_auth(
    config: &Config,
    client: &Client,
    registry_domain: &str,
    repo_path: &str,
) -> Result<OciAuth> {
    if let Some(token) = &config.docker_registry_token {
        debug!("Using explicit bearer for {}", registry_domain);
        return Ok(OciAuth::ExplicitBearer {
            token: token.clone(),
        });
    }
    if let Some(basic) = &config.docker_registry_basic_auth {
        debug!("Using explicit basic auth for {}", registry_domain);
        return Ok(OciAuth::Basic {
            encoded: basic.clone(),
        });
    }

    if registry_domain.eq_ignore_ascii_case(DEFAULT_GHCR_DOMAIN) && !repo_path.is_empty() {
        debug!(
            "Anonymous token fetch for {} scope={}",
            registry_domain, repo_path
        );
        match fetch_anonymous_token(client, registry_domain, repo_path).await {
            Ok(t) => return Ok(OciAuth::AnonymousBearer { token: t }),
            Err(e) => debug!("Anon token failed, proceeding unauthenticated: {}", e),
        }
    }
    Ok(OciAuth::None)
}

async fn fetch_anonymous_token(
    client: &Client,
    registry_domain: &str,
    repo_path: &str,
) -> Result<String> {
    let endpoint = if registry_domain.eq_ignore_ascii_case(DEFAULT_GHCR_DOMAIN) {
        DEFAULT_GHCR_TOKEN_ENDPOINT.to_string()
    } else {
        format!("https://{registry_domain}/token")
    };
    let scope = format!("repository:{repo_path}:pull");
    let token_url = format!("{endpoint}?service={registry_domain}&scope={scope}");

    const MAX_RETRIES: u8 = 3;
    let base_delay = Duration::from_millis(200);
    let mut delay = base_delay;
    // Use a Sendable RNG
    let mut rng = SmallRng::from_os_rng();

    for attempt in 0..=MAX_RETRIES {
        debug!(
            "Token attempt {}/{} from {}",
            attempt + 1,
            MAX_RETRIES + 1,
            token_url
        );

        match client.get(&token_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let tok: OciTokenResponse = resp.json().await.map_err(|e| {
                    SapphireError::ApiRequestError(format!("Parse token response: {e}"))
                })?;
                return Ok(tok.token);
            }
            Ok(resp) => {
                let code = resp.status();
                let body = resp.text().await.unwrap_or_default();
                error!("Token fetch {}: {} – {}", attempt + 1, code, body);
                if !code.is_server_error() || attempt == MAX_RETRIES {
                    return Err(SapphireError::Api(format!("Token endpoint {code}: {body}")));
                }
            }
            Err(e) => {
                error!("Network error on token fetch {}: {}", attempt + 1, e);
                if attempt == MAX_RETRIES {
                    return Err(SapphireError::Http(e));
                }
            }
        }

        // back‑off with jitter
        let jitter = rng.random_range(0..(base_delay.as_millis() as u64 / 2));
        tokio::time::sleep(delay + Duration::from_millis(jitter)).await;
        delay *= 2;
    }

    Err(SapphireError::Api(format!(
        "Failed to fetch OCI token after {} attempts",
        MAX_RETRIES + 1
    )))
}

async fn execute_oci_request(
    client: &Client,
    url: &str,
    accept: &str,
    auth: &OciAuth,
) -> Result<Response> {
    debug!("OCI request → {} (Accept: {})", url, accept);
    let mut req = client.get(url).header(ACCEPT, accept);
    match auth {
        OciAuth::AnonymousBearer { token } | OciAuth::ExplicitBearer { token }
            if !token.is_empty() =>
        {
            req = req.header(AUTHORIZATION, format!("Bearer {token}"))
        }
        OciAuth::Basic { encoded } if !encoded.is_empty() => {
            req = req.header(AUTHORIZATION, format!("Basic {encoded}"))
        }
        _ => {}
    }

    let resp = req.send().await.map_err(SapphireError::Http)?;
    let status = resp.status();
    if status.is_success() {
        Ok(resp)
    } else {
        let body = resp.text().await.unwrap_or_default();
        error!("OCI {} ⇒ {} – {}", url, status, body);
        let err = match status {
            StatusCode::UNAUTHORIZED => SapphireError::Api(format!("Auth required: {status}")),
            StatusCode::FORBIDDEN => SapphireError::Api(format!("Permission denied: {status}")),
            StatusCode::NOT_FOUND => SapphireError::NotFound(format!("Not found: {status}")),
            _ => SapphireError::Api(format!("HTTP {status} – {body}")),
        };
        Err(err)
    }
}
