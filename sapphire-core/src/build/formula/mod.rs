// src/build/formula/mod.rs
// *** Confirmed download_formula is async and uses await ***

use crate::model::formula::Formula;
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use log::warn;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command; // Use log crate imports

pub mod bottle;
pub mod link;
pub mod source;

/// Download formula resources from the internet asynchronously.
pub async fn download_formula(
    // Stays async
    formula: &Formula,
    config: &Config,
    client: &reqwest::Client, // Pass async client
) -> Result<PathBuf> {
    if has_bottle_for_current_platform(formula) {
        // Await the async bottle download
        bottle::download_bottle(formula, config, client).await
    } else {
        // Await the async source download
        source::download_source(formula, config).await
    }
}

// has_bottle_for_current_platform remains synchronous
pub fn has_bottle_for_current_platform(formula: &Formula) -> bool {
    if let Some(stable) = &formula.bottle.stable {
        let platform = get_current_platform();
        !stable.files.is_empty() && stable.files.contains_key(&platform)
    } else {
        false
    }
}

// get_current_platform remains synchronous
fn get_current_platform() -> String {
    if cfg!(target_os = "macos") {
        let arch = if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            std::env::consts::ARCH
        };
        let output = Command::new("/usr/bin/sw_vers")
            .args(&["-productName", "-productVersion"])
            .output();
        if let Ok(output) = output {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let lines: Vec<&str> = stdout.lines().collect();
                if lines.len() >= 2 {
                    let version_str = lines[1];
                    let os_name = match version_str.split('.').next() {
                        Some("15") => "sequoia",
                        Some("14") => "sonoma",
                        Some("13") => "ventura",
                        Some("12") => "monterey",
                        Some("11") => "big_sur",
                        Some("10") => match version_str.split('.').nth(1) {
                            Some("15") => "catalina",
                            Some("14") => "mojave",
                            _ => "unknown_macos",
                        },
                        _ => "unknown_macos",
                    };
                    if arch == "arm64" {
                        return format!("{}_{}", arch, os_name);
                    } else {
                        return os_name.to_string();
                    }
                }
            } else {
                let stderr_msg = String::from_utf8_lossy(&output.stderr);
                warn!(
                    "Warning: '/usr/bin/sw_vers' command failed (status: {}). Stderr: {}",
                    output.status, stderr_msg
                );
            }
        } else if let Err(e) = output {
            warn!(
                "Warning: Failed to execute '/usr/bin/sw_vers'. Error: {}",
                e
            );
        }
        warn!("Warning: Using fallback macOS platform detection (sw_vers failed or output unparseable).");
        if arch == "arm64" {
            return "arm64_monterey".to_string();
        } else {
            return "monterey".to_string();
        }
    } else if cfg!(target_os = "linux") {
        if std::env::consts::ARCH == "aarch64" {
            return "arm64_linux".to_string();
        } else {
            return "x86_64_linux".to_string();
        }
    }
    "unknown".to_string()
}

// extract_archive and helpers remain synchronous
pub fn extract_archive(archive_path: &Path, target_dir: &Path) -> Result<()> {
    /* Sync implementation */
    fs::create_dir_all(target_dir)?;
    let extension = archive_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");
    match extension {
        "tar" => extract_tar(archive_path, target_dir),
        "gz" | "tgz" => extract_tar_gz(archive_path, target_dir),
        "bz2" | "tbz" | "tbz2" => extract_tar_bz2(archive_path, target_dir),
        "xz" | "txz" => extract_tar_xz(archive_path, target_dir),
        "zip" => extract_zip(archive_path, target_dir),
        _ => Err(SapphireError::Generic(format!(
            "Unsupported archive format: {}",
            extension
        ))),
    }
}
fn extract_tar(archive_path: &Path, target_dir: &Path) -> Result<()> {
    /* Sync implementation */
    let output = Command::new("tar")
        .arg("-xf")
        .arg(archive_path)
        .arg("-C")
        .arg(target_dir)
        .output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Failed to extract tar archive: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}
fn extract_tar_gz(archive_path: &Path, target_dir: &Path) -> Result<()> {
    /* Sync implementation */
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(target_dir)
        .output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Failed to extract tar.gz archive: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}
fn extract_tar_bz2(archive_path: &Path, target_dir: &Path) -> Result<()> {
    /* Sync implementation */
    let output = Command::new("tar")
        .arg("-xjf")
        .arg(archive_path)
        .arg("-C")
        .arg(target_dir)
        .output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Failed to extract tar.bz2 archive: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}
fn extract_tar_xz(archive_path: &Path, target_dir: &Path) -> Result<()> {
    /* Sync implementation */
    let output = Command::new("tar")
        .arg("-xJf")
        .arg(archive_path)
        .arg("-C")
        .arg(target_dir)
        .output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Failed to extract tar.xz archive: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}
fn extract_zip(archive_path: &Path, target_dir: &Path) -> Result<()> {
    /* Sync implementation */
    let output = Command::new("unzip")
        .arg("-qq")
        .arg("-o")
        .arg(archive_path)
        .arg("-d")
        .arg(target_dir)
        .output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Failed to extract zip archive: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

// get_cellar_path remains synchronous
pub fn get_cellar_path() -> PathBuf {
    if std::env::consts::ARCH == "aarch64" {
        PathBuf::from("/opt/homebrew/Cellar")
    } else {
        PathBuf::from("/usr/local/Cellar")
    }
}

// get_formula_cellar_path remains synchronous
pub fn get_formula_cellar_path(formula: &Formula) -> PathBuf {
    let cellar = get_cellar_path();
    let version_string = formula.version_str_full();
    cellar.join(&formula.name).join(version_string)
}

// write_receipt remains synchronous
pub fn write_receipt(formula: &Formula, install_dir: &Path) -> Result<()> {
    let receipt_dir = install_dir.join("INSTALL_RECEIPT.json");
    let mut receipt_file = File::create(receipt_dir)?;
    let resources_result = formula.resources(); // Handle potential error from resources()
    let resources_installed = match resources_result {
        Ok(res) => res.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
        Err(_) => {
            warn!(
                "Could not retrieve resources for formula {} when writing receipt.",
                formula.name
            );
            vec![] // Write empty list if resources fail
        }
    };

    let receipt = serde_json::json!({
        "name": formula.name, "version": formula.version_str_full(), "time": chrono::Utc::now().to_rfc3339(),
        "source": { "type": "api", "url": formula.url, },
        "built_on": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH, "platform_tag": get_current_platform(), },
        "resources_installed": resources_installed, // Include installed resource names
    });
    let receipt_json = serde_json::to_string_pretty(&receipt)?;
    receipt_file.write_all(receipt_json.as_bytes())?;
    Ok(())
}

// Re-exports remain the same
pub use bottle::install_bottle;
pub use link::link_formula_artifacts;
pub use source::{build_from_source, download_source};
