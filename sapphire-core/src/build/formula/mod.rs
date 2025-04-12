// src/build/formula/mod.rs
// Contains the logic for downloading and building formulae

use std::path::{Path, PathBuf};
use crate::utils::error::{SapphireError, Result};
use crate::model::formula::Formula;
use crate::utils::config::Config;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::process::Command;

pub mod bottle;
pub mod source;
pub mod link;

/// Download formula resources from the internet
pub async fn download_formula(formula: &Formula, config: &Config) -> Result<PathBuf> {
    // Determine if we should download a bottle or source
    if has_bottle_for_current_platform(formula) {
        bottle::download_bottle(formula, config).await
    } else {
        source::download_source(formula, config).await
    }
}

/// Check if a bottle is available for the current platform
pub fn has_bottle_for_current_platform(formula: &Formula) -> bool {
    if let Some(stable) = &formula.bottle.stable {
        let platform = get_current_platform();
        !stable.files.is_empty() && stable.files.contains_key(&platform)
    } else {
        false
    }
}

/// Get the current platform identifier used in bottle filenames
fn get_current_platform() -> String {
    // This should match homebrew's platform identifiers
    if cfg!(target_os = "macos") {
        let arch = if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            std::env::consts::ARCH // Keep x86_64 as is
        };

        // Use `sw_vers` to get the macOS version name
        // Use the full path to avoid PATH issues
        let output = Command::new("/usr/bin/sw_vers")
            .args(&["-productName", "-productVersion"])
            .output();

        if let Ok(output) = output {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Example stdout: "macOS\n14.5\n"
                let lines: Vec<&str> = stdout.lines().collect();
                if lines.len() >= 2 {
                    let version_str = lines[1]; // e.g., "14.5"
                    // Map version number to name (needs refinement for future/past versions)
                    let os_name = match version_str.split('.').next() {
                        Some("15") => "sequoia",    // macOS 15
                        Some("14") => "sonoma",     // macOS 14
                        Some("13") => "ventura",    // macOS 13
                        Some("12") => "monterey",   // macOS 12
                        Some("11") => "big_sur",    // macOS 11
                        Some("10") => match version_str.split('.').nth(1) {
                            Some("15") => "catalina", // 10.15
                            Some("14") => "mojave",   // 10.14
                            // Add more older versions if needed
                            _ => "unknown_macos",
                        },
                        _ => "unknown_macos",
                    };
                    // Construct the platform string (e.g., "arm64_sonoma")
                    if arch == "arm64" {
                        return format!("{}_{}", arch, os_name);
                    } else {
                        // Intel Macs usually just use the OS name
                        return os_name.to_string();
                    }
                }
            } else {
                let stderr_msg = String::from_utf8_lossy(&output.stderr);
                eprintln!(
                    "Warning: '/usr/bin/sw_vers -productName -productVersion' command failed (status: {}). Stderr: {}",
                    output.status,
                    stderr_msg
                );
            }
        } else if let Err(e) = output {
            eprintln!("Warning: Failed to execute '/usr/bin/sw_vers'. Error: {}", e);
        }

        // Fallback if sw_vers fails (less accurate)
        eprintln!("Warning: Using fallback macOS platform detection (sw_vers failed or output unparseable).");
        if arch == "arm64" {
            // Returning a potentially incorrect default might hide the real issue.
            // Maybe return "unknown" to make the failure more explicit?
            // For now, keep the previous fallback but with a clearer warning.
            return "arm64_monterey".to_string();
        } else {
            return "monterey".to_string();
        }

    } else if cfg!(target_os = "linux") {
        // Linux detection remains the same
        if std::env::consts::ARCH == "aarch64" {
            return "arm64_linux".to_string();
        } else {
            return "x86_64_linux".to_string();
        }
    }

    // Default fallback for other OS
    "unknown".to_string()
}

