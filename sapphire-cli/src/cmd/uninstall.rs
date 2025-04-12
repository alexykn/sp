// src/cmd/uninstall.rs
// Contains the logic for the `uninstall` command.

use sapphire_core::utils::error::{BrewRsError, Result};
use crate::cmd::info;
use sapphire_core::build;
use std::fs;
use std::path::PathBuf;
use walkdir;

pub async fn run_uninstall(name: &str) -> Result<()> {
    println!("==> Uninstalling formula: {}", name);

    // Get formula information
    let formula = match info::get_formula_info(name).await {
        Ok(formula) => formula,
        Err(_) => {
            // If not found as a formula, notify the user
            return Err(BrewRsError::NotFound(format!("Formula '{}' not found", name)));
        }
    };

    // Check if the formula is installed
    let cellar_path = build::formula::get_formula_cellar_path(&formula);

    if !cellar_path.exists() {
        return Err(BrewRsError::NotFound(format!("No such keg: {}", cellar_path.display())));
    }

    // First unlink binaries
    build::formula::link::unlink_formula_binaries(&formula)?;

    // Count number of files and calculate size before removal
    let (file_count, size_bytes) = count_files_and_size(&cellar_path)?;

    // Remove the formula's directory from the Cellar
    fs::remove_dir_all(&cellar_path)?;

    // TODO: Remove symlinks from /usr/local/bin or /opt/homebrew/bin

    println!("Uninstalling {}... ({} files, {})",
        cellar_path.display(),
        file_count,
        format_size(size_bytes));

    Ok(())
}

/// Count files and calculate total size in a directory
fn count_files_and_size(path: &PathBuf) -> Result<(usize, u64)> {
    let mut file_count = 0;
    let mut total_size = 0;

    // Walk the directory recursively
    for entry in walkdir::WalkDir::new(path) {
        let entry = entry.map_err(|e| BrewRsError::Generic(e.to_string()))?;
        if entry.file_type().is_file() {
            file_count += 1;
            total_size += entry.metadata()
                .map_err(|e| BrewRsError::Generic(e.to_string()))?
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
