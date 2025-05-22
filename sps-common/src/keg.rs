// sps-common/src/keg.rs
use std::fs;
use std::path::PathBuf;

// Corrected tracing imports: added error, removed unused debug
use tracing::{debug, error, warn};

use super::config::Config;
use super::error::{Result, SpsError};

/// Represents information about an installed package (Keg).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledKeg {
    pub name: String,
    pub version_str: String,
    pub path: PathBuf,
}

/// Manages querying installed packages in the Cellar.
#[derive(Debug)]
pub struct KegRegistry {
    config: Config,
}

impl KegRegistry {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    fn formula_cellar_path(&self, name: &str) -> PathBuf {
        self.config.cellar_dir().join(name)
    }

    pub fn get_opt_path(&self, name: &str) -> PathBuf {
        self.config.opt_dir().join(name)
    }

    pub fn get_installed_keg(&self, name: &str) -> Result<Option<InstalledKeg>> {
        let formula_dir = self.formula_cellar_path(name);
        debug!(
            "[KEG_REGISTRY:{}] get_installed_keg: Checking for formula '{}'. Path to check: {}",
            name,
            name,
            formula_dir.display()
        );

        if !formula_dir.is_dir() {
            debug!("[KEG_REGISTRY:{}] get_installed_keg: Formula directory '{}' NOT FOUND or not a directory. Returning None.", name, formula_dir.display());
            return Ok(None);
        }

        let mut latest_keg: Option<InstalledKeg> = None;
        debug!(
            "[KEG_REGISTRY:{}] get_installed_keg: Reading entries in formula directory '{}'",
            name,
            formula_dir.display()
        );

        match fs::read_dir(&formula_dir) {
            Ok(entries) => {
                for entry_result in entries {
                    match entry_result {
                        Ok(entry) => {
                            let path = entry.path();
                            debug!(
                                "[KEG_REGISTRY:{}] get_installed_keg: Examining entry '{}'",
                                name,
                                path.display()
                            );

                            if path.is_dir() {
                                if let Some(version_str_full) =
                                    path.file_name().and_then(|n| n.to_str())
                                {
                                    debug!("[KEG_REGISTRY:{}] get_installed_keg: Entry is a directory with name: {}", name, version_str_full);

                                    let current_keg_candidate = InstalledKeg {
                                        name: name.to_string(),
                                        version_str: version_str_full.to_string(),
                                        path: path.clone(),
                                    };

                                    match latest_keg {
                                        Some(ref current_latest) => {
                                            // Compare &str with &str
                                            if version_str_full
                                                > current_latest.version_str.as_str()
                                            {
                                                debug!("[KEG_REGISTRY:{}] get_installed_keg: Updating latest keg (lexicographical) to: {}", name, path.display());
                                                latest_keg = Some(current_keg_candidate);
                                            }
                                        }
                                        None => {
                                            debug!("[KEG_REGISTRY:{}] get_installed_keg: Setting first found keg as latest: {}", name, path.display());
                                            latest_keg = Some(current_keg_candidate);
                                        }
                                    }
                                } else {
                                    debug!("[KEG_REGISTRY:{}] get_installed_keg: Could not get filename as string for path '{}'", name, path.display());
                                }
                            } else {
                                debug!("[KEG_REGISTRY:{}] get_installed_keg: Entry '{}' is not a directory.", name, path.display());
                            }
                        }
                        Err(e) => {
                            warn!("[KEG_REGISTRY:{}] get_installed_keg: Error reading a directory entry in '{}': {}. Skipping entry.", name, formula_dir.display(), e);
                        }
                    }
                }
            }
            Err(e) => {
                // Corrected macro usage
                error!("[KEG_REGISTRY:{}] get_installed_keg: Failed to read_dir for formula directory '{}' (error: {}). Returning error.", name, formula_dir.display(), e);
                return Err(SpsError::Io(std::sync::Arc::new(e)));
            }
        }

        if let Some(keg) = &latest_keg {
            // Corrected format string arguments
            debug!("[KEG_REGISTRY:{}] get_installed_keg: For formula '{}', latest keg found: path={}, version_str={}", name, name, keg.path.display(), keg.version_str);
        } else {
            // Corrected format string arguments
            debug!("[KEG_REGISTRY:{}] get_installed_keg: For formula '{}', no installable keg version found in directory '{}'.", name, name, formula_dir.display());
        }
        Ok(latest_keg)
    }

