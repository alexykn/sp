// ===== sapphire-core/src/build/cask/artifacts/vst_plugin.rs =====

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::Result;

/// Installs `vst_plugin` bundles from the staging area into
/// `~/Library/Audio/Plug-Ins/VST`, then symlinks them into the Caskroom.
///
/// Mirrors Homebrew’s `VstPlugin < Moved` pattern.
pub fn install_vst_plugin(
    cask: &Cask,
    stage_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    if let Some(artifacts_def) = &cask.artifacts {
        for art in artifacts_def {
            if let Some(obj) = art.as_object() {
                if let Some(entries) = obj.get("vst_plugin").and_then(|v| v.as_array()) {
                    // Target directory for VST plugins
                    let dest_dir = config
                        .home_dir()
                        .join("Library")
                        .join("Audio")
                        .join("Plug-Ins")
                        .join("VST");
                    fs::create_dir_all(&dest_dir)?;

                    for entry in entries {
                        if let Some(bundle_name) = entry.as_str() {
                            let src = stage_path.join(bundle_name);
                            if !src.exists() {
                                debug!(
                                    "VST plugin '{}' not found in staging; skipping",
                                    bundle_name
                                );
                                continue;
                            }

                            let dest = dest_dir.join(bundle_name);
                            if dest.exists() {
                                fs::remove_dir_all(&dest)?;
                            }

                            debug!(
                                "Installing VST plugin '{}' → '{}'",
                                src.display(),
                                dest.display()
                            );
                            // Try move, fallback to copy
                            let status = Command::new("mv").arg(&src).arg(&dest).status()?;
                            if !status.success() {
                                Command::new("cp").arg("-R").arg(&src).arg(&dest).status()?;
                            }

                            // Record moved plugin
                            installed.push(InstalledArtifact::App { path: dest.clone() });

                            // Symlink into Caskroom for reference
                            let link = cask_version_install_path.join(bundle_name);
                            let _ = fs::remove_file(&link);
                            symlink(&dest, &link)?;
                            installed.push(InstalledArtifact::CaskroomLink {
                                link_path: link,
                                target_path: dest,
                            });
                        }
                    }
                    break; // single stanza
                }
            }
        }
    }

    Ok(installed)
}