/// Extract a downloaded archive to the target directory
pub fn extract_archive(archive_path: &Path, target_dir: &Path) -> Result<()> {
    // Create target directory if it doesn't exist
    fs::create_dir_all(target_dir)?;

    // Check file extension to determine extraction method
    let extension = archive_path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    match extension {
        "tar" => extract_tar(archive_path, target_dir),
        "gz" | "tgz" => extract_tar_gz(archive_path, target_dir),
        "bz2" => extract_tar_bz2(archive_path, target_dir),
        "xz" => extract_tar_xz(archive_path, target_dir),
        "zip" => extract_zip(archive_path, target_dir),
        _ => Err(SapphireError::Generic(format!("Unsupported archive format: {}", extension)))
    }
}

/// Extract a tar archive
fn extract_tar(archive_path: &Path, target_dir: &Path) -> Result<()> {
    let output = Command::new("tar")
        .arg("-xf")
        .arg(archive_path)
        .arg("-C")
        .arg(target_dir)
        .output()?;

    if !output.status.success() {
        return Err(SapphireError::Generic(
            format!("Failed to extract tar archive: {}", String::from_utf8_lossy(&output.stderr))
        ));
    }

    Ok(())
}

/// Extract a tar.gz archive
fn extract_tar_gz(archive_path: &Path, target_dir: &Path) -> Result<()> {
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(target_dir)
        .output()?;

    if !output.status.success() {
        return Err(SapphireError::Generic(
            format!("Failed to extract tar.gz archive: {}", String::from_utf8_lossy(&output.stderr))
        ));
    }

    Ok(())
}

/// Extract a tar.bz2 archive
fn extract_tar_bz2(archive_path: &Path, target_dir: &Path) -> Result<()> {
    let output = Command::new("tar")
        .arg("-xjf")
        .arg(archive_path)
        .arg("-C")
        .arg(target_dir)
        .output()?;

    if !output.status.success() {
        return Err(SapphireError::Generic(
            format!("Failed to extract tar.bz2 archive: {}", String::from_utf8_lossy(&output.stderr))
        ));
    }

    Ok(())
}

/// Extract a tar.xz archive
fn extract_tar_xz(archive_path: &Path, target_dir: &Path) -> Result<()> {
    let output = Command::new("tar")
        .arg("-xJf")
        .arg(archive_path)
        .arg("-C")
        .arg(target_dir)
        .output()?;

    if !output.status.success() {
        return Err(SapphireError::Generic(
            format!("Failed to extract tar.xz archive: {}", String::from_utf8_lossy(&output.stderr))
        ));
    }

    Ok(())
}

/// Extract a zip archive
fn extract_zip(archive_path: &Path, target_dir: &Path) -> Result<()> {
    let output = Command::new("unzip")
        .arg("-qq")
        .arg(archive_path)
        .arg("-d")
        .arg(target_dir)
        .output()?;

    if !output.status.success() {
        return Err(SapphireError::Generic(
            format!("Failed to extract zip archive: {}", String::from_utf8_lossy(&output.stderr))
        ));
    }

    Ok(())
}

/// Get the standard Homebrew Cellar path
pub fn get_cellar_path() -> PathBuf {
    if std::env::consts::ARCH == "aarch64" {
        PathBuf::from("/opt/homebrew/Cellar")
    } else {
        PathBuf::from("/usr/local/Cellar")
    }
}

/// Get the path where a formula should be installed in the Cellar
pub fn get_formula_cellar_path(formula: &Formula) -> PathBuf {
    let cellar = get_cellar_path();
    let version = formula.version.clone();
    cellar.join(&formula.name).join(version.to_string())
}

/// Create a receipt file to record formula installation
pub fn write_receipt(formula: &Formula, install_dir: &Path) -> Result<()> {
    let receipt_dir = install_dir.join("INSTALL_RECEIPT.json");
    let mut receipt_file = File::create(receipt_dir)?;

    let receipt = serde_json::json!({
        "name": formula.name,
        "version": formula.version,
        "time": chrono::Utc::now().to_rfc3339(),
        "source": {
            "path": formula.homepage,
        },
        "built_on": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        }
    });

    let receipt_json = serde_json::to_string_pretty(&receipt)?;
    receipt_file.write_all(receipt_json.as_bytes())?;

    Ok(())
}

// Re-export relevant items
pub use source::{build_from_source, download_source};
pub use bottle::install_bottle;
pub use link::link_formula_binaries;
