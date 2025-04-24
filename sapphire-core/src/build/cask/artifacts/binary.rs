// ===== sapphire-core/src/build/cask/artifacts/binary.rs =====

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};

/// Installs `binary` artifacts, which can be declared as:
///  - a simple string: `"foo"` (source and target both `"foo"`)
///  - a map: `{ "source": "path/in/stage", "target": "name", "chmod": "0755" }`
///  - a map with just `"target"`: automatically generate a wrapper script
///
/// Copies or symlinks executables into the prefix bin directory,
/// and records both the link and caskroom reference.
pub fn install_binary(
    cask: &Cask,
    stage_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    if let Some(artifacts_def) = &cask.artifacts {
        for art in artifacts_def {
            if let Some(obj) = art.as_object() {
                if let Some(entries) = obj.get("binary") {
                    // Normalize into an array
                    let arr = if let Some(arr) = entries.as_array() {
                        arr.clone()
                    } else {
                        vec![entries.clone()]
                    };

                    let bin_dir = config.bin_dir();
                    fs::create_dir_all(&bin_dir)?;

                    for entry in arr {
                        // Determine source, target, and optional chmod
                        let (source_rel, target_name, chmod) = if let Some(tgt) = entry.as_str() {
                            // simple form: "foo"
                            (tgt.to_string(), tgt.to_string(), None)
                        } else if let Some(m) = entry.as_object() {
                            let target = m
                                .get("target")
                                .and_then(|v| v.as_str())
                                .map(String::from)
                                .ok_or_else(|| {
                                    SapphireError::InstallError(format!(
                                        "Binary artifact missing 'target': {m:?}"
                                    ))
                                })?;

                            let chmod = m.get("chmod").and_then(|v| v.as_str()).map(String::from);

                            // If `source` is provided, use it; otherwise generate wrapper
                            let source = if let Some(src) = m.get("source").and_then(|v| v.as_str())
                            {
                                src.to_string()
                            } else {
                                // generate wrapper script in caskroom
                                let wrapper_name = format!("{target}.wrapper.sh");
                                let wrapper_path = cask_version_install_path.join(&wrapper_name);

                                // assume the real executable lives inside the .app bundle
                                let app_name = format!("{}.app", cask.display_name());
                                let exe_path =
                                    format!("/Applications/{app_name}/Contents/MacOS/{target}");

                                let script =
                                    format!("#!/usr/bin/env bash\nexec \"{exe_path}\" \"$@\"\n");
                                fs::write(&wrapper_path, script)?;
                                Command::new("chmod")
                                    .arg("+x")
                                    .arg(&wrapper_path)
                                    .status()?;

                                wrapper_name
                            };

                            (source, target, chmod)
                        } else {
                            debug!("Invalid binary artifact entry: {:?}", entry);
                            continue;
                        };

                        let src_path = stage_path.join(&source_rel);
                        if !src_path.exists() {
                            debug!("Binary source '{}' not found, skipping", src_path.display());
                            continue;
                        }

                        // Link into bin_dir
                        let link_path = bin_dir.join(&target_name);
                        let _ = fs::remove_file(&link_path);
                        debug!(
                            "Linking binary '{}' → '{}'",
                            src_path.display(),
                            link_path.display()
                        );
                        symlink(&src_path, &link_path)?;

                        // Apply chmod if specified
                        if let Some(mode) = chmod.as_deref() {
                            let _ = Command::new("chmod").arg(mode).arg(&link_path).status();
                        }

                        installed.push(InstalledArtifact::BinaryLink {
                            link_path: link_path.clone(),
                            target_path: src_path.clone(),
                        });

                        // Also create a Caskroom symlink for reference
                        let caskroom_link = cask_version_install_path.join(&target_name);
                        let _ = fs::remove_file(&caskroom_link);
                        symlink(&link_path, &caskroom_link)?;
                        installed.push(InstalledArtifact::CaskroomLink {
                            link_path: caskroom_link,
                            target_path: link_path,
                        });
                    }

                    // Only one binary stanza per cask
                    break;
                }
            }
        }
    }

    Ok(installed)
}
