use std::path::Path;
use std::process::Command;

use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_common::model::cask::Cask;
use tracing::debug;

use crate::build::cask::InstalledArtifact;

/// Execute any `preflight` commands listed in the Cask’s JSON artifact stanza.
/// Returns an empty Vec since preflight does not produce install artifacts.
pub fn run_preflight(
    cask: &Cask,
    stage_path: &Path,
    _config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    // Iterate over artifacts, look for "preflight" keys
    if let Some(entries) = &cask.artifacts {
        for entry in entries.iter().filter_map(|v| v.as_object()) {
            if let Some(cmds) = entry.get("preflight").and_then(|v| v.as_array()) {
                for cmd_val in cmds.iter().filter_map(|v| v.as_str()) {
                    // Substitute $STAGEDIR placeholder
                    let cmd_str = cmd_val.replace("$STAGEDIR", stage_path.to_str().unwrap());
                    debug!("Running preflight: {}", cmd_str);
                    let status = Command::new("sh").arg("-c").arg(&cmd_str).status()?;
                    if !status.success() {
                        return Err(SpsError::InstallError(format!(
                            "preflight failed: {cmd_str}"
                        )));
                    }
                }
            }
        }
    }

    // No install artifacts to return
    Ok(Vec::new())
}
