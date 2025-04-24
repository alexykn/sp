// ===== sapphire-core/src/build/cask/artifacts/installer.rs =====

use std::path::Path;
use std::process::{Command, Stdio};

use tracing::debug;

use crate::build::cask::InstalledArtifact;
use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};

/// Implements the `installer` stanza:
/// - `manual`: prints instructions to open the staged path.
/// - `script`: runs the given executable with args, under sudo if requested.
///
/// Mirrors Homebrew’s `Cask::Artifact::Installer` behavior :contentReference[oaicite:1]{index=1}.
pub fn run_installer(
    cask: &Cask,
    stage_path: &Path,
    _cask_version_install_path: &Path,
    _config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    // Find the `installer` definitions in the raw JSON artifacts
    if let Some(artifacts_def) = &cask.artifacts {
        for art in artifacts_def {
            if let Some(obj) = art.as_object() {
                if let Some(insts) = obj.get("installer").and_then(|v| v.as_array()) {
                    for inst in insts {
                        if let Some(inst_obj) = inst.as_object() {
                            // Manual installer: user must open the path themselves
                            if let Some(man) = inst_obj.get("manual").and_then(|v| v.as_str()) {
                                debug!(
                                    "Cask {} requires manual install. To finish:\n    open {}",
                                    cask.token,
                                    stage_path.join(man).display()
                                );
                                // Nothing to record in InstalledArtifact for manual
                                continue;
                            }

                            // Script installer
                            let exe_key = if inst_obj.contains_key("script") {
                                "script"
                            } else {
                                "executable"
                            };
                            let executable = inst_obj
                                .get(exe_key)
                                .and_then(|v| v.as_str())
                                .ok_or_else(|| {
                                    SapphireError::Generic(format!(
                                        "installer stanza missing '{exe_key}' field"
                                    ))
                                })?;
                            let args: Vec<String> = inst_obj
                                .get("args")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|a| a.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let use_sudo = inst_obj
                                .get("sudo")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);

                            let script_path = stage_path.join(executable);
                            if !script_path.exists() {
                                return Err(SapphireError::NotFound(format!(
                                    "Installer script not found: {}",
                                    script_path.display()
                                )));
                            }

                            debug!(
                                "Running installer script '{}' for cask {}",
                                script_path.display(),
                                cask.token
                            );
                            // Build the command
                            let mut cmd = if use_sudo {
                                let mut c = Command::new("sudo");
                                c.arg(script_path.clone());
                                c
                            } else {
                                Command::new(script_path.clone())
                            };
                            cmd.args(&args);
                            // Inherit stdout/stderr so user sees progress
                            cmd.stdin(Stdio::null())
                                .stdout(Stdio::inherit())
                                .stderr(Stdio::inherit());

                            // Execute
                            let status = cmd.status().map_err(|e| {
                                SapphireError::Generic(format!(
                                    "Failed to spawn installer script: {e}"
                                ))
                            })?;
                            if !status.success() {
                                return Err(SapphireError::InstallError(format!(
                                    "Installer script exited with {status}"
                                )));
                            }

                            // No specific files to record here, but we can note the script ran
                            installed
                                .push(InstalledArtifact::CaskroomReference { path: script_path });
                        }
                    }
                }
            }
        }
    }

    Ok(installed)
}
