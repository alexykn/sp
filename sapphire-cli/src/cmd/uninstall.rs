// src/cmd/uninstall.rs
// Contains the logic for the `uninstall` command.

use crate::cmd::info; // Use crate::cmd path
// Removed unused colored import
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;
use std::sync::Arc;
use sapphire_core::build;
use sapphire_core::utils::error::{Result, SapphireError};
use serde_json;
use std::fs;
use walkdir; // Keep unused import for now if needed elsewhere

use log;
use sapphire_core::fetch::api; // Add api import
use sapphire_core::model::cask::Cask; // Add Cask import
use sapphire_core::utils::cache::Cache; // Add Cache import // Use log crate
use sapphire_core::utils::config::Config;

// Modified function signature to accept Vec<String>
pub async fn run_uninstall(names: &[String], config: &Config, cache: Arc<Cache>) -> Result<()> {
    let mut errors: Vec<SapphireError> = Vec::new();

    // Initialize config and cache (outside the loop)
    let cache_dir = match sapphire_core::utils::cache::get_cache_dir() {
        Ok(dir) => dir,
        Err(e) => {
            log::error!("Failed to get cache directory: {}", e);
            // Decide if you want to proceed without cache or return error
            return Err(e);
        }
    };
    let _cache = Cache::new(&cache_dir).map_err(|e| {
        log::error!("Failed to initialize cache: {}", e);
        e // Return the original error
    })?;

    // Main loop to iterate through package names
    for name in names {
        // Spinner for uninstall
        let pb = ProgressBar::new_spinner();
        pb.set_style(ProgressStyle::with_template("{spinner:.red} {msg}").unwrap());
        pb.set_message(format!("Uninstalling {}", name));
        pb.enable_steady_tick(Duration::from_millis(100));

        // Try to get info as a formula first
        match info::get_formula_info(name, config, &cache).await {
            Ok(formula) => {
                log::debug!("Attempting to uninstall formula: {}", name);
                let cellar_path = build::formula::get_formula_cellar_path(&formula); // Assuming this returns Result

                if !cellar_path.exists() {
                    log::error!(
                        "Formula '{}' is not installed (no keg at {})",
                        name,
                        cellar_path.display()
                    );
                    errors.push(SapphireError::NotFound(format!(
                        "Formula '{}' is not installed (no keg at {})",
                        name,
                        cellar_path.display()
                    )));
                    pb.finish_and_clear(); // Clear spinner for this item
                    continue; // Skip to the next package
                }

                // Count files before removal
                let (file_count, size_bytes) = match count_files_and_size(&cellar_path) {
                    Ok((count, size)) => (count, size),
                    Err(e) => {
                        log::warn!(
                            "Failed to count files/size for {}: {}. Uninstalling anyway.",
                            cellar_path.display(),
                            e
                        );
                        (0, 0) // Default to 0 if counting fails
                    }
                };

                // *** FIX: Call the correct public unlink function ***
                match build::formula::link::unlink_formula_artifacts(&formula) {
                    Ok(_) => log::debug!("Successfully unlinked artifacts for {}", formula.name()),
                    Err(e) => {
                        // Log the error but proceed with cellar removal attempt
                        log::error!(
                            "Failed to unlink artifacts for {}: {}. Attempting cellar removal anyway.",
                            formula.name(),
                            e
                        );
                        // Collect this error as well? Maybe less critical than full removal failure.
                        // errors.push(e);
                    }
                }

                // Remove the formula's directory from the Cellar
                log::debug!("Removing keg directory: {}", cellar_path.display());
                if let Err(e) = fs::remove_dir_all(&cellar_path) {
                     let removal_error = SapphireError::Io(std::io::Error::new(
                         e.kind(),
                         format!("Failed remove keg {}: {}", cellar_path.display(), e),
                     ));
                     log::error!("{}", removal_error); // Log the specific error
                     errors.push(removal_error);
                     pb.finish_and_clear(); // Clear spinner for this item
                    continue; // Skip to the next package
                }


                // Finish spinner with summary
                pb.finish_with_message(format!(
                    "Uninstalled {} ({} files, {})",
                    cellar_path.display(),
                    file_count,
                    format_size(size_bytes)
                ));

                 // Successfully uninstalled, continue loop
                continue; // Explicitly continue to next item
            }
            Err(SapphireError::NotFound(_)) => {
                // Formula not found, proceed to check if it's a cask
                log::debug!("Formula '{}' not found, checking if it's a cask.", name);
            }
            Err(e) => {
                // Other error fetching formula info
                log::error!("Error getting formula info for '{}': {}", name, e);
                errors.push(e);
                pb.finish_and_clear(); // Clear spinner for this item
                continue; // Skip to the next package
            }
        }

        // If not a formula, try as a cask
        match api::fetch_cask(name).await {
            Ok(cask_json) => {
                let cask: Cask = match serde_json::from_value(cask_json) {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!("Failed to parse cask JSON for {}: {}", name, e);
                        errors.push(SapphireError::Json(e));
                        pb.finish_and_clear(); // Clear spinner for this item
                        continue; // Skip to the next package
                    }
                };
                log::debug!("Attempting to uninstall cask: {}", name);
                let caskroom_path = build::cask::get_cask_path(&cask);

                if !caskroom_path.exists() {
                    log::error!(
                        "Cask '{}' is not installed (no caskroom at {})",
                        name,
                        caskroom_path.display()
                    );
                    errors.push(SapphireError::NotFound(format!(
                        "Cask '{}' is not installed (no caskroom at {})",
                        name,
                        caskroom_path.display()
                    )));
                    pb.finish_and_clear(); // Clear spinner for this item
                    continue; // Skip to the next package
                }

                // Count files before removal (might be less accurate for casks)
                let (file_count, size_bytes) = match count_files_and_size(&caskroom_path) {
                    Ok((count, size)) => (count, size),
                    Err(e) => {
                        log::warn!(
                            "Failed to count files/size for {}: {}. Uninstalling anyway.",
                            caskroom_path.display(),
                            e
                        );
                        (0, 0)
                    }
                };

                // Remove files/dirs/pkg receipts listed in INSTALL_MANIFEST.json
                // Cask uninstall logic requires manifest and potentially sudo
                let manifest_path = caskroom_path.join("INSTALL_MANIFEST.json"); // Use consistent name
                let mut artifact_removal_errors = 0; // Track errors within this cask's uninstall
                if manifest_path.is_file() {
                    match std::fs::read_to_string(&manifest_path) {
                        Ok(manifest_str) => {
                            match serde_json::from_str::<Vec<String>>(&manifest_str) {
                                Ok(files_to_remove) => {
                                    for file_path_str in files_to_remove {
                                        // Check for pkgutil directive
                                        if let Some(pkg_id) = file_path_str.strip_prefix("pkgutil:") {
                                            log::debug!("==> Forgetting package receipt: {}", pkg_id);
                                            let output = std::process::Command::new("sudo")
                                                .arg("pkgutil")
                                                .arg("--forget")
                                                .arg(pkg_id)
                                                .output(); // Capture output
                                            match output {
                                                Ok(out) => {
                                                    if !out.status.success() {
                                                        log::warn!(
                                                            "Failed to forget package receipt {}: {}",
                                                            pkg_id,
                                                            String::from_utf8_lossy(&out.stderr)
                                                        );
                                                         artifact_removal_errors += 1; // Count as an error for this cask
                                                    } else {
                                                        log::debug!(
                                                            "Successfully forgot package receipt {}",
                                                            pkg_id
                                                        );
                                                    }
                                                }
                                                Err(e) => {
                                                    log::warn!("Failed to execute sudo pkgutil --forget {}: {}", pkg_id, e);
                                                     artifact_removal_errors += 1;
                                                }
                                            }
                                            continue; // Don't try to remove this as a file/dir
                                        }

                                        // Handle regular file/directory removal
                                        let file_path = std::path::Path::new(&file_path_str);
                                        log::debug!(
                                            "Attempting removal of artifact: {}",
                                            file_path.display()
                                        );
                                        // Use symlink_metadata to check existence without following link
                                        match file_path.symlink_metadata() {
                                            Ok(metadata) => {
                                                let remove_result = if metadata.file_type().is_dir() {
                                                    log::debug!(
                                                        "Removing directory: {}",
                                                        file_path.display()
                                                    );
                                                    std::fs::remove_dir_all(file_path)
                                                } else {
                                                    log::debug!(
                                                        "Removing file/symlink: {}",
                                                        file_path.display()
                                                    );
                                                    std::fs::remove_file(file_path)
                                                };

                                                if let Err(e) = remove_result {
                                                    log::warn!("Failed to remove artifact {}: {}. Attempting with sudo...", file_path.display(), e);
                                                    // Attempt removal with sudo as fallback
                                                    let sudo_output =
                                                        std::process::Command::new("sudo")
                                                            .arg("rm")
                                                            .arg("-rf") // Force recursive removal
                                                            .arg(file_path)
                                                            .output();
                                                    match sudo_output {
                                                        Ok(out) => {
                                                            if !out.status.success() {
                                                                log::error!("Failed to remove artifact {} with sudo: {}", file_path.display(), String::from_utf8_lossy(&out.stderr));
                                                                 artifact_removal_errors += 1;
                                                            } else {
                                                                log::debug!("Successfully removed artifact {} with sudo.", file_path.display());
                                                            }
                                                        }
                                                        Err(sudo_err) => {
                                                            log::error!(
                                                                "Error executing sudo rm for {}: {}",
                                                                file_path.display(),
                                                                sudo_err
                                                            );
                                                             artifact_removal_errors += 1;
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                                log::debug!(
                                                    "Artifact listed in manifest not found: {}",
                                                    file_path.display()
                                                );
                                            }
                                            Err(e) => {
                                                log::warn!(
                                                    "Failed to get metadata for artifact {}: {}",
                                                    file_path.display(),
                                                    e
                                                );
                                                 artifact_removal_errors += 1;
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                     log::warn!(
                                        "Failed to parse cask install manifest {}: {}",
                                        manifest_path.display(),
                                        e
                                    );
                                     errors.push(SapphireError::Generic(format!("Failed to parse manifest for cask '{}'", name)));
                                     // Mark as error for this cask
                                     artifact_removal_errors +=1;
                                }

                            }
                        }
                        Err(e) => {
                            log::warn!(
                                "Failed to read cask install manifest {}: {}",
                                manifest_path.display(),
                                e
                            );
                            errors.push(SapphireError::Generic(format!("Failed to read manifest for cask '{}'", name)));
                             // Mark as error for this cask
                             artifact_removal_errors +=1;
                        }
                    }
                } else {
                    log::warn!("No install manifest found for cask {}. Cannot perform clean uninstall based on recorded artifacts.", name);
                    // Optionally, add logic here to attempt removal based on cask stanzas if manifest is missing
                    // For now, just add a warning error
                    errors.push(SapphireError::Generic(format!("No manifest found for cask '{}', uninstall might be incomplete.", name)));
                     // Mark as error for this cask
                     artifact_removal_errors +=1;
                }

                 // If any artifact removal failed for this cask, add a general error
                 if artifact_removal_errors > 0 {
                     // Check if a more specific error was already pushed
                     if !errors.iter().any(|e| match e {
                         SapphireError::Generic(s) if s.contains(&format!("cask '{}'", name)) => true,
                         _ => false,
                     }) {
                         errors.push(SapphireError::Generic(format!("Failed to remove {} artifacts for cask '{}'", artifact_removal_errors, name)));
                     }
                 }

                // Remove the cask's version directory from the Caskroom (attempt even if manifest had issues)
                log::debug!("Removing caskroom directory: {}", caskroom_path.display());
                if let Err(e) = fs::remove_dir_all(&caskroom_path) {
                    log::warn!(
                        "Failed to remove caskroom directory {}: {}",
                        caskroom_path.display(),
                        e
                    );
                     // Add error if caskroom removal fails
                     errors.push(SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed to remove caskroom for '{}'", name))));
                }

                // Finish spinner with summary (only if no *major* error occurred for this cask yet)
                if artifact_removal_errors == 0 && !errors.iter().any(|err| match err {
                    SapphireError::NotFound(s) if s.contains(name) => true, // Already handled not found
                    SapphireError::Json(e) if e.to_string().contains(name) => true, // Already handled parse error
                     _ => false,
                }) {
                    pb.finish_with_message(format!(
                        "Uninstalled {} (~{} files, ~{})",
                        caskroom_path.display(),
                        file_count,
                        format_size(size_bytes)
                    ));
                } else {
                     pb.finish_and_clear(); // Clear spinner if error already logged/collected
                }


                 // Successfully uninstalled (or attempted), continue loop
                continue;
            }
            Err(SapphireError::NotFound(_)) => {
                // Also not found as a cask
                log::error!("Formula or Cask '{}' not found.", name);
                errors.push(SapphireError::NotFound(format!(
                    "Formula or Cask '{}' not found",
                    name
                )));
                pb.finish_and_clear(); // Clear spinner for this item
                continue; // Skip to the next package
            }
            Err(e) => {
                // Other error fetching cask info
                log::error!("Error getting cask info for '{}': {}", name, e);
                errors.push(e);
                pb.finish_and_clear(); // Clear spinner for this item
                continue; // Skip to the next package
            }
        }
    } // End of loop over names

    // Check if any errors occurred during the process
    if !errors.is_empty() {
        // Use eprintln for final error output visible to user
        eprintln!("Finished uninstalling with errors:");
        for error in &errors {
            eprintln!("  - {}", error);
        }
        // Combine errors into a single summary error
        let error_summary = errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n  - ");
        return Err(SapphireError::Generic(format!(
            "Uninstall failed for {} package(s):\n  - {}", errors.len(), error_summary
        )));
    }

    Ok(())
}


/// Count files and calculate total size in a directory
fn count_files_and_size(path: &std::path::Path) -> Result<(usize, u64)> {
    // Use std::path::Path
    let mut file_count = 0;
    let mut total_size = 0;

    // Walk the directory recursively
    for entry in walkdir::WalkDir::new(path) {
        match entry {
            Ok(entry_data) => {
                // Skip directories themselves in the count/size
                if entry_data.file_type().is_file() {
                    match entry_data.metadata() {
                        Ok(metadata) => {
                            file_count += 1;
                            total_size += metadata.len();
                        }
                        Err(e) => {
                            log::warn!(
                                "Could not get metadata for {}: {}",
                                entry_data.path().display(),
                                e
                            );
                            // Continue counting other files
                        }
                    }
                }
            }
            Err(e) => {
                log::warn!("Error traversing directory {}: {}", path.display(), e);
                // Continue if possible, but might skew results
            }
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