// ===== sapphire-core/src/build/cask/artifacts/internet_plugin.rs =====

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::Result;

/// Implements the `internet_plugin` stanza by moving each declared
/// internet plugin bundle from the staging area into
/// `~/Library/Internet Plug-Ins`, then symlinking it in the Caskroom.
///
/// Mirrors Homebrew’s `InternetPlugin < Moved` pattern.
pub fn install_internet_plugin(
    cask: &Cask,
    stage_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    // Look for "internet_plugin" entries in the JSON artifacts
    if let Some(artifacts_def) = &cask.artifacts {
        for art in artifacts_def {
            if let Some(obj) = art.as_object() {
                if let Some(entries) = obj.get("internet_plugin").and_then(|v| v.as_array()) {
                    // Target directory for user internet plugins
                    let dest_dir = config.home_dir().join("Library").join("Internet Plug-Ins");
                    fs::create_dir_all(&dest_dir)?;

                    for entry in entries {
                        if let Some(name) = entry.as_str() {
                            let src = stage_path.join(name);
                            if !src.exists() {
                                debug!("Internet plugin '{}' not found in staging; skipping", name);
                                continue;
                            }

                            let dest = dest_dir.join(name);
                            if dest.exists() {
                                fs::remove_dir_all(&dest)?;
                            }

                            debug!(
                                "Installing internet plugin '{}' → '{}'",
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
                            let link = cask_version_install_path.join(name);
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
