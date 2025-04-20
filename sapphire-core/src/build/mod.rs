// ===== sapphire-core/src/build/mod.rs =====
// Main module for build functionality

pub mod cask;
pub mod devtools;
pub mod env;
pub mod formula;

use crate::model::formula::Formula; // Import Formula
use crate::utils::config::Config; // Import Config
use crate::utils::error::{Result, SapphireError};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;

// Re-export common functionality (keep cellar/receipt for now, might be refactored later)
pub use formula::{get_formula_cellar_path, write_receipt};

// REMOVED: get_homebrew_prefix (now in Config)

// --- Path helpers using Config ---
pub fn get_formula_opt_path(formula: &Formula, config: &Config) -> PathBuf {
    // Use Config method
    config.formula_opt_link_path(formula.name())
}


/// Extract a downloaded archive to the target directory, stripping leading components.
pub fn extract_archive_strip_components(
    archive_path: &Path,
    target_dir: &Path,
    strip_components: usize,
) -> Result<()> {
    std::fs::create_dir_all(target_dir)?;

    let extension = archive_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    let mut cmd = Command::new("tar");
    cmd.arg("-xf")
        .arg(archive_path)
        .arg("-C")
        .arg(target_dir)
        .arg(format!("--strip-components={}", strip_components));

    let cmd = match extension {
        "gz" | "tgz" => cmd.arg("-z"),
        "bz2" | "tbz" | "tbz2" => cmd.arg("-j"),
        "xz" | "txz" => cmd.arg("-J"),
        "tar" => &mut cmd,
        _ => {
            return Err(SapphireError::Generic(format!(
                "Unsupported archive format for stripping: {}",
                extension
            )))
        }
    };

    let output = cmd.stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()?;

    if !output.status.success() {
        log::error!("Tar stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        log::error!("Tar stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(format!(
            "Failed to extract archive with strip-components: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(())
}

// --- Kept extract_archive for potential non-stripping use cases ---
// ... (extract_archive and helpers remain unchanged) ...
pub fn extract_archive(archive_path: &Path, target_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(target_dir)?;
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