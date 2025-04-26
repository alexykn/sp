// ===== spm-core/src/build/cask/artifacts/zap.rs =====

use std::fs;
use std::path::{Path, PathBuf}; // Import Path
use std::process::{Command, Stdio};

use regex::Regex;
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
                                                if !is_safe_path(&target, &home) {
                                                    debug!(
                                                        "Unsafe trash path {}, skipping",
                                                        target.display()
                                                    );
                                                    continue;
                                                }
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
                                                if !is_safe_path(&target, &home) {
                                                    debug!(
                                                        "Unsafe delete path {}, skipping",
                                                        target.display()
                                                    );
                                                    continue;
                                                }
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
                                                if !is_safe_path(&target, &home) {
                                                    debug!(
                                                        "Unsafe rmdir path {}, skipping",
                                                        target.display()
                                                    );
                                                    continue;
                                                }
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
                                                if !is_valid_pkgid(item) {
                                                    debug!(
                                                        "Invalid pkgutil id '{}', skipping",
                                                        item
                                                    );
                                                    continue;
                                                }
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
                                                if !is_valid_label(label) {
                                                    debug!(
                                                        "Invalid launchctl label '{}', skipping",
                                                        label
                                                    );
                                                    continue;
                                                }
                                                let plist = home // Use expanded home
                                                    .join("Library/LaunchAgents")
                                                    .join(format!("{label}.plist"));
                                                if !is_safe_path(&plist, &home) {
                                                    debug!("Unsafe plist path {} for label {}, skipping", plist.display(), label);
                                                    continue;
                                                }
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
                                            if !is_valid_command(cmd) {
                                                debug!(
                                                    "Invalid zap script command '{}', skipping",
                                                    cmd
                                                );
                                                continue;
                                            }
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
                                                if !is_valid_command(cmd) {
                                                    debug!(
                                                        "Invalid signal command '{}', skipping",
                                                        cmd
                                                    );
                                                    continue;
                                                }
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

// New helper functions to validate paths and strings.
fn is_safe_path(path: &Path, home: &Path) -> bool {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let path_str = path.to_string_lossy();
    if path.is_absolute()
        && (path_str.starts_with("/Applications") || path_str.starts_with("/Library"))
    {
        return true;
    }
    if path.starts_with(home) {
        return true;
    }
    if path_str.contains("Caskroom/Cellar") {
        return true;
    }
    false
}

fn is_valid_pkgid(pkgid: &str) -> bool {
    let re = Regex::new(r"^[a-zA-Z0-9.-]+$").unwrap();
    re.is_match(pkgid)
}

fn is_valid_label(label: &str) -> bool {
    let re = Regex::new(r"^[a-zA-Z0-9.-]+$").unwrap();
    re.is_match(label)
}

fn is_valid_command(cmd: &str) -> bool {
    let re = Regex::new(r"^[a-zA-Z0-9\s\-_./]+$").unwrap();
    re.is_match(cmd)
}

/// Expand a path that may start with '~' to the user's home directory
fn expand_tilde(path: &str, home: &Path) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        home.join(stripped)
    } else {
        PathBuf::from(path)
    }
}
