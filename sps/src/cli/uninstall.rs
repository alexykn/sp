use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use clap::Args;
use colored::Colorize;
use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_common::model::cask::Cask;
use sps_common::Cache;
use sps_core::check::{installed, PackageType};
use sps_core::{uninstall as core_uninstall, UninstallOptions};
use sps_net::api;
use tracing::{debug, error, warn};
use {serde_json, walkdir};

#[derive(Args, Debug)]
pub struct Uninstall {
    /// The names of the formulas or casks to uninstall
    #[arg(required = true)] // Ensure at least one name is given
    pub names: Vec<String>,
    /// Perform a deep clean for casks, removing associated user data, caches,
    /// and configuration files. Use with caution, data will be lost! Ignored for formulas.
    #[arg(
        long,
        help = "Perform a deep clean for casks, removing associated user data, caches, and configuration files. Use with caution!"
    )]
    pub zap: bool,
}

impl Uninstall {
    pub async fn run(&self, config: &Config, cache: Arc<Cache>) -> Result<()> {
        let names = &self.names;
        let mut errors: Vec<(String, SpsError)> = Vec::new();

        for name in names {
            // Basic name validation to prevent path traversal
            if name.contains('/') || name.contains("..") {
                let msg = format!("Invalid package name '{name}' contains disallowed characters");
                error!("✖ {msg}");
                errors.push((name.to_string(), SpsError::Generic(msg)));
                continue;
            }

            println!("Uninstalling {name}...");

            match installed::get_installed_package(name, config).await {
                Ok(Some(installed_info)) => {
                    let (file_count, size_bytes) =
                        count_files_and_size(&installed_info.path).unwrap_or((0, 0));
                    let uninstall_opts = UninstallOptions { skip_zap: false };
                    debug!(
                        "Attempting uninstall for {} ({:?})",
                        name, installed_info.pkg_type
                    );
                    let uninstall_result = match installed_info.pkg_type {
                        PackageType::Formula => {
                            if self.zap {
                                warn!("--zap flag is ignored for formulas like '{}'.", name);
                            }
                            core_uninstall::uninstall_formula_artifacts(
                                &installed_info,
                                config,
                                &uninstall_opts,
                            )
                        }
                        PackageType::Cask => {
                            core_uninstall::uninstall_cask_artifacts(&installed_info, config)
                        }
                    };

                    if let Err(e) = uninstall_result {
                        error!("✖ Failed to uninstall '{}': {}", name.cyan(), e);
                        errors.push((name.to_string(), e));
                        // Continue to zap anyway for casks, as per plan
                    } else {
                        println!(
                            "✓ Uninstalled {:?} {} ({} files, {})",
                            installed_info.pkg_type,
                            name.green(),
                            file_count,
                            format_size(size_bytes)
                        );
                    }

                    // --- Zap Uninstall (Conditional) ---
                    if self.zap && installed_info.pkg_type == PackageType::Cask {
                        println!("Zapping {name}...");
                        debug!(
                            "--zap specified for cask '{}', attempting deep clean.",
                            name
                        );

                        // Fetch the Cask definition (needed for the zap stanza)
                        let cask_def_result: Result<Cask> = async {
                            match api::get_cask(name).await {
                                Ok(cask) => Ok(cask),
                                Err(e) => {
                                    warn!("Failed API fetch for zap definition for '{}' ({}), trying cache...", name, e);
                                    match cache.load_raw("cask.json") {
                                        Ok(raw_json) => {
                                            let casks: Vec<Cask> = serde_json::from_str(&raw_json)
                                                .map_err(|cache_e| SpsError::Cache(format!("Failed parse cached cask.json: {cache_e}")))?;
                                            casks.into_iter()
                                                .find(|c| c.token == *name)
                                                .ok_or_else(|| SpsError::NotFound(format!("Cask '{name}' def not in cache either")))
                                        }
                                        Err(cache_e) => Err(SpsError::Cache(format!("Failed load cask cache for zap: {cache_e}"))),
                                    }
                                }
                            }
                        }.await;

                        match cask_def_result {
                            Ok(cask_def) => {
                                match core_uninstall::zap_cask_artifacts(
                                    &installed_info,
                                    &cask_def,
                                    config,
                                )
                                .await
                                {
                                    Ok(_) => {
                                        println!("✓ Zap complete for {}", name.green());
                                    }
                                    Err(zap_err) => {
                                        error!("✖ Zap failed for '{}': {}", name.cyan(), zap_err);
                                        errors.push((format!("{name} (zap)"), zap_err));
                                    }
                                }
                            }
                            Err(e) => {
                                error!(
                                    "✖ Could not get Cask definition for zap '{}': {}",
                                    name.cyan(),
                                    e
                                );
                                errors.push((format!("{name} (zap definition)"), e));
                            }
                        }
                    }
                }
                Ok(None) => {
                    let msg = format!("Package '{name}' is not installed.");
                    error!("✖ {msg}");
                    errors.push((name.to_string(), SpsError::NotFound(msg)));
                }
                Err(e) => {
                    let msg = format!("Failed check install status for '{name}': {e}");
                    error!("✖ {msg}");
                    errors.push((name.clone(), SpsError::Generic(msg)));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            eprintln!("\n{}:", "Finished uninstalling with errors".yellow());
            let mut errors_by_pkg: HashMap<String, Vec<String>> = HashMap::new();
            for (pkg_name, error) in errors {
                errors_by_pkg
                    .entry(pkg_name)
                    .or_default()
                    .push(error.to_string());
            }
            for (pkg_name, error_list) in errors_by_pkg {
                eprintln!("Package '{}':", pkg_name.cyan());
                let unique_errors: HashSet<_> = error_list.into_iter().collect();
                for error_str in unique_errors {
                    eprintln!("- {}", error_str.red());
                }
            }
            Err(SpsError::Generic(
                "Uninstall failed for one or more packages.".to_string(),
            ))
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
                            tracing::warn!(
                                "Could not get metadata for {}: {}",
                                entry_data.path().display(),
                                e
                            );
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Error traversing directory {}: {}", path.display(), e);
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
        format!("{size}B")
    }
}
