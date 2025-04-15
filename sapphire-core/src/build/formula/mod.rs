// src/build/formula/mod.rs

use crate::model::formula::Formula;
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use log::{warn, error, debug, info};
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

// Declare submodules
pub mod bottle;
pub mod link;
pub mod macho;
pub mod source;

/// Download formula resources from the internet asynchronously.
pub async fn download_formula(
    formula: &Formula,
    config: &Config,
    client: &reqwest::Client,
) -> Result<PathBuf> {
    if has_bottle_for_current_platform(formula) {
        bottle::download_bottle(formula, config, client).await
    } else {
         info!("No suitable bottle found for {} on this platform, downloading source.", formula.name());
        source::download_source(formula, config).await
    }
}

/// Checks if a suitable bottle exists for the current platform, considering fallbacks.
pub fn has_bottle_for_current_platform(formula: &Formula) -> bool {
    let result = crate::build::formula::bottle::get_bottle_for_platform(formula);
     debug!("has_bottle_for_current_platform check for '{}': {:?}", formula.name(), result.is_ok());
     if let Err(e) = &result {
         debug!("Reason for no bottle: {}", e);
     }
    result.is_ok()
}

// *** Updated get_current_platform function ***
fn get_current_platform() -> String {
    if cfg!(target_os = "macos") {
        let arch = if std::env::consts::ARCH == "aarch64" { "arm64" }
                   else if std::env::consts::ARCH == "x86_64" { "x86_64" }
                   else { std::env::consts::ARCH };

        debug!("Attempting to determine macOS version using /usr/bin/sw_vers -productVersion...");
        // *** Use only -productVersion argument ***
        match Command::new("/usr/bin/sw_vers").arg("-productVersion").output() {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                debug!("sw_vers status: {}", output.status);
                // Log stdout/stderr if exit wasn't clean or stderr has content
                 if !output.status.success() || !stderr.trim().is_empty() {
                    debug!("sw_vers stdout:\n{}", stdout);
                    if !stderr.trim().is_empty() { warn!("sw_vers stderr:\n{}", stderr); }
                }

                if output.status.success() {
                    // *** Parse the single line output ***
                    let version_str = stdout.trim(); // Get the single line
                    if !version_str.is_empty() {
                        debug!("Extracted version string: {}", version_str);
                        let os_name = match version_str.split('.').next() {
                            Some("15") => "sequoia",
                            Some("14") => "sonoma",
                            Some("13") => "ventura",
                            Some("12") => "monterey",
                            Some("11") => "big_sur",
                            Some("10") => match version_str.split('.').nth(1) {
                                Some("15") => "catalina",
                                Some("14") => "mojave",
                                _ => { warn!("Unrecognized legacy macOS 10.x version: {}", version_str); "unknown_macos" }
                            },
                            _ => { warn!("Unrecognized macOS major version: {}", version_str); "unknown_macos" }
                        };

                        if os_name != "unknown_macos" {
                            let platform_tag = if arch == "arm64" {
                                format!("{}_{}", arch, os_name)
                            } else {
                                // Use OS name only for Intel tags by convention
                                os_name.to_string()
                            };
                            info!("Determined platform tag: {}", platform_tag);
                            return platform_tag;
                        }
                    } else {
                         error!("sw_vers -productVersion output was empty.");
                    }
                } else {
                    error!("sw_vers -productVersion command failed with status: {}. Stderr: {}", output.status, stderr.trim());
                }
            }
            Err(e) => {
                error!("Failed to execute /usr/bin/sw_vers -productVersion: {}", e);
            }
        }

        // Fallback Logic
        error!("!!! FAILED TO DETECT MACOS VERSION VIA SW_VERS !!!");
        warn!("Using UNRELIABLE fallback platform detection. Bottle selection may be incorrect.");
        if arch == "arm64" {
            warn!("Falling back to platform tag: arm64_monterey");
            return "arm64_monterey".to_string();
        } else {
             warn!("Falling back to platform tag: monterey");
            return "monterey".to_string();
        }

    } else if cfg!(target_os = "linux") {
        if std::env::consts::ARCH == "aarch64" { "arm64_linux".to_string() }
        else if std::env::consts::ARCH == "x86_64" { "x86_64_linux".to_string() }
        else { "unknown".to_string() } // Handle other linux arches if needed
    } else {
         warn!("Could not determine platform tag for OS: {}", std::env::consts::OS);
        "unknown".to_string()
    }
}


