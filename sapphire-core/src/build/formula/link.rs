// src/build/link.rs
// Contains logic for linking binaries from the Cellar to bin directory

use crate::Result;
use crate::model::formula::Formula;
use std::path::{Path, PathBuf};
use std::fs;
use std::os::unix::fs as unix_fs;
use serde_json;
use crate::utils::error::SapphireError;

/// Link binaries from a formula's installation directory to the bin directory
pub fn link_formula_binaries(formula: &Formula, formula_dir: &Path) -> Result<()> {
    println!("==> Linking binaries for {}", formula.name);

    // Get bin directory
    let bin_dir = get_bin_directory();

    // Create bin directory if it doesn't exist
    fs::create_dir_all(&bin_dir)?;

    // Find bin directory in the formula directory
    let formula_bin_dir = formula_dir.join("bin");

    if !formula_bin_dir.exists() {
        println!("No binaries to link for {}", formula.name);
        return Ok(());
    }

    // Find all executables in the formula bin directory
    let entries = fs::read_dir(formula_bin_dir)?;
    let mut linked_count = 0;
    let mut symlinks: Vec<String> = Vec::new();

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && is_executable(&path)? {
            let file_name = path.file_name().unwrap();
            let target_link = bin_dir.join(file_name);

            // Remove existing link if it exists
            if target_link.exists() {
                fs::remove_file(&target_link)?;
            }

            // Create symlink
            unix_fs::symlink(&path, &target_link)?;
            println!("  Linked {} -> {}", target_link.display(), path.display());
            linked_count += 1;
            symlinks.push(target_link.to_string_lossy().to_string());
        }
    }

    // Write symlinks manifest
    if !symlinks.is_empty() {
        let manifest_path = formula_dir.join("INSTALL_MANIFEST.json");
        let manifest_json = serde_json::to_string_pretty(&symlinks)
            .map_err(|e| SapphireError::Generic(e.to_string()))?;
        fs::write(&manifest_path, manifest_json)?;
        println!("Wrote install manifest: {}", manifest_path.display());
    }

    if linked_count > 0 {
        println!("Successfully linked {} binaries for {}", linked_count, formula.name);
    } else {
        println!("No binaries found to link for {}", formula.name);
    }

    Ok(())
}

/// Link all artifacts (binaries, libraries, headers, etc.) from a formula's installation directory
pub fn link_formula_artifacts(formula: &Formula, formula_dir: &Path) -> Result<()> {
    println!("==> Linking artifacts for {}", formula.name);

    let mut symlinks: Vec<String> = Vec::new();

    // Define standard directories to link
    let artifact_dirs = ["bin", "lib", "include", "share/man"];
    let prefix_dir = if std::env::consts::ARCH == "aarch64" {
        PathBuf::from("/opt/homebrew")
    } else {
        PathBuf::from("/usr/local")
    };

    for dir in &artifact_dirs {
        let formula_subdir = formula_dir.join(dir);
        let target_subdir = prefix_dir.join(dir);

        if formula_subdir.exists() {
            fs::create_dir_all(&target_subdir)?;

            for entry in fs::read_dir(&formula_subdir)? {
                let entry = entry?;
                let path = entry.path();

                if path.is_file() {
                    let file_name = path.file_name().unwrap();
                    let target_link = target_subdir.join(file_name);

                    // Remove existing link if it exists
                    if target_link.exists() {
                        fs::remove_file(&target_link)?;
                    }

                    // Create symlink
                    unix_fs::symlink(&path, &target_link)?;
                    println!("  Linked {} -> {}", target_link.display(), path.display());
                    symlinks.push(target_link.to_string_lossy().to_string());
                }
            }
        }
    }

    // Write symlinks manifest
    if !symlinks.is_empty() {
        let manifest_path = formula_dir.join("INSTALL_MANIFEST.json");
        let manifest_json = serde_json::to_string_pretty(&symlinks)
            .map_err(|e| SapphireError::Generic(e.to_string()))?;
        fs::write(&manifest_path, manifest_json)?;
        println!("Wrote install manifest: {}", manifest_path.display());
    }

    println!("Successfully linked artifacts for {}", formula.name);
    Ok(())
}

/// Unlink binaries for a formula
pub fn unlink_formula_binaries(formula: &Formula) -> Result<()> {
    println!("==> Unlinking binaries for {}", formula.name);

    // Get bin directory
    let bin_dir = get_bin_directory();

    if !bin_dir.exists() {
        return Ok(());
    }

    // Get formula directory
    let formula_dir = super::get_formula_cellar_path(formula);
    let formula_bin_dir = formula_dir.join("bin");

    if !formula_bin_dir.exists() {
        return Ok(());
    }

    // Find all executables in the formula bin directory
    let entries = fs::read_dir(formula_bin_dir)?;
    let mut unlinked_count = 0;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && is_executable(&path)? {
            let file_name = path.file_name().unwrap();
            let target_link = bin_dir.join(file_name);

            if target_link.exists() {
                // Verify it's a symlink to our formula
                if is_symlink_to(&target_link, &path)? {
                    fs::remove_file(&target_link)?;
                    println!("  Unlinked {}", target_link.display());
                    unlinked_count += 1;
                }
            }
        }
    }

    if unlinked_count > 0 {
        println!("Successfully unlinked {} binaries for {}", unlinked_count, formula.name);
    } else {
        println!("No binaries found to unlink for {}", formula.name);
    }

    Ok(())
}

/// Get the standard Homebrew bin directory
pub(crate) fn get_bin_directory() -> PathBuf {
    if std::env::consts::ARCH == "aarch64" {
        PathBuf::from("/opt/homebrew/bin")
    } else {
        PathBuf::from("/usr/local/bin")
    }
}

/// Check if a file is executable
fn is_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path)?;
    let permissions = metadata.permissions();

    // Check if the file has execute permission (for owner)
    Ok(permissions.mode() & 0o100 != 0)
}

/// Check if a symlink points to a specific target
fn is_symlink_to(link: &Path, target: &Path) -> Result<bool> {
    if !link.exists() {
        return Ok(false);
    }

    if !link.is_symlink() {
        return Ok(false);
    }

    let link_target = fs::read_link(link)?;
    Ok(link_target == *target)
}
