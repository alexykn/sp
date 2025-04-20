// ===== src/build/cask/artifacts/manpage.rs =====

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use log::{info, warn};
use regex::Regex;

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::Result;

/// Install any `manpage` stanzas from the Cask definition.
/// Mirrors Homebrew’s `Cask::Artifact::Manpage < Symlinked` behavior
/// :contentReference[oaicite:3]{index=3}.
pub fn install_manpage(
    cask: &Cask,
    stage_path: &Path,
    _cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    // Look up the "manpage" array in the raw artifacts JSON :contentReference[oaicite:4]{index=4}
    if let Some(artifacts_def) = &cask.artifacts {
        for art in artifacts_def {
            if let Some(obj) = art.as_object() {
                if let Some(entries) = obj.get("manpage").and_then(|v| v.as_array()) {
                    // regex to extract section number or letter from filename
                    // :contentReference[oaicite:5]{index=5}
                    let re = Regex::new(r"\.([1-8nl])(?:\.gz)?$").unwrap();

                    for entry in entries {
                        if let Some(man_file) = entry.as_str() {
                            let src = stage_path.join(man_file);
                            if !src.exists() {
                                warn!("Manpage '{}' not found in staging area, skipping", man_file);
                                continue;
                            }

                            // Determine the section ("1", "5", "n", "l", etc.)
                            let section = if let Some(caps) = re.captures(man_file) {
                                caps.get(1).unwrap().as_str()
                            } else {
                                warn!(
                                    "Filename '{}' does not look like a manpage, skipping",
                                    man_file
                                );
                                continue;
                            };

                            // Build the target directory: e.g. /usr/local/share/man/man1
                            // :contentReference[oaicite:6]{index=6}
                            let man_dir = config.manpagedir().join(format!("man{}", section));
                            fs::create_dir_all(&man_dir)?;

                            // Determine the target path
                            let file_name = Path::new(man_file).file_name().unwrap();
                            let dest = man_dir.join(file_name);

                            // Remove any existing file or symlink
                            // :contentReference[oaicite:7]{index=7}
                            if dest.exists() || dest.symlink_metadata().is_ok() {
                                fs::remove_file(&dest)?;
                            }

                            info!("Linking manpage '{}' → '{}'", src.display(), dest.display());
                            // Create the symlink
                            symlink(&src, &dest)?;

                            // Record it in our manifest
                            installed.push(InstalledArtifact::CaskroomLink {
                                link_path: dest.clone(),
                                target_path: src.clone(),
                            });
                        }
                    }
                    break; // only one "manpage" stanza per Cask
                }
            }
        }
    }

    Ok(installed)
}
