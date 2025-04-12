// utils/http.rs - HTTP utility functions

use crate::Result;
use reqwest::Client;
use std::path::Path;
use std::fs::File;
use std::io::copy;

/// Download a file from a URL to a local path
pub async fn download_file(url: &str, destination: &Path) -> Result<()> {
    let client = Client::new();
    let response = client.get(url)
        .send()
        .await?;

    let mut file = File::create(destination)?;
    let mut content = response.bytes().await?;
    copy(&mut content.as_ref(), &mut file)?;

    Ok(())
}

/// Fetch the content of a URL as a string
pub async fn fetch_string(url: &str) -> Result<String> {
    let client = Client::new();
    let response = client.get(url)
        .send()
        .await?;

    let body = response.text().await?;
    Ok(body)
}

/// Fetch and parse JSON from a URL
pub async fn fetch_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    let client = Client::new();
    let response = client.get(url)
        .send()
        .await?;

    let json = response.json::<T>().await?;
    Ok(json)
}
