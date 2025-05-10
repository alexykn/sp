use std::path::Path;
use std::process::Command;

use sps_common::error::{Result, SpsError};
use tracing::{debug, error}; // Added error
use uuid::Uuid;

// Helper to get current timestamp as hex
fn get_timestamp_hex() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default() // Defaults to 0 if time is before UNIX_EPOCH
        .as_secs();
    format!("{secs:x}")
}

// Helper to generate a UUID as hex string
fn get_uuid_hex() -> String {
    Uuid::new_v4().as_hyphenated().to_string().to_uppercase()
}

/// Sets the 'com.apple.quarantine' extended attribute on a file or directory.
/// Uses flags commonly seen for user-initiated downloads (0081).
/// Logs errors assertively, as failure is critical for correct behavior.
pub fn set_quarantine_attribute(path: &Path, agent_name: &str) -> Result<()> {
    if !cfg!(target_os = "macos") {
        debug!(
            "Not on macOS, skipping quarantine attribute for {}",
            path.display()
        );
        return Ok(());
    }

    if !path.exists() {
        error!(
            "Cannot set quarantine attribute, path does not exist: {}",
            path.display()
        );
        return Err(SpsError::NotFound(format!(
            "Path not found for setting quarantine attribute: {}",
            path.display()
        )));
    }

    let timestamp_hex = get_timestamp_hex();
    let uuid_hex = get_uuid_hex();
    // Using "0081" as it's broadly recognized as "downloaded, check pending".
    // Format: "flags;timestamp_hex;agent_name;uuid_hex"
    let quarantine_value = format!("0081;{timestamp_hex};{agent_name};{uuid_hex}");

    debug!(
        "Setting quarantine attribute on {}: value='{}'",
        path.display(),
        quarantine_value
    );

    let output = Command::new("xattr")
        .arg("-w")
        .arg("com.apple.quarantine")
        .arg(&quarantine_value)
        .arg(path.as_os_str())
        .output();

    match output {
        Ok(out) => {
            if out.status.success() {
                debug!(
                    "Successfully set quarantine attribute for {}",
                    path.display()
                );
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                error!( // Changed from warn to error as this is critical for the bug
                    "Failed to set quarantine attribute for {} (status: {}): {}. This may lead to data loss on reinstall or Gatekeeper issues.",
                    path.display(),
                    out.status,
                    stderr.trim()
                );
                // Return an error because failure to set this is likely to cause the reported bug
                Err(SpsError::Generic(format!(
                    "Failed to set com.apple.quarantine on {}: {}",
                    path.display(),
                    stderr.trim()
                )))
            }
        }
        Err(e) => {
            error!(
                "Failed to execute xattr command for {}: {}. Quarantine attribute not set.",
                path.display(),
                e
            );
            Err(SpsError::Io(std::sync::Arc::new(e)))
        }
    }
}
