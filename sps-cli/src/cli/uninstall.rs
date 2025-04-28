use std::sync::Arc;

use clap::Args;
use colored::Colorize;
use sps_common::Cache;
use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_core::{PackageType, UninstallOptions, installed, uninstall as core_uninstall};
use tracing::{debug, error}; // Removed warn
use walkdir;

use crate::ui;

#[derive(Args, Debug)]
pub struct Uninstall {
    /// The names of the formulas or casks to uninstall
    #[arg(required = true)] // Ensure at least one name is given
    pub names: Vec<String>,
}

impl Uninstall {
    pub async fn run(&self, config: &Config, _cache: Arc<Cache>) -> Result<()> {
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

            let pb = ui::create_spinner(&format!("Uninstalling {name}"));

            match installed::get_installed_package(name, config).await? {
                Some(installed_info) => {
                    let (file_count, size_bytes) =
                        count_files_and_size(&installed_info.path).unwrap_or((0, 0));
                    let uninstall_opts = UninstallOptions { skip_zap: false }; // Explicit uninstall includes zap
                    debug!(
                        "Attempting uninstall for {} ({:?})",
                        name, installed_info.pkg_type
                    );
                    let uninstall_result = match installed_info.pkg_type {
                        PackageType::Formula => core_uninstall::uninstall_formula_artifacts(
                            &installed_info,
                            config,
                            &uninstall_opts,
                        ),
                        PackageType::Cask => core_uninstall::uninstall_cask_artifacts(
                            &installed_info,
                            config,
                            &uninstall_opts,
                        ),
                    };

                    if let Err(e) = uninstall_result {
                        error!("✖ Failed to uninstall '{}': {}", name.cyan(), e);
                        errors.push((name.to_string(), e));
                        pb.finish_and_clear();
                    } else {
                        pb.finish_with_message(format!(
                            "✓ Uninstalled {:?} {} ({} files, {})",
                            installed_info.pkg_type,
                            name.green(),
                            file_count,
                            format_size(size_bytes)
                        ));
                    }
                }
                None => {
                    let msg = format!("Package '{name}' is not installed.");
                    error!("✖ {msg}");
                    errors.push((name.to_string(), SpsError::NotFound(msg)));
                    pb.finish_and_clear();
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            eprintln!("\n{}:", "Finished uninstalling with errors".yellow());
            let mut errors_by_pkg: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            for (pkg_name, error) in errors {
                errors_by_pkg
                    .entry(pkg_name)
                    .or_default()
                    .push(error.to_string());
            }
            for (pkg_name, error_list) in errors_by_pkg {
                eprintln!("Package '{}':", pkg_name.cyan());
                let unique_errors: std::collections::HashSet<_> = error_list.into_iter().collect();
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