    pub fn list_installed_kegs(&self) -> Result<Vec<InstalledKeg>> {
        let mut installed_kegs = Vec::new();
        let cellar_dir = self.cellar_path();
        debug!(
            "[KEG_REGISTRY] list_installed_kegs: Scanning cellar: {}",
            cellar_dir.display()
        );

        if !cellar_dir.is_dir() {
            debug!("[KEG_REGISTRY] list_installed_kegs: Cellar directory NOT FOUND. Returning empty list.");
            return Ok(installed_kegs);
        }

        for formula_entry_res in fs::read_dir(cellar_dir)? {
            let formula_entry = match formula_entry_res {
                Ok(fe) => fe,
                Err(e) => {
                    warn!("[KEG_REGISTRY] list_installed_kegs: Error reading entry in cellar: {}. Skipping.", e);
                    continue;
                }
            };
            let formula_path = formula_entry.path();
            debug!(
                "[KEG_REGISTRY] list_installed_kegs: Examining formula path: {}",
                formula_path.display()
            );

            if formula_path.is_dir() {
                if let Some(formula_name) = formula_path.file_name().and_then(|n| n.to_str()) {
                    debug!(
                        "[KEG_REGISTRY] list_installed_kegs: Found formula directory: {}",
                        formula_name
                    );
                    match fs::read_dir(&formula_path) {
                        Ok(version_entries) => {
                            for version_entry_res in version_entries {
                                let version_entry = match version_entry_res {
                                    Ok(ve) => ve,
                                    Err(e) => {
                                        warn!("[KEG_REGISTRY:{}] list_installed_kegs: Error reading version entry in '{}': {}. Skipping.", formula_name, formula_path.display(), e);
                                        continue;
                                    }
                                };
                                let version_path = version_entry.path();
                                debug!("[KEG_REGISTRY:{}] list_installed_kegs: Examining version path: {}", formula_name, version_path.display());

                                if version_path.is_dir() {
                                    if let Some(version_str_full) =
                                        version_path.file_name().and_then(|n| n.to_str())
                                    {
                                        debug!("[KEG_REGISTRY:{}] list_installed_kegs: Found version directory '{}' with name: {}", formula_name, version_path.display(), version_str_full);
                                        installed_kegs.push(InstalledKeg {
                                            name: formula_name.to_string(),
                                            version_str: version_str_full.to_string(),
                                            path: version_path.clone(),
                                        });
                                    } else {
                                        debug!("[KEG_REGISTRY:{}] list_installed_kegs: Could not get filename for version path {}", formula_name, version_path.display());
                                    }
                                } else {
                                    debug!("[KEG_REGISTRY:{}] list_installed_kegs: Version path {} is not a directory.", formula_name, version_path.display());
                                }
                            }
                        }
                        Err(e) => {
                            warn!("[KEG_REGISTRY:{}] list_installed_kegs: Failed to read_dir for formula versions in '{}': {}.", formula_name, formula_path.display(), e);
                        }
                    }
                } else {
                    debug!("[KEG_REGISTRY] list_installed_kegs: Could not get filename for formula path {}", formula_path.display());
                }
            } else {
                debug!(
                    "[KEG_REGISTRY] list_installed_kegs: Formula path {} is not a directory.",
                    formula_path.display()
                );
            }
        }
        debug!(
            "[KEG_REGISTRY] list_installed_kegs: Found {} total installed keg versions.",
            installed_kegs.len()
        );
        Ok(installed_kegs)
    }

    pub fn cellar_path(&self) -> PathBuf {
        self.config.cellar_dir()
    }

    pub fn get_keg_path(&self, name: &str, version_str_raw: &str) -> PathBuf {
        self.formula_cellar_path(name).join(version_str_raw)
    }
}
