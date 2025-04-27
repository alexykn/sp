// sp-core/src/installed.rs
use std::fs::{self, DirEntry}; // Keep DirEntry
use std::io; // Keep io for Error type
use std::path::PathBuf;
use std::sync::Arc; // Keep Arc

use serde::{Deserialize, Serialize};
use sp_common::config::Config;
use sp_common::error::{Result, SpError};
use sp_common::keg::KegRegistry;
use tracing::{debug, warn};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageType {
    Formula,
    Cask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackageInfo {
    pub name: String,
    pub version: String,
    pub pkg_type: PackageType,
    pub path: PathBuf,
}

// Helper closure to handle io::Result<DirEntry> -> Option<DirEntry> logging errors
// Defined outside the functions to avoid repetition
fn handle_dir_entry(res: io::Result<DirEntry>, dir_path_str: &str) -> Option<DirEntry> {
    match res {
        Ok(entry) => Some(entry),
        Err(e) => {
            warn!("Error reading entry in {}: {}", dir_path_str, e);
            None
        }
    }
}

pub async fn get_installed_packages(config: &Config) -> Result<Vec<InstalledPackageInfo>> {
    let mut installed = Vec::new();
    let keg_registry = KegRegistry::new(config.clone());

    // Get Formulae (Sync)
    match keg_registry.list_installed_kegs() {
        Ok(kegs) => {
            for keg in kegs {
                let version_str = keg
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| format!("{}_{}", keg.version, keg.revision));
                installed.push(InstalledPackageInfo {
                    name: keg.name,
                    version: version_str,
                    pkg_type: PackageType::Formula,
                    path: keg.path,
                });
            }
        }
        Err(e) => warn!("Failed to list installed formulae: {}", e),
    }

    // Get Casks (Sync Part - Reading Dirs)
    let caskroom_dir = config.caskroom_dir();
    if caskroom_dir.is_dir() {
        let caskroom_dir_str = caskroom_dir.to_str().unwrap_or("caskroom").to_string();
        let cask_token_entries_iter =
            fs::read_dir(&caskroom_dir).map_err(|e| SpError::Io(Arc::new(e)))?;

        // *** FIX for E0631: Explicit loop and match ***
        for entry_res in cask_token_entries_iter {
            if let Some(entry) = handle_dir_entry(entry_res, &caskroom_dir_str) {
                let cask_token_path = entry.path();
                if !cask_token_path.is_dir() {
                    continue;
                }
                let cask_token = entry.file_name().to_string_lossy().to_string();

                match fs::read_dir(&cask_token_path) {
                    Ok(version_entries_iter) => {
                        // *** FIX for E0631: Explicit loop and match ***
                        for version_entry_res in version_entries_iter {
                            if let Some(version_entry) = handle_dir_entry(
                                version_entry_res,
                                cask_token_path.to_str().unwrap_or("token_path"),
                            ) {
                                let version_path = version_entry.path();
                                if version_path.is_dir()
                                    && version_path.join("CASK_INSTALL_MANIFEST.json").is_file()
                                {
                                    let version_str =
                                        version_entry.file_name().to_string_lossy().to_string();
                                    installed.push(InstalledPackageInfo {
                                        name: cask_token.clone(),
                                        version: version_str,
                                        pkg_type: PackageType::Cask,
                                        path: version_path,
                                    });
                                    break; // Assume one installed version
                                }
                            }
                        }
                    }
                    Err(e) => warn!("Failed to read cask versions for {}: {}", cask_token, e),
                }
            }
        }
    } else {
        debug!(
            "Caskroom directory {} does not exist.",
            caskroom_dir.display()
        );
    }
    Ok(installed)
}

pub async fn get_installed_package(
    name: &str,
    config: &Config,
) -> Result<Option<InstalledPackageInfo>> {
    // Check Formula (Sync - ok)
    let keg_registry = KegRegistry::new(config.clone());
    if let Some(keg) = keg_registry.get_installed_keg(name)? {
        let version_str = keg
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("{}_{}", keg.version, keg.revision));
        return Ok(Some(InstalledPackageInfo {
            name: keg.name,
            version: version_str,
            pkg_type: PackageType::Formula,
            path: keg.path,
        }));
    }

    // Check Cask (Sync Part - Reading Dirs)
    let cask_token_path = config.cask_dir(name);
    if cask_token_path.is_dir() {
        let version_entries_iter =
            fs::read_dir(&cask_token_path).map_err(|e| SpError::Io(Arc::new(e)))?;
        // *** FIX for E0631: Explicit loop and match ***
        for version_entry_res in version_entries_iter {
            if let Some(version_entry) = handle_dir_entry(
                version_entry_res,
                cask_token_path.to_str().unwrap_or("token_path"),
            ) {
                let version_path = version_entry.path();
                if version_path.is_dir()
                    && version_path.join("CASK_INSTALL_MANIFEST.json").is_file()
                {
                    let version_str = version_entry.file_name().to_string_lossy().to_string();
                    return Ok(Some(InstalledPackageInfo {
                        name: name.to_string(),
                        version: version_str,
                        pkg_type: PackageType::Cask,
                        path: version_path,
                    }));
                }
            }
        }
    }
    Ok(None) // Not found
}