// --- extract_archive and helpers (unchanged) ---
pub fn extract_archive(archive_path: &Path, target_dir: &Path) -> Result<()> {
    // (Implementation remains the same)
    fs::create_dir_all(target_dir)?;
    let extension = archive_path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    match extension {
        "tar" => extract_tar(archive_path, target_dir),
        "gz" | "tgz" => extract_tar_gz(archive_path, target_dir),
        "bz2" | "tbz" | "tbz2" => extract_tar_bz2(archive_path, target_dir),
        "xz" | "txz" => extract_tar_xz(archive_path, target_dir),
        "zip" => extract_zip(archive_path, target_dir),
        _ => Err(SapphireError::Generic(format!("Unsupported archive format: {}", extension))),
    }
}
fn extract_tar(archive_path: &Path, target_dir: &Path) -> Result<()> {
    // (Implementation remains the same)
    let output = Command::new("tar").arg("-xf").arg(archive_path).arg("-C").arg(target_dir).output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!("Failed to extract tar archive: {}", String::from_utf8_lossy(&output.stderr))));
    }
    Ok(())
}
fn extract_tar_gz(archive_path: &Path, target_dir: &Path) -> Result<()> {
    // (Implementation remains the same)
     let output = Command::new("tar").arg("-xzf").arg(archive_path).arg("-C").arg(target_dir).output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!("Failed to extract tar.gz archive: {}", String::from_utf8_lossy(&output.stderr))));
    }
    Ok(())
}
fn extract_tar_bz2(archive_path: &Path, target_dir: &Path) -> Result<()> {
    // (Implementation remains the same)
      let output = Command::new("tar").arg("-xjf").arg(archive_path).arg("-C").arg(target_dir).output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!("Failed to extract tar.bz2 archive: {}", String::from_utf8_lossy(&output.stderr))));
    }
    Ok(())
}
fn extract_tar_xz(archive_path: &Path, target_dir: &Path) -> Result<()> {
    // (Implementation remains the same)
      let output = Command::new("tar").arg("-xJf").arg(archive_path).arg("-C").arg(target_dir).output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!("Failed to extract tar.xz archive: {}", String::from_utf8_lossy(&output.stderr))));
    }
    Ok(())
}
fn extract_zip(archive_path: &Path, target_dir: &Path) -> Result<()> {
    // (Implementation remains the same)
     let output = Command::new("unzip").arg("-qq").arg("-o").arg(archive_path).arg("-d").arg(target_dir).output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!("Failed to extract zip archive: {}", String::from_utf8_lossy(&output.stderr))));
    }
    Ok(())
}

// --- get_cellar_path (unchanged) ---
pub fn get_cellar_path() -> PathBuf {
    // (Implementation remains the same)
    if std::env::consts::ARCH == "aarch64" { PathBuf::from("/opt/homebrew/Cellar") }
    else { PathBuf::from("/usr/local/Cellar") }
}

// --- get_formula_cellar_path (unchanged) ---
pub fn get_formula_cellar_path(formula: &Formula) -> PathBuf {
    // (Implementation remains the same)
    let cellar = get_cellar_path();
    let version_string = formula.version_str_full();
    cellar.join(&formula.name).join(version_string)
}

// --- write_receipt (unchanged) ---
pub fn write_receipt(formula: &Formula, install_dir: &Path) -> Result<()> {
    // (Implementation remains the same)
     let receipt_dir = install_dir.join("INSTALL_RECEIPT.json");
    let receipt_file = File::create(&receipt_dir);
     let mut receipt_file = match receipt_file {
         Ok(file) => file,
         Err(e) => {
             error!("Failed to create receipt file at {}: {}", receipt_dir.display(), e);
             return Err(SapphireError::Io(e));
         }
     };

    let resources_result = formula.resources();
    let resources_installed = match resources_result {
        Ok(res) => res.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
        Err(_) => {
            warn!("Could not retrieve resources for formula {} when writing receipt.", formula.name);
            vec![]
        }
    };

    // Assuming chrono crate is added to Cargo.toml
    let timestamp = chrono::Utc::now().to_rfc3339();

    let receipt = serde_json::json!({
        "name": formula.name, "version": formula.version_str_full(), "time": timestamp,
        "source": { "type": "api", "url": formula.url, },
        "built_on": {
            "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "platform_tag": get_current_platform(),
         },
        "resources_installed": resources_installed,
    });

     let receipt_json = match serde_json::to_string_pretty(&receipt) {
         Ok(json) => json,
         Err(e) => {
             error!("Failed to serialize receipt JSON for {}: {}", formula.name, e);
             return Err(SapphireError::Json(e));
         }
     };

     if let Err(e) = receipt_file.write_all(receipt_json.as_bytes()) {
         error!("Failed to write receipt file for {}: {}", formula.name, e);
         return Err(SapphireError::Io(e));
     }

    Ok(())
}

// --- Re-exports (unchanged) ---
pub use bottle::install_bottle;
pub use link::link_formula_artifacts;
pub use source::{build_from_source, download_source};