use std::path::PathBuf;

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::error::Result;

/// At install time, scan the `uninstall` stanza and turn each directive
/// into an InstalledArtifact variant, so it can later be torn down.
pub fn record_uninstall(cask: &Cask) -> Result<Vec<InstalledArtifact>> {
    let mut artifacts = Vec::new();

    if let Some(entries) = &cask.artifacts {
        for entry in entries.iter().filter_map(|v| v.as_object()) {
            if let Some(steps) = entry.get("uninstall").and_then(|v| v.as_array()) {
                for step in steps.iter().filter_map(|v| v.as_object()) {
                    for (key, val) in step {
                        match key.as_str() {
                            "pkgutil" => {
                                if let Some(id) = val.as_str() {
                                    artifacts.push(InstalledArtifact::PkgUtilReceipt {
                                        id: id.to_string(),
                                    });
                                }
                            }
                            "delete" => {
                                if let Some(arr) = val.as_array() {
                                    for p in arr.iter().filter_map(|v| v.as_str()) {
                                        artifacts.push(InstalledArtifact::App {
                                            path: PathBuf::from(p),
                                        });
                                    }
                                }
                            }
                            "rmdir" => {
                                if let Some(arr) = val.as_array() {
                                    for p in arr.iter().filter_map(|v| v.as_str()) {
                                        artifacts.push(InstalledArtifact::App {
                                            path: PathBuf::from(p),
                                        });
                                    }
                                }
                            }
                            "launchctl" => {
                                if let Some(arr) = val.as_array() {
                                    for lbl in arr.iter().filter_map(|v| v.as_str()) {
                                        artifacts.push(InstalledArtifact::Launchd {
                                            label: lbl.to_string(),
                                            path: None,
                                        });
                                    }
                                }
                            }
                            // Add other uninstall keys similarly...
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    Ok(artifacts)
}
