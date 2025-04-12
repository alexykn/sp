// build/mod.rs - Main module for build functionality

pub mod formula;
pub mod cask;
pub mod devtools;
pub mod fallback;
pub mod env;

use crate::utils::error::{Result, SapphireError};
use std::path::Path;
use std::process::Command;


// Re-export common functionality
pub use formula::{get_cellar_path, get_formula_cellar_path, write_receipt};

// Helper function to get Homebrew prefix (used by build modules)
pub fn get_homebrew_prefix() -> std::path::PathBuf {
    // Prioritize HOMEBREW_PREFIX env var if set
    if let Ok(prefix) = std::env::var("HOMEBREW_PREFIX") {
         if !prefix.is_empty() {
             return std::path::PathBuf::from(prefix);
         }
    }
    // Otherwise, check standard locations
    if std::path::Path::new("/opt/homebrew").exists() {
        std::path::PathBuf::from("/opt/homebrew")
    } else if std::path::Path::new("/usr/local").exists() {
        std::path::PathBuf::from("/usr/local")
    } else {
        // Sensible default if others don't exist (e.g., clean install)
        if cfg!(target_arch = "aarch64") {
             std::path::PathBuf::from("/opt/homebrew")
        } else {
             std::path::PathBuf::from("/usr/local")
        }
    }
}

// Path helpers
pub fn get_formula_opt_path(formula: &crate::model::formula::Formula) -> std::path::PathBuf {
    let prefix = get_homebrew_prefix();
    prefix.join("opt").join(formula.name()) // Use formula.name() method
}


/// Extract a downloaded archive to the target directory, stripping leading components.
pub fn extract_archive_strip_components(
    archive_path: &Path,
    target_dir: &Path,
    strip_components: usize
) -> Result<()> {
    // Create target directory if it doesn't exist
    std::fs::create_dir_all(target_dir)?;

    // Check file extension to determine extraction method
    let extension = archive_path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    let mut cmd = Command::new("tar");
    cmd.arg("-xf") // Extract files
       .arg(archive_path)
       .arg("-C") // Change to target directory
       .arg(target_dir)
       .arg(format!("--strip-components={}", strip_components)); // Strip leading components

    match extension {
        "gz" | "tgz" => { cmd.arg("-z"); } // gzip
        "bz2" | "tbz" | "tbz2" => { cmd.arg("-j"); } // bzip2
        "xz" | "txz" => { cmd.arg("-J"); } // xz
        "tar" => { /* No compression flag needed */ }
        // TODO: Add zip support if needed (would require a different command)
        // "zip" => return extract_zip_strip(archive_path, target_dir, strip_components),
        _ => return Err(SapphireError::Generic(format!("Unsupported archive format for stripping: {}", extension)))
    }

    println!("Executing tar command: {:?}", cmd);
    let output = cmd.output()?;

    if !output.status.success() {
        eprintln!("Tar stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Tar stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(
            format!("Failed to extract archive with strip-components: {}", String::from_utf8_lossy(&output.stderr))
        ));
    }

    Ok(())
}


// --- Kept extract_archive for potential non-stripping use cases ---

/// Extract a downloaded archive to the target directory (without stripping components).
pub fn extract_archive(archive_path: &Path, target_dir: &Path) -> Result<()> {
    // Create target directory if it doesn't exist
    std::fs::create_dir_all(target_dir)?;

    // Check file extension to determine extraction method
    let extension = archive_path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    match extension {
        "tar" => extract_tar(archive_path, target_dir),
        "gz" | "tgz" => extract_tar_gz(archive_path, target_dir),
        "bz2" | "tbz" | "tbz2" => extract_tar_bz2(archive_path, target_dir),
        "xz" | "txz" => extract_tar_xz(archive_path, target_dir),
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
        .arg("-qq") // Quiet
        .arg("-o") // Overwrite existing files without prompting
        .arg(archive_path)
        .arg("-d") // Destination directory
        .arg(target_dir)
        .output()?;

    if !output.status.success() {
        return Err(SapphireError::Generic(
            format!("Failed to extract zip archive: {}", String::from_utf8_lossy(&output.stderr))
        ));
    }

    Ok(())
}