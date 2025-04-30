// sps-aio/src/process.rs (NEW FILE)
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Output as StdOutput;
use std::process::Stdio; // Keep for sync version
use std::sync::Arc;

use sps_common::error::{Result, SpsError};
use tokio::process::Command; // Use tokio's Command
use tracing::{debug, error};

/// Asynchronously runs an external command and captures its output.
pub async fn run_command_async(
    command: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    envs: Option<HashMap<String, String>>,
) -> Result<StdOutput> {
    debug!(
        "Async Running command: {} {:?} (cwd: {:?}, envs: {:?})",
        command,
        args,
        cwd,
        envs.as_ref().map(|e| e.keys().collect::<Vec<_>>()) // Log only keys for envs
    );

    let mut cmd = Command::new(command);
    cmd.args(args);
    cmd.kill_on_drop(true); // Ensure process is killed if the command handle is dropped

    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if let Some(env_map) = envs {
        cmd.envs(env_map);
    }

    // Capture stdout/stderr. Consider making this configurable if needed.
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::null()); // Prevent hanging on stdin

    match cmd.output().await {
        Ok(output) => {
            // Log output only if command failed for debugging
            if !output.status.success() {
                debug!("Async Command failed with status: {}", output.status);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stdout.trim().is_empty() {
                    debug!("Stdout:\n{}", stdout.trim());
                }
                if !stderr.trim().is_empty() {
                    debug!("Stderr:\n{}", stderr.trim());
                }
            } else {
                debug!("Async Command finished successfully.");
            }
            Ok(output) // Return the full output regardless of status
        }
        Err(e) => {
            error!("Async Failed to execute command: {}", e);
            Err(SpsError::Io(Arc::new(e)))
        }
    }
}

// --- Sync Version (Kept for reference) ---
pub fn run_command_sync(
    command: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    envs: Option<HashMap<String, String>>,
) -> Result<StdOutput> {
    debug!(
        "Sync Running command: {} {:?} (cwd: {:?}, envs: {:?})",
        command,
        args,
        cwd,
        envs.as_ref().map(|e| e.keys().collect::<Vec<_>>())
    );
    let mut cmd = std::process::Command::new(command);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if let Some(env_map) = envs {
        cmd.envs(env_map);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::null());

    match cmd.output() {
        Ok(output) => {
            if !output.status.success() {
                debug!("Sync Command failed with status: {}", output.status);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stdout.trim().is_empty() {
                    debug!("Stdout:\n{}", stdout.trim());
                }
                if !stderr.trim().is_empty() {
                    debug!("Stderr:\n{}", stderr.trim());
                }
            } else {
                debug!("Sync Command finished successfully.");
            }
            Ok(output)
        }
        Err(e) => {
            error!("Sync Failed to execute command: {}", e);
            Err(SpsError::Io(Arc::new(e)))
        }
    }
}
