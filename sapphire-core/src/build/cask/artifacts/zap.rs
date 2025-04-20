// ===== sapphire-core/src/build/cask/artifacts/zap.rs =====

use crate::model::cask::Cask;
use crate::build::cask::InstalledArtifact;
use crate::utils::config::Config;
use crate::utils::error::Result;
use log::{info, warn};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Implements the `zap` stanza by performing deep-clean actions
/// such as trash, delete, rmdir, pkgutil forget, launchctl unload,
/// and arbitrary scripts, matching Homebrew's Cask behavior.
pub fn install_zap(
    cask: &Cask,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut artifacts = Vec::new();
    let home = config.home_dir();

    if let Some(entries) = &cask.artifacts {
        for entry in entries {
            if let Some(obj) = entry.as_object() {
                if let Some(zaps) = obj.get("zap").and_then(|v| v.as_array()) {
                    for zap_map in zaps {
                        if let Some(zap_obj) = zap_map.as_object() {
                            for (key, val) in zap_obj {
                                match key.as_str() {
                                    "trash" => {
                                        if let Some(arr) = val.as_array() {
                                            for item in arr.iter().filter_map(|v| v.as_str()) {
                                                let target = expand_tilde(item, &home);
                                                info!("Trashing {}...", target.display());
                                                let _ = Command::new("trash").arg(&target).status();
                                            }
                                        }
                                    }
                                    "delete" => {
                                        if let Some(arr) = val.as_array() {
                                            for item in arr.iter().filter_map(|v| v.as_str()) {
                                                let target = expand_tilde(item, &home);
                                                info!("Deleting file {}...", target.display());
                                                let _ = fs::remove_file(&target);
                                            }
                                        }
                                    }
                                    "rmdir" => {
                                        if let Some(arr) = val.as_array() {
                                            for item in arr.iter().filter_map(|v| v.as_str()) {
                                                let target = expand_tilde(item, &home);
                                                info!("Removing directory {}...", target.display());
                                                let _ = fs::remove_dir_all(&target);
                                            }
                                        }
                                    }
                                    "pkgutil" => {
                                        if let Some(arr) = val.as_array() {
                                            for item in arr.iter().filter_map(|v| v.as_str()) {
                                                info!("Forgetting pkgutil receipt {}...", item);
                                                let _ = Command::new("pkgutil").arg("--forget").arg(item).status();
                                                artifacts.push(InstalledArtifact::PkgUtilReceipt { id: item.to_string() });
                                            }
                                        }
                                    }
                                    "launchctl" => {
                                        if let Some(arr) = val.as_array() {
                                            for label in arr.iter().filter_map(|v| v.as_str()) {
                                                let plist = home.join("Library/LaunchAgents").join(format!("{}.plist", label));
                                                info!("Unloading launchctl {}...", plist.display());
                                                let _ = Command::new("launchctl").arg("unload").arg(&plist).status();
                                                artifacts.push(InstalledArtifact::Launchd { label: label.to_string(), path: Some(plist) });
                                            }
                                        }
                                    }
                                    "script" => {
                                        if let Some(cmd) = val.as_str() {
                                            info!("Running zap script: {}...", cmd);
                                            let _ = Command::new("sh").arg("-c").arg(cmd).status();
                                        }
                                    }
                                    "signal" => {
                                        if let Some(arr) = val.as_array() {
                                            for cmd in arr.iter().filter_map(|v| v.as_str()) {
                                                info!("Running signal command: {}...", cmd);
                                                let _ = Command::new("sh").arg("-c").arg(cmd).status();
                                            }
                                        }
                                    }
                                    _ => warn!("Unsupported zap key '{}', skipping", key),
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(artifacts)
}

/// Expand a path that may start with '~' to the user's home directory
fn expand_tilde(path: &str, home: &PathBuf) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        home.join(stripped)
    } else {
        PathBuf::from(path)
    }
}