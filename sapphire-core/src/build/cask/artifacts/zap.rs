// ===== sapphire-core/src/build/cask/artifacts/zap.rs =====

use std::fs;
use std::path::{Path, PathBuf}; // Import Path
use std::process::{Command, Stdio};

use tracing::debug;

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::Result;

/// Implements the `zap` stanza by performing deep-clean actions
/// such as trash, delete, rmdir, pkgutil forget, launchctl unload,
/// and arbitrary scripts, matching Homebrew's Cask behavior.
pub fn install_zap(cask: &Cask, config: &Config) -> Result<Vec<InstalledArtifact>> {
    let mut artifacts: Vec<InstalledArtifact> = Vec::new();
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
                                                let target = expand_tilde(item, &home); // Pass &Path
                                                debug!("Trashing {}...", target.display());
                                                let _ = Command::new("trash")
                                                    .arg(&target)
                                                    .stdout(Stdio::null())
                                                    .stderr(Stdio::null())
                                                    .status();
                                            }
                                        }
                                    }
                                    "delete" => {
                                        if let Some(arr) = val.as_array() {
                                            for item in arr.iter().filter_map(|v| v.as_str()) {
                                                let target = expand_tilde(item, &home); // Pass &Path
                                                debug!("Deleting file {}...", target.display());
                                                if let Err(e) = fs::remove_file(&target) {
                                                    if e.kind() != std::io::ErrorKind::NotFound {
                                                        debug!(
                                                            "Failed to delete {}: {}",
                                                            target.display(),
                                                            e
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    "rmdir" => {
                                        if let Some(arr) = val.as_array() {
                                            for item in arr.iter().filter_map(|v| v.as_str()) {
                                                let target = expand_tilde(item, &home); // Pass &Path
                                                debug!(
                                                    "Removing directory {}...",
                                                    target.display()
                                                );
                                                if let Err(e) = fs::remove_dir_all(&target) {
                                                    if e.kind() != std::io::ErrorKind::NotFound {
                                                        debug!(
                                                            "Failed to rmdir {}: {}",
                                                            target.display(),
                                                            e
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    "pkgutil" => {
                                        if let Some(arr) = val.as_array() {
                                            for item in arr.iter().filter_map(|v| v.as_str()) {
                                                debug!("Forgetting pkgutil receipt {}...", item);
                                                let _ = Command::new("pkgutil")
                                                    .arg("--forget")
                                                    .arg(item)
                                                    .stdout(Stdio::null())
                                                    .stderr(Stdio::null())
                                                    .status();
                                                artifacts.push(InstalledArtifact::PkgUtilReceipt {
                                                    id: item.to_string(),
                                                });
                                            }
                                        }
                                    }
                                    "launchctl" => {
                                        if let Some(arr) = val.as_array() {
                                            for label in arr.iter().filter_map(|v| v.as_str()) {
                                                let plist = home // Use expanded home
                                                    .join("Library/LaunchAgents")
                                                    .join(format!("{label}.plist"));
                                                debug!(
                                                    "Unloading launchctl {}...",
                                                    plist.display()
                                                );
                                                let _ = Command::new("launchctl")
                                                    .arg("unload")
                                                    .arg(&plist)
                                                    .stdout(Stdio::null())
                                                    .stderr(Stdio::null())
                                                    .status();
                                                artifacts.push(InstalledArtifact::Launchd {
                                                    label: label.to_string(),
                                                    path: Some(plist),
                                                });
                                            }
                                        }
                                    }
                                    "script" => {
                                        if let Some(cmd) = val.as_str() {
                                            debug!("Running zap script: {}...", cmd);
                                            let _ = Command::new("sh")
                                                .arg("-c")
                                                .arg(cmd)
                                                .stdout(Stdio::null())
                                                .stderr(Stdio::null())
                                                .status();
                                        }
                                    }
                                    "signal" => {
                                        if let Some(arr) = val.as_array() {
                                            for cmd in arr.iter().filter_map(|v| v.as_str()) {
                                                debug!("Running signal command: {}...", cmd);
                                                let _ = Command::new("sh")
                                                    .arg("-c")
                                                    .arg(cmd)
                                                    .stdout(Stdio::null())
                                                    .stderr(Stdio::null())
                                                    .status();
                                            }
                                        }
                                    }
                                    _ => debug!("Unsupported zap key '{}', skipping", key),
                                }
                            }
                        }
                    }
                    // Only process the first "zap" stanza found
                    break;
                }
            }
        }
    }

    Ok(artifacts)
}

/// Expand a path that may start with '~' to the user's home directory
fn expand_tilde(path: &str, home: &Path) -> PathBuf {
    // Changed to &Path
    if let Some(stripped) = path.strip_prefix("~/") {
        home.join(stripped)
    } else {
        PathBuf::from(path)
    }
}
