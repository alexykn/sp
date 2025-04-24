// ===== sapphire-core/src/build/cask/artifacts/dictionary.rs =====

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::Result;

/// Implements the `dictionary` stanza by moving each declared
/// `.dictionary` bundle from the staging area into `~/Library/Dictionaries`,
/// then symlinking it in the Caskroom.
///
/// Homebrew’s Ruby definition is simply:
/// ```ruby
/// class Dictionary < Moved; end
/// ```
/// :contentReference[oaicite:2]{index=2}
pub fn install_dictionary(
    cask: &Cask,
    stage_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    // Find any `dictionary` arrays in the raw JSON artifacts
    if let Some(artifacts_def) = &cask.artifacts {
        for art in artifacts_def {
            if let Some(obj) = art.as_object() {
                if let Some(entries) = obj.get("dictionary").and_then(|v| v.as_array()) {
                    for entry in entries {
                        if let Some(bundle_name) = entry.as_str() {
                            let src = stage_path.join(bundle_name);
                            if !src.exists() {
                                debug!(
                                    "Dictionary bundle '{}' not found in staging; skipping",
                                    bundle_name
                                );
                                continue;
                            }

                            // Standard user dictionary directory: ~/Library/Dictionaries
                            // :contentReference[oaicite:3]{index=3}
                            let dest_dir = config
                                .home_dir() // e.g. /Users/alxknt
                                .join("Library")
                                .join("Dictionaries");
                            fs::create_dir_all(&dest_dir)?;

                            let dest = dest_dir.join(bundle_name);
                            // Remove any previous install
                            if dest.exists() {
                                fs::remove_dir_all(&dest)?;
                            }

                            debug!(
                                "Moving dictionary '{}' → '{}'",
                                src.display(),
                                dest.display()
                            );
                            // Try a direct move; fall back to recursive copy
                            let status = Command::new("mv").arg(&src).arg(&dest).status()?;
                            if !status.success() {
                                Command::new("cp").arg("-R").arg(&src).arg(&dest).status()?;
                            }

                            // Record the moved bundle
                            installed.push(InstalledArtifact::App { path: dest.clone() });

                            // Symlink back into Caskroom for reference
                            let link = cask_version_install_path.join(bundle_name);
                            let _ = fs::remove_file(&link);
                            symlink(&dest, &link)?;
                            installed.push(InstalledArtifact::CaskroomLink {
                                link_path: link,
                                target_path: dest,
                            });
                        }
                    }
                    break; // Only one `dictionary` stanza per cask
                }
            }
        }
    }

    Ok(installed)
}
