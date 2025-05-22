// sps-core/src/check/installed.rs
use std::fs::{self}; // Removed DirEntry as it's not directly used here
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_common::keg::KegRegistry; // KegRegistry is used
use tracing::{debug, warn};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageType {
    Formula,
    Cask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackageInfo {
    pub name: String,
    pub version: String, // This will now store keg.version_str
    pub pkg_type: PackageType,
    pub path: PathBuf,
}

// Helper closure to handle io::Result<DirEntry> -> Option<DirEntry> logging errors
// Defined outside the functions to avoid repetition
fn handle_dir_entry(res: io::Result<fs::DirEntry>, dir_path_str: &str) -> Option<fs::DirEntry> {
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

    match keg_registry.list_installed_kegs() {
        Ok(kegs) => {
            for keg in kegs {
                installed.push(InstalledPackageInfo {
                    name: keg.name,
                    version: keg.version_str, // Use keg.version_str
                    pkg_type: PackageType::Formula,
                    path: keg.path,
                });
            }
        }
        Err(e) => warn!("Failed to list installed formulae: {}", e),
    }

    let caskroom_dir = config.cask_room_dir();
    if caskroom_dir.is_dir() {
        let caskroom_dir_str = caskroom_dir.to_str().unwrap_or("caskroom").to_string();
        let cask_token_entries_iter =
            fs::read_dir(&caskroom_dir).map_err(|e| SpsError::Io(Arc::new(e)))?;

        for entry_res in cask_token_entries_iter {
            if let Some(entry) = handle_dir_entry(entry_res, &caskroom_dir_str) {
                let cask_token_path = entry.path();
                if !cask_token_path.is_dir() {
                    continue;
                }
                let cask_token = entry.file_name().to_string_lossy().to_string();

                match fs::read_dir(&cask_token_path) {
                    Ok(version_entries_iter) => {
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
                                    let manifest_path =
                                        version_path.join("CASK_INSTALL_MANIFEST.json");
                                    let mut include = true;
                                    if manifest_path.is_file() {
                                        if let Ok(manifest_str) =
                                            std::fs::read_to_string(&manifest_path)
                                        {
                                            if let Ok(manifest_json) =
                                                serde_json::from_str::<serde_json::Value>(
                                                    &manifest_str,
                                                )
                                            {
                                                if let Some(is_installed) = manifest_json
                                                    .get("is_installed")
                                                    .and_then(|v| v.as_bool())
                                                {
                                                    include = is_installed;
                                                }
                                            }
                                        }
                                    }
                                    if include {
                                        installed.push(InstalledPackageInfo {
                                            name: cask_token.clone(),
                                            version: version_str,
                                            pkg_type: PackageType::Cask,
                                            path: version_path,
                                        });
                                    }
                                    // Assuming one actively installed version per cask token based
                                    // on manifest logic
                                    // If multiple version folders exist but only one manifest says
                                    // is_installed=true, this is fine.
                                    // If the intent is to list *all* version folders, the break
                                    // might be removed,
                                    // but then "is_installed" logic per version becomes more
                                    // important.
                                    // For now, finding the first "active" one is usually sufficient
                                    // for list/upgrade checks.
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
    let keg_registry = KegRegistry::new(config.clone());
    if let Some(keg) = keg_registry.get_installed_keg(name)? {
        return Ok(Some(InstalledPackageInfo {
            name: keg.name,
            version: keg.version_str, // Use keg.version_str
            pkg_type: PackageType::Formula,
            path: keg.path,
        }));
    }

    let cask_token_path = config.cask_room_token_path(name);
    if cask_token_path.is_dir() {
        let version_entries_iter =
            fs::read_dir(&cask_token_path).map_err(|e| SpsError::Io(Arc::new(e)))?;
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
                    let manifest_path = version_path.join("CASK_INSTALL_MANIFEST.json");
                    let mut include = true;
                    if manifest_path.is_file() {
                        if let Ok(manifest_str) = std::fs::read_to_string(&manifest_path) {
                            if let Ok(manifest_json) =
                                serde_json::from_str::<serde_json::Value>(&manifest_str)
                            {
                                if let Some(is_installed) =
                                    manifest_json.get("is_installed").and_then(|v| v.as_bool())
                                {
                                    include = is_installed;
                                }
                            }
                        }
                    }
                    if include {
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
    }
    Ok(None)
}
