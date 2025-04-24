// src/build/cask/artifacts/suite.rs

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::Result;

/// Implements the `suite` stanza by moving each named directory from
/// the staging area into `/Applications`, then symlinking it in the Caskroom.
///
/// Mirrors Homebrew’s Suite < Moved behavior (dirmethod :appdir)
/// :contentReference[oaicite:3]{index=3}
pub fn install_suite(
    cask: &Cask,
    stage_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    // Find the `suite` definition in the raw JSON artifacts
    if let Some(artifacts_def) = &cask.artifacts {
        for art in artifacts_def.iter() {
            if let Some(obj) = art.as_object() {
                if let Some(entries) = obj.get("suite").and_then(|v| v.as_array()) {
                    for entry in entries {
                        if let Some(dir_name) = entry.as_str() {
                            let src = stage_path.join(dir_name);
                            if !src.exists() {
                                debug!(
                                    "Suite directory '{}' not found in staging, skipping",
                                    dir_name
                                );
                                continue;
                            }

                            let dest_dir = config.applications_dir(); // e.g. /Applications
                            let dest = dest_dir.join(dir_name); // e.g. /Applications/Foobar Suite
                            if dest.exists() {
                                fs::remove_dir_all(&dest)?; // remove old
                            }

                            debug!("Moving suite '{}' → '{}'", src.display(), dest.display());
                            // Try a rename (mv); fall back to recursive copy if cross‑filesystem
                            let mv_status = Command::new("mv").arg(&src).arg(&dest).status()?;
                            if !mv_status.success() {
                                Command::new("cp").arg("-R").arg(&src).arg(&dest).status()?;
                            }

                            // Record as an App artifact (a directory moved into /Applications)
                            installed.push(InstalledArtifact::App { path: dest.clone() });

                            // Then symlink it under Caskroom for reference
                            let link = cask_version_install_path.join(dir_name);
                            let _ = fs::remove_file(&link);
                            symlink(&dest, &link)?;
                            installed.push(InstalledArtifact::CaskroomLink {
                                link_path: link,
                                target_path: dest,
                            });
                        }
                    }
                    break; // only one "suite" stanza per cask
                }
            }
        }
    }

    Ok(installed)
}
