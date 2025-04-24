// ===== sapphire-core/src/build/cask/artifacts/keyboard_layout.rs =====

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::Result;

/// Installs `keyboard_layout` bundles from the staging area into
/// `~/Library/Keyboard Layouts`, then symlinks them into the Caskroom.
///
/// Mirrors Homebrew’s `KeyboardLayout < Moved` behavior.
pub fn install_keyboard_layout(
    cask: &Cask,
    stage_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    if let Some(artifacts_def) = &cask.artifacts {
        for art in artifacts_def {
            if let Some(obj) = art.as_object() {
                if let Some(entries) = obj.get("keyboard_layout").and_then(|v| v.as_array()) {
                    // Target directory for user keyboard layouts
                    let dest_dir = config.home_dir().join("Library").join("Keyboard Layouts");
                    fs::create_dir_all(&dest_dir)?;

                    for entry in entries {
                        if let Some(bundle_name) = entry.as_str() {
                            let src = stage_path.join(bundle_name);
                            if !src.exists() {
                                debug!(
                                    "Keyboard layout '{}' not found in staging; skipping",
                                    bundle_name
                                );
                                continue;
                            }

                            let dest = dest_dir.join(bundle_name);
                            if dest.exists() {
                                fs::remove_dir_all(&dest)?;
                            }

                            debug!(
                                "Installing keyboard layout '{}' → '{}'",
                                src.display(),
                                dest.display()
                            );
                            // Try move, fallback to copy
                            let status = Command::new("mv").arg(&src).arg(&dest).status()?;
                            if !status.success() {
                                Command::new("cp").arg("-R").arg(&src).arg(&dest).status()?;
                            }

                            // Record moved bundle
                            installed.push(InstalledArtifact::App { path: dest.clone() });

                            // Symlink into Caskroom
                            let link = cask_version_install_path.join(bundle_name);
                            let _ = fs::remove_file(&link);
                            symlink(&dest, &link)?;
                            installed.push(InstalledArtifact::CaskroomLink {
                                link_path: link,
                                target_path: dest,
                            });
                        }
                    }
                    break; // one stanza only
                }
            }
        }
    }

    Ok(installed)
}
