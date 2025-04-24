// ===== sapphire-core/src/build/cask/artifacts/colorpicker.rs =====

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::Result;

/// Installs any `colorpicker` stanzas from the Cask definition.
///
/// Homebrew’s `Colorpicker` artifact simply subclasses `Moved` with
/// `dirmethod :colorpickerdir` → `~/Library/ColorPickers` :contentReference[oaicite:3]{index=3}.
pub fn install_colorpicker(
    cask: &Cask,
    stage_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    if let Some(artifacts_def) = &cask.artifacts {
        for art in artifacts_def {
            if let Some(obj) = art.as_object() {
                if let Some(entries) = obj.get("colorpicker").and_then(|v| v.as_array()) {
                    // For each declared bundle name:
                    for entry in entries {
                        if let Some(bundle_name) = entry.as_str() {
                            let src = stage_path.join(bundle_name);
                            if !src.exists() {
                                debug!(
                                    "Colorpicker bundle '{}' not found in stage; skipping",
                                    bundle_name
                                );
                                continue;
                            }

                            // Ensure ~/Library/ColorPickers exists
                            // :contentReference[oaicite:4]{index=4}
                            let dest_dir = config
                                .home_dir() // e.g. /Users/alxknt
                                .join("Library")
                                .join("ColorPickers");
                            fs::create_dir_all(&dest_dir)?;

                            let dest = dest_dir.join(bundle_name);
                            // Remove any previous copy
                            if dest.exists() {
                                fs::remove_dir_all(&dest)?;
                            }

                            debug!(
                                "Moving colorpicker '{}' → '{}'",
                                src.display(),
                                dest.display()
                            );
                            // mv, fallback to cp -R if necessary (cross‑device)
                            let status = Command::new("mv").arg(&src).arg(&dest).status()?;
                            if !status.success() {
                                Command::new("cp").arg("-R").arg(&src).arg(&dest).status()?;
                            }

                            // Record as a moved artifact (bundle installed)
                            installed.push(InstalledArtifact::App { path: dest.clone() });

                            // Symlink back into Caskroom for reference
                            // :contentReference[oaicite:5]{index=5}
                            let link = cask_version_install_path.join(bundle_name);
                            let _ = fs::remove_file(&link);
                            symlink(&dest, &link)?;
                            installed.push(InstalledArtifact::CaskroomLink {
                                link_path: link,
                                target_path: dest,
                            });
                        }
                    }
                    break; // only one `colorpicker` stanza per cask
                }
            }
        }
    }

    Ok(installed)
}
