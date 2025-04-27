// ===== sp-core/src/build/cask/artifacts/input_method.rs =====

use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::Path;

use sp_common::config::Config;
use sp_common::error::Result;
use sp_common::model::cask::Cask;

use crate::build::cask::{InstalledArtifact, write_cask_manifest};

/// Install `input_method` artifacts from the staged directory into
/// `~/Library/Input Methods` and record installed artifacts.
pub fn install_input_method(
    cask: &Cask,
    stage_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    // Ensure we have an array of input_method names
    if let Some(artifacts_def) = &cask.artifacts {
        for artifact_value in artifacts_def {
            if let Some(obj) = artifact_value.as_object() {
                if let Some(names) = obj.get("input_method").and_then(|v| v.as_array()) {
                    for name_val in names {
                        if let Some(name) = name_val.as_str() {
                            let source = stage_path.join(name);
                            if source.exists() {
                                // Target directory: ~/Library/Input Methods
                                let target_dir =
                                    config.home_dir().join("Library").join("Input Methods");
                                if !target_dir.exists() {
                                    fs::create_dir_all(&target_dir)?;
                                }
                                let target = target_dir.join(name);

                                // Remove existing input method if present
                                if target.exists() {
                                    fs::remove_dir_all(&target)?;
                                }

                                // Move (or rename) the staged bundle
                                fs::rename(&source, &target)
                                    .or_else(|_| unix_fs::symlink(&source, &target))?;

                                // Record the main artifact
                                installed.push(InstalledArtifact::App {
                                    path: target.clone(),
                                });

                                // Create a caskroom symlink for uninstallation
                                let link_path = cask_version_install_path.join(name);
                                if link_path.exists() {
                                    fs::remove_file(&link_path)?;
                                }
                                #[cfg(unix)]
                                std::os::unix::fs::symlink(&target, &link_path)?;

                                installed.push(InstalledArtifact::CaskroomLink {
                                    link_path,
                                    target_path: target,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // Write manifest for these artifacts
    write_cask_manifest(cask, cask_version_install_path, installed.clone())?;
    Ok(installed)
}
