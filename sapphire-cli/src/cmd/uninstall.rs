// ===== sapphire-cli/src/cmd/uninstall.rs =====
use crate::cmd::info;
// Removed unused ProgressBar and ProgressStyle imports
use std::sync::Arc;
use sapphire_core::build;
use sapphire_core::utils::error::{Result, SapphireError};
use sapphire_core::utils::config::Config; // Ensure Config is imported
use sapphire_core::utils::cache::Cache;
use serde_json;
use std::fs;
use walkdir;
use log;
use sapphire_core::fetch::api;
use sapphire_core::model::cask::Cask;
use crate::ui; // <-- ADDED: Import ui module

// Signature already correct (accepts &Config)
pub async fn run_uninstall(names: &[String], config: &Config, cache: Arc<Cache>) -> Result<()> {
    let mut errors: Vec<SapphireError> = Vec::new();

    for name in names {
        // Use the ui utility function to create the spinner
        let pb = ui::create_spinner(&format!("Uninstalling {}", name)); // <-- CHANGED

        // Try formula uninstall first
        match info::get_formula_info(name, config, &cache).await {
            Ok(formula) => {
                log::debug!("Attempting to uninstall formula: {}", name);
                // Use config method to get cellar path
                let cellar_path = config.formula_keg_path(formula.name(), &formula.version_str_full());

                if !cellar_path.exists() {
                    log::warn!("Formula '{}' seems installed (info found) but keg missing: {}", name, cellar_path.display());
                    // Try unlinking anyway, as the keg might be gone but links remain
                    match build::formula::link::unlink_formula_artifacts(&formula, config) {
                         Ok(_) => {
                             log::debug!("Successfully unlinked artifacts for potentially missing keg {}", formula.name());
                             pb.finish_with_message(format!("Unlinked remaining artifacts for {} (keg missing)", name));
                         },
                         Err(e) => {
                             log::error!("Failed to unlink artifacts for {}: {}", formula.name(), e);
                             errors.push(SapphireError::NotFound(format!("Formula '{}' is not installed (no keg at {})", name, cellar_path.display())));
                             pb.finish_and_clear();
                         }
                     }
                    continue; // Move to next name
                }

                let (file_count, size_bytes) = match count_files_and_size(&cellar_path) {
                    Ok((count, size)) => (count, size),
                    Err(e) => { log::warn!("Failed to count files/size for {}: {}. Uninstalling anyway.", cellar_path.display(), e); (0, 0) }
                };

                // Pass config to unlink function
                match build::formula::link::unlink_formula_artifacts(&formula, config) {
                    Ok(_) => log::debug!("Successfully unlinked artifacts for {}", formula.name()),
                    Err(e) => {
                        log::error!("Failed to unlink artifacts for {}: {}. Attempting cellar removal anyway.", formula.name(), e);
                        errors.push(e); // Record unlink error
                    }
                }

                log::debug!("Removing keg directory: {}", cellar_path.display());
                if let Err(e) = fs::remove_dir_all(&cellar_path) {
                     let removal_error = SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed remove keg {}: {}", cellar_path.display(), e)));
                     log::error!("{}", removal_error);
                     errors.push(removal_error);
                     pb.finish_and_clear();
                    continue;
                }

                 // Check if errors occurred during unlink before showing success message
                if !errors.iter().any(|e| matches!(e, SapphireError::Io(_) | SapphireError::Generic(_))) { // Check for specific errors related to this formula
                    pb.finish_with_message(format!("Uninstalled {} ({} files, {})", cellar_path.display(), file_count, format_size(size_bytes)));
                } else {
                    pb.finish_and_clear(); // Clear spinner if there were errors during unlink
                }
                continue; // Successfully uninstalled formula (or attempted removal)
            }
            Err(SapphireError::NotFound(_)) => {
                log::debug!("Formula '{}' not found, checking if it's a cask.", name);
                // Proceed to check cask below
            }
            Err(e) => {
                log::error!("Error getting formula info for '{}': {}", name, e);
                errors.push(e);
                pb.finish_and_clear();
                continue; // Skip to next name on error
            }
        }

        // Try cask uninstall if formula wasn't found or failed gracefully
        match api::fetch_cask(name).await { // Fetches JSON Value
            Ok(cask_json) => {
                let cask: Cask = match serde_json::from_value(cask_json) {
                    Ok(c) => c,
                    Err(e) => { log::error!("Failed to parse cask JSON for {}: {}", name, e); errors.push(SapphireError::Json(e)); pb.finish_and_clear(); continue; }
                };
                log::debug!("Attempting to uninstall cask: {}", name);

                let installed_version = match cask.installed_version(config) {
                     Some(v) => v,
                     None => {
                         log::error!("Cask '{}' is not installed.", name);
                         // Don't add error here if formula was also not found, only if it's explicitly a cask not found
                         if !errors.iter().any(|e| matches!(e, SapphireError::NotFound(_))) {
                            errors.push(SapphireError::NotFound(format!("Cask '{}' is not installed", name)));
                         }
                         pb.finish_and_clear();
                         continue;
                     }
                 };
                let cask_version_path = config.cask_version_path(&cask.token, &installed_version);


                if !cask_version_path.exists() {
                    log::error!("Cask '{}' version '{}' is installed (metadata found) but dir missing: {}", name, installed_version, cask_version_path.display());
                    errors.push(SapphireError::NotFound(format!("Cask '{}' version '{}' installation is inconsistent (missing dir)", name, installed_version)));
                    pb.finish_and_clear();
                    continue;
                }

                let (file_count, size_bytes) = match count_files_and_size(&cask_version_path) {
                    Ok((count, size)) => (count, size),
                    Err(e) => { log::warn!("Failed to count files/size for {}: {}. Uninstalling anyway.", cask_version_path.display(), e); (0, 0) }
                };

                let manifest_path = cask_version_path.join("INSTALL_MANIFEST.json");
                let mut artifact_removal_errors = 0;

                if manifest_path.is_file() {
                    match std::fs::read_to_string(&manifest_path) {
                        Ok(manifest_str) => {
                            match serde_json::from_str::<Vec<String>>(&manifest_str) {
                                Ok(files_to_remove) => {
                                    for file_path_str in files_to_remove.iter().rev() { // Iterate backwards for safety if paths overlap
                                        if let Some(pkg_id) = file_path_str.strip_prefix("pkgutil:") {
                                            log::debug!("==> Forgetting package receipt: {}", pkg_id);
                                            let output = std::process::Command::new("sudo").arg("pkgutil").arg("--forget").arg(pkg_id).output();
                                            match output {
                                                Ok(out) if !out.status.success() => { log::warn!("Failed to forget package receipt {}: {}", pkg_id, String::from_utf8_lossy(&out.stderr)); artifact_removal_errors += 1; }
                                                Ok(_) => { log::debug!("Successfully forgot package receipt {}", pkg_id); }
                                                Err(e) => { log::warn!("Failed to execute sudo pkgutil --forget {}: {}", pkg_id, e); artifact_removal_errors += 1; }
                                            }
                                            continue;
                                        }

                                        let file_path = std::path::Path::new(file_path_str);
                                        log::debug!("Attempting removal of artifact: {}", file_path.display());
                                        match file_path.symlink_metadata() { // Use symlink_metadata to avoid following links
                                            Ok(metadata) => {
                                                 // Only remove if it exists
                                                let remove_result = if metadata.is_dir() {
                                                    log::debug!("Removing directory artifact: {}", file_path.display());
                                                    std::fs::remove_dir_all(file_path)
                                                } else {
                                                    log::debug!("Removing file/symlink artifact: {}", file_path.display());
                                                    std::fs::remove_file(file_path)
                                                };
                                                if let Err(e) = remove_result {
                                                    log::warn!("Failed to remove artifact {}: {}. Attempting with sudo...", file_path.display(), e);
                                                    let sudo_output = std::process::Command::new("sudo").arg("rm").arg("-rf").arg(file_path).output();
                                                    match sudo_output {
                                                        Ok(out) if !out.status.success() => { log::error!("Failed to remove artifact {} with sudo: {}", file_path.display(), String::from_utf8_lossy(&out.stderr)); artifact_removal_errors += 1; }
                                                        Ok(_) => { log::debug!("Successfully removed artifact {} with sudo.", file_path.display()); }
                                                        Err(sudo_err) => { log::error!("Error executing sudo rm for {}: {}", file_path.display(), sudo_err); artifact_removal_errors += 1; }
                                                    }
                                                } else {
                                                     log::debug!("Successfully removed artifact: {}", file_path.display());
                                                }
                                            }
                                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => { log::debug!("Artifact listed in manifest not found (already removed?): {}", file_path.display()); }
                                            Err(e) => { log::warn!("Failed to get metadata for artifact {}: {}", file_path.display(), e); artifact_removal_errors += 1; } // Count error if metadata fails
                                        }
                                    }
                                }
                                Err(e) => { log::warn!("Failed to parse cask install manifest {}: {}", manifest_path.display(), e); errors.push(SapphireError::Generic(format!("Failed to parse manifest for cask '{}'", name))); artifact_removal_errors +=1; }
                            }
                        }
                        Err(e) => { log::warn!("Failed to read cask install manifest {}: {}", manifest_path.display(), e); errors.push(SapphireError::Generic(format!("Failed to read manifest for cask '{}'", name))); artifact_removal_errors +=1; }
                    }
                } else {
                    log::warn!("No install manifest found for cask {}. Cannot perform clean uninstall based on recorded artifacts.", name);
                    // Don't add error here, just warn. Removal of caskroom dir is the main action.
                }

                 if artifact_removal_errors > 0 {
                     // Add a generic error if specific ones haven't been added yet
                     if !errors.iter().any(|e| matches!(e, SapphireError::Generic(s) if s.contains(&format!("cask '{}'", name)))) {
                         errors.push(SapphireError::Generic(format!("Failed to remove {} artifacts for cask '{}'", artifact_removal_errors, name)));
                     }
                 }

                log::debug!("Removing cask version directory: {}", cask_version_path.display());
                if let Err(e) = fs::remove_dir_all(&cask_version_path) {
                    log::warn!("Failed to remove cask version directory {}: {}", cask_version_path.display(), e);
                    errors.push(SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed to remove cask version dir for '{}'", name))));
                     pb.finish_and_clear(); // Clear spinner on error
                    continue; // Skip to next name
                }

                // Attempt to remove the parent cask directory if it's now empty
                let parent_cask_dir = config.cask_dir(&cask.token);
                if parent_cask_dir.exists() {
                    match std::fs::read_dir(&parent_cask_dir) {
                        Ok(mut entries) => {
                            if entries.next().is_none() { // Check if directory is empty
                                log::debug!("Removing empty parent cask directory: {}", parent_cask_dir.display());
                                if let Err(e) = std::fs::remove_dir(&parent_cask_dir) {
                                    log::warn!("Failed to remove empty parent cask directory {}: {}", parent_cask_dir.display(), e);
                                     // Optionally push a minor error
                                }
                            }
                        },
                        Err(e) => log::warn!("Failed to read parent cask directory {} to check if empty: {}", parent_cask_dir.display(), e)
                    }
                }

                // Check for errors before displaying success message
                if !errors.iter().any(|err| match err { // Check errors specific to this cask
                    SapphireError::Generic(s) => s.contains(name),
                    SapphireError::Io(io_err) => io_err.to_string().contains(name),
                    SapphireError::Json(_) => true, // Assume Json error relates to this cask parse
                    _ => false }) && artifact_removal_errors == 0 {

                   pb.finish_with_message(format!("Uninstalled {} (~{} files, ~{})", cask_version_path.display(), file_count, format_size(size_bytes)));
                } else {
                     pb.finish_and_clear(); // Clear spinner if there were errors
                }
                continue; // Successfully uninstalled cask
            }
            Err(SapphireError::NotFound(_)) => {
                log::error!("Formula or Cask '{}' not found.", name);
                 // Only add error if formula wasn't checked or also not found
                 if !errors.iter().any(|e| matches!(e, SapphireError::NotFound(_))) {
                     errors.push(SapphireError::NotFound(format!("Formula or Cask '{}' not found", name)));
                 }
                pb.finish_and_clear();
                continue;
            }
            Err(e) => {
                log::error!("Error getting cask info for '{}': {}", name, e);
                errors.push(e);
                pb.finish_and_clear();
                continue;
            }
        }
    }

    if !errors.is_empty() {
        eprintln!("Finished uninstalling with errors:");
        // Deduplicate and print errors concisely
        let unique_errors: std::collections::HashSet<_> = errors.iter().map(|e| e.to_string()).collect();
        for error_str in unique_errors { eprintln!("  - {}", error_str); }

        return Err(SapphireError::Generic(format!("Uninstall failed for {} package(s). See logs above.", errors.len())));
    }

    Ok(())
}

// ... (count_files_and_size, format_size remain unchanged) ...
fn count_files_and_size(path: &std::path::Path) -> Result<(usize, u64)> {
    let mut file_count = 0;
    let mut total_size = 0;
    for entry in walkdir::WalkDir::new(path) {
        match entry {
            Ok(entry_data) => {
                 // Count both files and symlinks towards the file count
                 if entry_data.file_type().is_file() || entry_data.file_type().is_symlink() {
                    match entry_data.metadata() {
                        Ok(metadata) => {
                             file_count += 1;
                             // Only add size for actual files, not symlinks
                             if entry_data.file_type().is_file() {
                                 total_size += metadata.len();
                             }
                        }
                        Err(e) => { log::warn!("Could not get metadata for {}: {}", entry_data.path().display(), e); }
                    }
                 } else if entry_data.file_type().is_dir() {
                     // Optionally count directories if needed, currently just files/links
                 }
            }
            Err(e) => { log::warn!("Error traversing directory {}: {}", path.display(), e); }
        }
    }
    Ok((file_count, total_size))
}
fn format_size(size: u64) -> String {
    const KB: u64 = 1024; const MB: u64 = KB * 1024; const GB: u64 = MB * 1024;
    if size >= GB { format!("{:.1}GB", size as f64 / GB as f64) }
    else if size >= MB { format!("{:.1}MB", size as f64 / MB as f64) }
    else if size >= KB { format!("{:.1}KB", size as f64 / KB as f64) }
    else { format!("{}B", size) }
}