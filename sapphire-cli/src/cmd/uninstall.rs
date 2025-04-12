// src/cmd/uninstall.rs
// Contains the logic for the `uninstall` command.

use sapphire_core::utils::error::{SapphireError, Result};
use crate::cmd::info;
use sapphire_core::build;
use std::fs;
use std::path::PathBuf;
use walkdir;
use serde_json;

use sapphire_core::model::cask::Cask; // Add Cask import
use sapphire_core::utils::cache::Cache; // Add Cache import
use sapphire_core::fetch::api; // Add api import

pub async fn run_uninstall(name: &str) -> Result<()> {
    let cache_dir = sapphire_core::utils::cache::get_cache_dir()?;
    let _cache = Cache::new(&cache_dir)?; // renamed to _cache since it's not used below

    // Try to get info as a formula first
    if let Ok(formula) = info::get_formula_info(name).await {
        println!("==> Uninstalling formula: {}", name);
        let cellar_path = build::formula::get_formula_cellar_path(&formula);

        if !cellar_path.exists() {
            return Err(SapphireError::NotFound(format!("Formula '{}' is not installed (no keg at {})", name, cellar_path.display())));
        }

        // Count files before removal
        let (file_count, size_bytes) = count_files_and_size(&cellar_path)?;

        // Remove symlinks listed in INSTALL_MANIFEST.json
        let manifest_path = cellar_path.join("INSTALL_MANIFEST.json");
        if manifest_path.exists() {
            match std::fs::read_to_string(&manifest_path) {
                Ok(manifest_str) => {
                    match serde_json::from_str::<Vec<String>>(&manifest_str) {
                        Ok(symlinks) => {
                            for symlink in symlinks {
                                let symlink_path = std::path::Path::new(&symlink);
                                if symlink_path.exists() {
                                    if let Err(e) = std::fs::remove_file(symlink_path) {
                                        eprintln!("Warning: Failed to remove symlink {}: {}", symlink_path.display(), e);
                                    } else {
                                        println!("Removed symlink: {}", symlink_path.display());
                                    }
                                }
                            }
                        }
                        Err(e) => eprintln!("Warning: Failed to parse formula install manifest: {}", e),
                    }
                }
                Err(e) => eprintln!("Warning: Failed to read formula install manifest: {}", e),
            }
        } else {
            // Fallback: Try the old unlink logic if manifest doesn't exist
            println!("Warning: No install manifest found, attempting legacy unlink...");
            build::formula::link::unlink_formula_binaries(&formula)?;
        }

        // Remove the formula's directory from the Cellar
        fs::remove_dir_all(&cellar_path)?;

        println!("Uninstalling {}... ({} files, {})",
            cellar_path.display(),
            file_count,
            format_size(size_bytes));

        return Ok(());
    }

    // If not a formula, try as a cask
    if let Ok(cask_json) = api::fetch_cask(name).await {
        let cask: Cask = serde_json::from_value(cask_json)?;
        println!("==> Uninstalling cask: {}", name);
        let caskroom_path = build::cask::get_cask_path(&cask);

        if !caskroom_path.exists() {
            return Err(SapphireError::NotFound(format!("Cask '{}' is not installed (no caskroom at {})", name, caskroom_path.display())));
        }

        // Count files before removal (might be less accurate for casks)
        let (file_count, size_bytes) = count_files_and_size(&caskroom_path)?;

        // Remove files listed in INSTALL_MANIFEST.json
        let manifest_path = caskroom_path.join("INSTALL_MANIFEST.json");
        if manifest_path.exists() {
            match std::fs::read_to_string(&manifest_path) {
                Ok(manifest_str) => {
                    match serde_json::from_str::<Vec<String>>(&manifest_str) {
                        Ok(files_to_remove) => {
                            for file_path_str in files_to_remove {
                                // Check for pkgutil directive
                                if let Some(pkg_id) = file_path_str.strip_prefix("pkgutil:") {
                                    println!("==> Forgetting package receipt: {}", pkg_id);
                                    let output = std::process::Command::new("sudo")
                                        .arg("pkgutil")
                                        .arg("--forget")
                                        .arg(pkg_id)
                                        .output()?;
                                    if !output.status.success() {
                                        eprintln!("Warning: Failed to forget package receipt {}: {}", pkg_id, String::from_utf8_lossy(&output.stderr));
                                    }
                                    continue; // Don't try to remove this as a file/dir
                                }

                                // Handle regular file/directory removal
                                let file_path = std::path::Path::new(&file_path_str);
                                if file_path.exists() {
                                    if file_path.is_dir() {
                                        // Try removing directory (e.g., the .app bundle)
                                        if let Err(e) = std::fs::remove_dir_all(file_path) {
                                            eprintln!("Warning: Failed to remove directory {}: {}", file_path.display(), e);
                                            println!("Attempting to remove directory with sudo...");
                                            let output = std::process::Command::new("sudo")
                                                .arg("rm")
                                                .arg("-rf")
                                                .arg(file_path)
                                                .output();
                                            if let Err(sudo_err) = output {
                                                eprintln!("Error: Failed to remove directory {} with sudo: {}", file_path.display(), sudo_err);
                                            } else {
                                                println!("Successfully removed directory {} with sudo.", file_path.display());
                                            }
                                        } else {
                                            println!("Removed directory: {}", file_path.display());
                                        }
                                    } else {
                                        // Try removing file (e.g., the caskroom symlink)
                                        if let Err(e) = std::fs::remove_file(file_path) {
                                            eprintln!("Warning: Failed to remove file {}: {}", file_path.display(), e);
                                        } else {
                                            println!("Removed file: {}", file_path.display());
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => eprintln!("Warning: Failed to parse cask install manifest: {}", e),
                    }
                }
                Err(e) => eprintln!("Warning: Failed to read cask install manifest: {}", e),
            }
        } else {
            println!("Warning: No install manifest found for cask {}. Cannot perform clean uninstall.", name);
            // Optionally, add logic here to attempt removal based on cask stanzas if manifest is missing
        }

        // Remove the cask's directory from the Caskroom
        // Only do this if the manifest was successfully processed or didn't exist
        if manifest_path.exists() { // Re-check existence in case reading failed
             if let Err(e) = fs::remove_dir_all(&caskroom_path) {
                 eprintln!("Warning: Failed to remove caskroom directory {}: {}", caskroom_path.display(), e);
             }
        } else {
             // If no manifest, still try to remove the caskroom
             if let Err(e) = fs::remove_dir_all(&caskroom_path) {
                 eprintln!("Warning: Failed to remove caskroom directory {}: {}", caskroom_path.display(), e);
             }
        }


        println!("Uninstalling {}... (~{} files, ~{})",
            caskroom_path.display(),
            file_count,
            format_size(size_bytes));

        return Ok(());
    }

    // If not found as formula or cask
    Err(SapphireError::NotFound(format!("Formula or Cask '{}' not found or not installed", name)))
}

/// Count files and calculate total size in a directory
fn count_files_and_size(path: &PathBuf) -> Result<(usize, u64)> {
    let mut file_count = 0;
    let mut total_size = 0;

    // Walk the directory recursively
    for entry in walkdir::WalkDir::new(path) {
        let entry = entry.map_err(|e| SapphireError::Generic(e.to_string()))?;
        if entry.file_type().is_file() {
            file_count += 1;
            total_size += entry.metadata()
                .map_err(|e| SapphireError::Generic(e.to_string()))?
                .len();
        }
    }

    Ok((file_count, total_size))
}

/// Format file size in human-readable format
fn format_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if size >= GB {
        format!("{:.1}GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1}MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.1}KB", size as f64 / KB as f64)
    } else {
        format!("{}B", size)
    }
}
