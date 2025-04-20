// ===== sapphire-core/src/build/cask/artifacts/zap.rs =====

use std::fs;
use std::path::PathBuf;
// Import Stdio for output redirection
use std::process::{Command, Stdio};

use tracing::{debug, warn};

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
                                                let target = expand_tilde(item, &home);
                                                debug!("Trashing {}...", target.display());
                                                // Redirect stdout and stderr to null
                                                let _ = Command::new("trash")
                                                    .arg(&target)
                                                    .stdout(Stdio::null()) // <--- Added
                                                    .stderr(Stdio::null()) // <--- Added
                                                    .status();
                                            }
                                        }
                                    }
                                    "delete" => {
                                        // fs::remove_file doesn't print to stdout/stderr,
                                        // errors are handled via Result. No change needed here.
                                        if let Some(arr) = val.as_array() {
                                            for item in arr.iter().filter_map(|v| v.as_str()) {
                                                let target = expand_tilde(item, &home);
                                                debug!("Deleting file {}...", target.display());
                                                if let Err(e) = fs::remove_file(&target) {
                                                    // Log error only if it's NOT file not found
                                                    if e.kind() != std::io::ErrorKind::NotFound {
                                                        warn!(
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
                                        // fs::remove_dir_all doesn't print to stdout/stderr.
                                        if let Some(arr) = val.as_array() {
                                            for item in arr.iter().filter_map(|v| v.as_str()) {
                                                let target = expand_tilde(item, &home);
                                                debug!(
                                                    "Removing directory {}...",
                                                    target.display()
                                                );
                                                if let Err(e) = fs::remove_dir_all(&target) {
                                                    // Log error only if it's NOT file not found
                                                    if e.kind() != std::io::ErrorKind::NotFound {
                                                        warn!(
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
                                                // Redirect stdout and stderr to null
                                                let _ = Command::new("pkgutil")
                                                    .arg("--forget")
                                                    .arg(item)
                                                    .stdout(Stdio::null()) // <--- Added
                                                    .stderr(Stdio::null()) // <--- Added
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
                                                let plist = home
                                                    .join("Library/LaunchAgents")
                                                    .join(format!("{}.plist", label));
                                                debug!(
                                                    "Unloading launchctl {}...",
                                                    plist.display()
                                                );
                                                // Redirect stdout and stderr to null
                                                let _ = Command::new("launchctl")
                                                    .arg("unload")
                                                    // Consider adding -w for persistent unload if
                                                    // needed
                                                    .arg(&plist)
                                                    .stdout(Stdio::null()) // <--- Added
                                                    .stderr(Stdio::null()) // <--- Added
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
                                            // Redirect stdout and stderr to null
                                            let _ = Command::new("sh")
                                                .arg("-c")
                                                .arg(cmd)
                                                .stdout(Stdio::null()) // <--- Added
                                                .stderr(Stdio::null()) // <--- Added
                                                .status();
                                        }
                                    }
                                    "signal" => {
                                        // Signals often target processes directly, less likely to
                                        // have stdout/stderr,
                                        // but redirecting won't hurt.
                                        if let Some(arr) = val.as_array() {
                                            for cmd in arr.iter().filter_map(|v| v.as_str()) {
                                                debug!("Running signal command: {}...", cmd);
                                                // Redirect stdout and stderr to null
                                                let _ = Command::new("sh")
                                                    .arg("-c")
                                                    .arg(cmd)
                                                    .stdout(Stdio::null()) // <--- Added
                                                    .stderr(Stdio::null()) // <--- Added
                                                    .status();
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
