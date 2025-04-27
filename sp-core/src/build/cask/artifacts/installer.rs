// ===== sp-core/src/build/cask/artifacts/installer.rs =====

use std::path::Path;
use std::process::{Command, Stdio};

use sp_common::config::Config;
use sp_common::error::{Result, SpError};
use sp_common::model::cask::Cask;
use tracing::debug;

use crate::build::cask::InstalledArtifact;

// Helper to validate that the executable is a filename (relative, no '/' or "..")
fn validate_filename_or_relative_path(file: &str) -> Result<String> {
    if file.starts_with("/") || file.contains("..") || file.contains("/") {
        return Err(SpError::Generic(format!(
            "Invalid executable filename: {file}"
        )));
    }
    Ok(file.to_string())
}

// Helper to validate a command argument based on allowed characters or allowed option form
fn validate_argument(arg: &str) -> Result<String> {
    if arg.starts_with("-") {
        return Ok(arg.to_string());
    }
    if arg.starts_with("/") || arg.contains("..") || arg.contains("/") {
        return Err(SpError::Generic(format!("Invalid argument: {arg}")));
    }
    if !arg
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(SpError::Generic(format!(
            "Invalid characters in argument: {arg}"
        )));
    }
    Ok(arg.to_string())
}

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
                            if let Some(man) = inst_obj.get("manual").and_then(|v| v.as_str()) {
                                debug!(
                                    "Cask {} requires manual install. To finish:\n    open {}",
                                    cask.token,
                                    stage_path.join(man).display()
                                );
                                continue;
                            }
                            let exe_key = if inst_obj.contains_key("script") {
                                "script"
                            } else {
                                "executable"
                            };
                            let executable = inst_obj
                                .get(exe_key)
                                .and_then(|v| v.as_str())
                                .ok_or_else(|| {
                                    SpError::Generic(format!(
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

                            let validated_executable =
                                validate_filename_or_relative_path(executable)?;
                            let mut validated_args = Vec::new();
                            for arg in &args {
                                validated_args.push(validate_argument(arg)?);
                            }

                            let script_path = stage_path.join(&validated_executable);
                            if !script_path.exists() {
                                return Err(SpError::NotFound(format!(
                                    "Installer script not found: {}",
                                    script_path.display()
                                )));
                            }

                            debug!(
                                "Running installer script '{}' for cask {}",
                                script_path.display(),
                                cask.token
                            );
                            let mut cmd = if use_sudo {
                                let mut c = Command::new("sudo");
                                c.arg(script_path.clone());
                                c
                            } else {
                                Command::new(script_path.clone())
                            };
                            cmd.args(&validated_args);
                            cmd.stdin(Stdio::null())
                                .stdout(Stdio::inherit())
                                .stderr(Stdio::inherit());

                            let status = cmd.status().map_err(|e| {
                                SpError::Generic(format!("Failed to spawn installer script: {e}"))
                            })?;
                            if !status.success() {
                                return Err(SpError::InstallError(format!(
                                    "Installer script exited with {status}"
                                )));
                            }

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
