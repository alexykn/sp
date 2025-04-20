use std::fs;
use std::path::Path; // Added PathBuf
use std::process::Command; // Added Command
use std::sync::Arc;

use colored::Colorize;
use sapphire_core::build;
use sapphire_core::build::cask::{CaskInstallManifest, InstalledArtifact};
use sapphire_core::fetch::api;
use sapphire_core::model::cask::Cask;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::{Result, SapphireError};
use {log, serde_json, walkdir};

use crate::cmd::info;
// Import the new cask manifest types
use crate::ui;

pub async fn run_uninstall(names: &[String], config: &Config, cache: Arc<Cache>) -> Result<()> {
    let mut errors: Vec<(String, SapphireError)> = Vec::new(); // Store errors per package name

    for name in names {
        let pb = ui::create_spinner(&format!("Uninstalling {}", name));

        let formula_result = info::get_formula_info(name, config, &cache).await;
        let is_formula = formula_result.is_ok();

        if is_formula {
            // --- Formula Uninstall Logic (largely unchanged, uses existing manifest) ---
            let formula = formula_result.unwrap(); // Safe unwrap due to is_ok() check
            log::debug!("Attempting to uninstall formula: {}", name);
            let cellar_path = config.formula_keg_path(formula.name(), &formula.version_str_full());

            if !cellar_path.exists() {
                log::warn!(
                    "Formula '{}' info found but keg missing: {}. Attempting cleanup.",
                    name,
                    cellar_path.display()
                );
                match build::formula::link::unlink_formula_artifacts(&formula, config) {
                    Ok(_) => pb.finish_with_message(format!(
                        "Unlinked remaining artifacts for {} (keg missing)",
                        name
                    )),
                    Err(e) => {
                        log::error!(
                            "Failed to unlink artifacts for missing keg {}: {}",
                            formula.name(),
                            e
                        );
                        errors.push((
                            name.to_string(),
                            SapphireError::NotFound(format!(
                                "Formula '{}' is not installed (no keg at {})",
                                name,
                                cellar_path.display()
                            )),
                        ));
                        pb.finish_and_clear();
                    }
                }
                continue;
            }

            let (file_count, size_bytes) = count_files_and_size(&cellar_path).unwrap_or((0, 0));
            let mut unlink_error: Option<SapphireError> = None;
            match build::formula::link::unlink_formula_artifacts(&formula, config) {
                Ok(_) => log::debug!("Successfully unlinked artifacts for {}", formula.name()),
                Err(e) => {
                    log::error!(
                        "Failed to unlink artifacts for {}: {}. Proceeding with keg removal.",
                        formula.name(),
                        e
                    );
                    unlink_error = Some(e);
                }
            }

            log::debug!("Removing formula keg directory: {}", cellar_path.display());
            if let Err(e) = fs::remove_dir_all(&cellar_path) {
                let removal_error = SapphireError::Io(std::io::Error::new(
                    e.kind(),
                    format!("Failed remove keg {}: {}", cellar_path.display(), e),
                ));
                log::error!("{}", removal_error);
                errors.push((name.to_string(), removal_error));
                pb.finish_and_clear();
                continue;
            }

            if let Some(e) = unlink_error {
                errors.push((name.to_string(), e)); // Record unlink error after successful removal
                pb.finish_and_clear();
            } else {
                pb.finish_with_message(format!(
                    "Uninstalled {} ({} files, {})",
                    cellar_path.display(),
                    file_count,
                    format_size(size_bytes)
                ));
            }
            continue; // Successfully uninstalled formula
        }

        // --- Cask Uninstall Logic (New Manifest-Based) ---
        match api::fetch_cask(name).await {
            // Fetch JSON definition
            Ok(cask_json) => {
                let cask: Cask = match serde_json::from_value(cask_json) {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!("Failed to parse cask JSON for {}: {}", name, e);
                        errors.push((name.to_string(), SapphireError::Json(e)));
                        pb.finish_and_clear();
                        continue;
                    }
                };
                log::debug!("Attempting to uninstall cask: {}", name);

                let installed_version = match cask.installed_version(config) {
                    Some(v) => v,
                    None => {
                        log::info!("Cask '{}' is not installed.", name);
                        // Avoid double "not found" if formula also wasn't found.
                        if errors
                            .iter()
                            .all(|(n, e)| n != name || !matches!(e, SapphireError::NotFound(_)))
                        {
                            // Only add error if it wasn't already marked as not found
                            errors.push((
                                name.to_string(),
                                SapphireError::NotFound(format!(
                                    "Cask '{}' is not installed",
                                    name
                                )),
                            ));
                        }
                        pb.finish_and_clear();
                        continue;
                    }
                };
                let cask_version_path = config.cask_version_path(&cask.token, &installed_version);

                if !cask_version_path.exists() {
                    log::error!(
                        "Cask '{}' version '{}' inconsistent: Dir missing: {}",
                        name,
                        installed_version,
                        cask_version_path.display()
                    );
                    errors.push((
                        name.to_string(),
                        SapphireError::NotFound(format!(
                            "Cask '{}' version '{}' installation is inconsistent (missing dir)",
                            name, installed_version
                        )),
                    ));
                    pb.finish_and_clear();
                    continue;
                }

                let (file_count, size_bytes) =
                    count_files_and_size(&cask_version_path).unwrap_or((0, 0));

                // --- Process CASK_INSTALL_MANIFEST.json ---
                let manifest_path = cask_version_path.join("CASK_INSTALL_MANIFEST.json");
                let mut artifact_removal_errors = 0;

                if manifest_path.is_file() {
                    log::debug!("Processing manifest: {}", manifest_path.display());
                    match fs::read_to_string(&manifest_path) {
                        Ok(manifest_str) => {
                            match serde_json::from_str::<CaskInstallManifest>(&manifest_str) {
                                Ok(manifest) => {
                                    for artifact in manifest.artifacts.iter().rev() {
                                        // Iterate backward for safety
                                        if !process_artifact_uninstall(artifact) {
                                            artifact_removal_errors += 1;
                                        }
                                    }
                                }
                                Err(e) => {
                                    log::warn!("Failed to parse cask manifest {}: {}. Falling back to directory removal only.", manifest_path.display(), e);
                                    errors.push((
                                        name.to_string(),
                                        SapphireError::Generic(format!(
                                            "Failed parse manifest for cask '{}'",
                                            name
                                        )),
                                    ));
                                    artifact_removal_errors += 1; // Count as error if manifest is
                                                                  // unparseable
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!("Failed to read cask manifest {}: {}. Falling back to directory removal only.", manifest_path.display(), e);
                            errors.push((
                                name.to_string(),
                                SapphireError::Generic(format!(
                                    "Failed read manifest for cask '{}'",
                                    name
                                )),
                            ));
                            artifact_removal_errors += 1; // Count as error if manifest cannot be
                                                          // read
                        }
                    }
                } else {
                    log::warn!("No CASK_INSTALL_MANIFEST.json found for cask {}. Uninstalling Caskroom directory only.", name);
                    // This is not necessarily an error, just a limitation. Don't increment
                    // artifact_removal_errors here.
                }

                // --- Remove Caskroom Version Directory ---
                log::debug!(
                    "Removing cask version directory: {}",
                    cask_version_path.display()
                );
                if let Err(e) = fs::remove_dir_all(&cask_version_path) {
                    log::error!(
                        "Failed to remove cask version directory {}: {}",
                        cask_version_path.display(),
                        e
                    );
                    errors.push((
                        name.to_string(),
                        SapphireError::Io(std::io::Error::new(
                            e.kind(),
                            format!("Failed to remove cask version dir for '{}'", name),
                        )),
                    ));
                    // If dir removal fails, stop processing this cask.
                    pb.finish_and_clear();
                    continue;
                }

                // --- Cleanup Parent Directory (if empty) ---
                let parent_cask_dir = config.cask_dir(&cask.token);
                cleanup_parent_cask_dir(&parent_cask_dir);

                // --- Final Message ---
                if artifact_removal_errors == 0 && errors.iter().all(|(n, _)| n != name) {
                    // Check errors specifically for this cask
                    pb.finish_with_message(format!(
                        "Uninstalled {} (~{} files, ~{})",
                        cask_version_path.display(),
                        file_count,
                        format_size(size_bytes)
                    ));
                } else {
                    log::warn!(
                        "Uninstalled {} but encountered {} artifact removal errors.",
                        cask_version_path.display(),
                        artifact_removal_errors
                    );
                    // Add generic error if needed
                    if errors.iter().all(|(n, _)| n != name) {
                        errors.push((
                            name.to_string(),
                            SapphireError::Generic(format!(
                                "Encountered {} errors removing artifacts for cask '{}'",
                                artifact_removal_errors, name
                            )),
                        ));
                    }
                    pb.finish_and_clear();
                }
                continue; // Successfully uninstalled cask (or attempted to)
            }
            Err(SapphireError::NotFound(_)) => {
                log::error!("Formula or Cask '{}' not found.", name);
                // Avoid double "not found" errors
                if errors
                    .iter()
                    .all(|(n, e)| n != name || !matches!(e, SapphireError::NotFound(_)))
                {
                    errors.push((
                        name.to_string(),
                        SapphireError::NotFound(format!("Formula or Cask '{}' not found", name)),
                    ));
                }
                pb.finish_and_clear();
                continue;
            }
            Err(e) => {
                log::error!("Error getting cask info for '{}': {}", name, e);
                errors.push((name.to_string(), e));
                pb.finish_and_clear();
                continue;
            }
        }
    } // End loop over names

    if !errors.is_empty() {
        eprintln!("\n{}:", "Finished uninstalling with errors".yellow());
        // Group errors by package name
        let mut errors_by_pkg: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for (pkg_name, error) in errors {
            errors_by_pkg
                .entry(pkg_name)
                .or_default()
                .push(error.to_string());
        }
        for (pkg_name, error_list) in errors_by_pkg {
            eprintln!("  Package '{}':", pkg_name.cyan());
            // Deduplicate errors for the same package
            let unique_errors: std::collections::HashSet<_> = error_list.into_iter().collect();
            for error_str in unique_errors {
                eprintln!("    - {}", error_str.red());
            }
        }

        return Err(SapphireError::Generic(format!(
            "Uninstall failed for one or more packages."
        )));
    }

    Ok(())
}

/// Helper function to process the uninstallation of a single artifact from the manifest.
/// Returns true on success, false on failure.
fn process_artifact_uninstall(artifact: &InstalledArtifact) -> bool {
    log::debug!("Uninstalling artifact: {:?}", artifact);
    match artifact {
        InstalledArtifact::App { path } => {
            remove_filesystem_artifact(path, true) // Use sudo for /Applications
        }
        InstalledArtifact::CaskroomLink { link_path, .. }
        | InstalledArtifact::BinaryLink { link_path, .. } => {
            remove_filesystem_artifact(link_path, false) // No sudo usually needed for links in
                                                         // PREFIX
        }
        InstalledArtifact::PkgUtilReceipt { id } => forget_pkgutil_receipt(id),
        InstalledArtifact::Launchd { label, path } => {
            unload_and_remove_launchd(label, path.as_deref())
        }
        InstalledArtifact::CaskroomReference { .. } => {
            log::debug!("Ignoring CaskroomReference artifact during uninstall.");
            true // Not an error to ignore this type
        } /* Add cases for other artifact types here
           * _ => {
           *     log::warn!("Uninstall not yet implemented for artifact type: {:?}", artifact);
           *     false // Consider unknown types as failure for now
           * } */
    }
}

/// Helper to remove a file or directory, trying sudo if necessary.
fn remove_filesystem_artifact(path: &Path, use_sudo: bool) -> bool {
    match path.symlink_metadata() {
        Ok(metadata) => {
            let is_dir = metadata.is_dir();
            log::debug!(
                "Removing {} at: {}",
                if is_dir { "directory" } else { "file/symlink" },
                path.display()
            );
            let remove_result = if is_dir {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };

            if let Err(e) = remove_result {
                if use_sudo
                    && (e.kind() == std::io::ErrorKind::PermissionDenied
                        || e.kind() == std::io::ErrorKind::DirectoryNotEmpty)
                {
                    log::warn!("Direct removal failed ({}). Trying with sudo rm -rf...", e);
                    let output = Command::new("sudo").arg("rm").arg("-rf").arg(path).output();
                    match output {
                        Ok(out) if out.status.success() => {
                            log::info!("Successfully removed {} with sudo.", path.display());
                            true
                        }
                        Ok(out) => {
                            log::error!(
                                "Failed to remove {} with sudo: {}",
                                path.display(),
                                String::from_utf8_lossy(&out.stderr)
                            );
                            false
                        }
                        Err(sudo_err) => {
                            log::error!(
                                "Error executing sudo rm for {}: {}",
                                path.display(),
                                sudo_err
                            );
                            false
                        }
                    }
                } else {
                    log::error!("Failed to remove artifact {}: {}", path.display(), e);
                    false
                }
            } else {
                log::debug!("Successfully removed artifact: {}", path.display());
                true
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::debug!("Artifact not found (already removed?): {}", path.display());
            true // Not finding it is success in uninstall context
        }
        Err(e) => {
            log::warn!(
                "Failed to get metadata for artifact {}: {}",
                path.display(),
                e
            );
            false // Fail if we can't even check metadata
        }
    }
}

/// Helper to forget a pkgutil receipt using sudo.
fn forget_pkgutil_receipt(id: &str) -> bool {
    log::info!("Forgetting package receipt (requires sudo): {}", id);
    let output = Command::new("sudo")
        .arg("pkgutil")
        .arg("--forget")
        .arg(id)
        .output();
    match output {
        Ok(out) => {
            if out.status.success() {
                log::debug!("Successfully forgot package receipt {}", id);
                true
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                // Don't log error if it's "No receipt found", that's okay.
                if !stderr.contains("No receipt for") {
                    log::error!("Failed to forget package receipt {}: {}", id, stderr);
                } else {
                    log::debug!("Package receipt {} already forgotten or never existed.", id);
                }
                // Even if not found, consider it "success" in uninstall context
                true
            }
        }
        Err(e) => {
            log::error!("Failed to execute sudo pkgutil --forget {}: {}", id, e);
            false
        }
    }
}

/// Helper to unload and optionally remove launchd plists.
fn unload_and_remove_launchd(label: &str, path: Option<&Path>) -> bool {
    log::info!("Unloading launchd agent/daemon (requires sudo): {}", label);
    let mut success = true;

    // Attempt to unload first
    let unload_output = Command::new("sudo")
        .arg("launchctl")
        .arg("unload")
        .arg("-w")
        .arg(label)
        .output();
    match unload_output {
        Ok(out) => {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                // Ignore "Could not find specified service" errors during unload
                if !stderr.contains("Could not find") && !stderr.contains("service not loaded") {
                    log::warn!("Failed to unload launchd item {}: {}", label, stderr);
                    success = false; // Mark as partial failure if unload fails unexpectedly
                } else {
                    log::debug!("Launchd item {} already unloaded or not found.", label);
                }
            } else {
                log::debug!("Successfully unloaded launchd item {}.", label);
            }
        }
        Err(e) => {
            log::error!(
                "Failed to execute sudo launchctl unload for {}: {}",
                label,
                e
            );
            success = false;
        }
    }

    // Remove the plist file if path is provided
    if let Some(plist_path) = path {
        log::debug!(
            "Attempting removal of launchd plist: {}",
            plist_path.display()
        );
        // Use the helper, assuming plist might need sudo depending on location
        if !remove_filesystem_artifact(plist_path, true) {
            log::warn!(
                "Failed to remove launchd plist file: {}",
                plist_path.display()
            );
            success = false; // Mark as partial failure if plist removal fails
        }
    }

    // Optionally try `launchctl remove <label>` if unload failed? Might be too aggressive.

    success
}

/// Helper to clean up empty parent cask directory.
fn cleanup_parent_cask_dir(parent_cask_dir: &Path) {
    if parent_cask_dir.exists() && parent_cask_dir.is_dir() {
        match std::fs::read_dir(parent_cask_dir) {
            Ok(mut entries) => {
                if entries.next().is_none() {
                    // Check if directory is empty
                    log::debug!(
                        "Removing empty parent cask directory: {}",
                        parent_cask_dir.display()
                    );
                    if let Err(e) = std::fs::remove_dir(parent_cask_dir) {
                        log::warn!(
                            "Failed to remove empty parent cask directory {}: {}",
                            parent_cask_dir.display(),
                            e
                        );
                    }
                } else {
                    log::debug!(
                        "Parent cask directory {} is not empty, skipping removal.",
                        parent_cask_dir.display()
                    );
                }
            }
            Err(e) => log::warn!(
                "Failed to read parent cask directory {} to check if empty: {}",
                parent_cask_dir.display(),
                e
            ),
        }
    }
}

// --- Unchanged Helper Functions ---
fn count_files_and_size(path: &std::path::Path) -> Result<(usize, u64)> {
    let mut file_count = 0;
    let mut total_size = 0;
    for entry in walkdir::WalkDir::new(path) {
        match entry {
            Ok(entry_data) => {
                if entry_data.file_type().is_file() || entry_data.file_type().is_symlink() {
                    match entry_data.metadata() {
                        Ok(metadata) => {
                            file_count += 1;
                            if entry_data.file_type().is_file() {
                                total_size += metadata.len();
                            }
                        }
                        Err(e) => {
                            log::warn!(
                                "Could not get metadata for {}: {}",
                                entry_data.path().display(),
                                e
                            );
                        }
                    }
                }
            }
            Err(e) => {
                log::warn!("Error traversing directory {}: {}", path.display(), e);
            }
        }
    }
    Ok((file_count, total_size))
}
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
