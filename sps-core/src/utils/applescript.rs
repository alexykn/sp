use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use plist::Value as PlistValue;
use sps_common::error::Result;
use tracing::{debug, warn};

fn get_bundle_identifier_from_app_path(app_path: &Path) -> Option<String> {
    let info_plist_path = app_path.join("Contents/Info.plist");
    if !info_plist_path.is_file() {
        debug!("Info.plist not found at {}", info_plist_path.display());
        return None;
    }
    match PlistValue::from_file(&info_plist_path) {
        Ok(PlistValue::Dictionary(dict)) => dict
            .get("CFBundleIdentifier")
            .and_then(PlistValue::as_string)
            .map(String::from),
        Ok(val) => {
            warn!(
                "Info.plist at {} is not a dictionary. Value: {:?}",
                info_plist_path.display(),
                val
            );
            None
        }
        Err(e) => {
            warn!(
                "Failed to parse Info.plist at {}: {}",
                info_plist_path.display(),
                e
            );
            None
        }
    }
}

fn is_app_running_by_bundle_id(bundle_id: &str) -> Result<bool> {
    let script = format!(
        "tell application \"System Events\" to (exists (process 1 where bundle identifier is \"{bundle_id}\"))"
    );
    debug!(
        "Checking if app with bundle ID '{}' is running using script: {}",
        bundle_id, script
    );

    let output = Command::new("osascript").arg("-e").arg(&script).output()?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_lowercase();
        debug!(
            "is_app_running_by_bundle_id ('{}') stdout: '{}'",
            bundle_id, stdout
        );
        Ok(stdout == "true")
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "osascript check running status for bundle ID '{}' failed. Status: {}, Stderr: {}",
            bundle_id,
            output.status,
            stderr.trim()
        );
        Ok(false)
    }
}

/// Attempts to gracefully quit an application using its bundle identifier (preferred) or name via
/// AppleScript. Retries several times, checking if the app is still running between attempts.
/// Returns Ok even if the app could not be quit, as uninstall should proceed.
pub fn quit_app_gracefully(app_path: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        debug!("Not on macOS, skipping app quit for {}", app_path.display());
        return Ok(());
    }
    if !app_path.exists() {
        debug!(
            "App path {} does not exist, skipping quit attempt.",
            app_path.display()
        );
        return Ok(());
    }

    let app_name_for_log = app_path
        .file_name()
        .map_or_else(|| app_path.to_string_lossy(), |name| name.to_string_lossy())
        .trim_end_matches(".app")
        .to_string();

    let bundle_identifier = get_bundle_identifier_from_app_path(app_path);

    let (script_target, using_bundle_id) = match &bundle_identifier {
        Some(id) => (id.clone(), true),
        None => {
            warn!(
                "Could not get bundle identifier for {}. Will attempt to quit by name '{}'. This is less reliable.",
                app_path.display(),
                app_name_for_log
            );
            (app_name_for_log.clone(), false)
        }
    };

    debug!(
        "Attempting to quit app '{}' (script target: '{}', using bundle_id: {})",
        app_name_for_log, script_target, using_bundle_id
    );

    // Initial check if app is running (only reliable if we have bundle ID)
    if using_bundle_id {
        match is_app_running_by_bundle_id(&script_target) {
            Ok(true) => debug!(
                "App '{}' is running. Proceeding with quit attempts.",
                script_target
            ),
            Ok(false) => {
                debug!("App '{}' is not running. Quit unnecessary.", script_target);
                return Ok(());
            }
            Err(e) => {
                warn!(
                    "Could not determine if app '{}' is running (check failed: {}). Proceeding with quit attempt.",
                    script_target, e
                );
            }
        }
    }

    let quit_command = if using_bundle_id {
        format!("tell application id \"{script_target}\" to quit")
    } else {
        format!("tell application \"{script_target}\" to quit")
    };

    const MAX_QUIT_ATTEMPTS: usize = 4;
    const QUIT_DELAYS_SECS: [u64; MAX_QUIT_ATTEMPTS - 1] = [2, 3, 5];

    // Use enumerate over QUIT_DELAYS_SECS for Clippy compliance
    for (attempt, delay) in QUIT_DELAYS_SECS.iter().enumerate() {
        debug!(
            "Quit attempt #{} for '{}' using command: {}",
            attempt + 1,
            script_target,
            quit_command
        );

        let output = Command::new("osascript")
            .arg("-e")
            .arg(&quit_command)
            .output()?; // Propagate IO errors from osascript execution

        if output.status.success() {
            debug!(
                "osascript quit command sent successfully for '{}' (attempt #{}).",
                script_target,
                attempt + 1
            );
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("-1712")
                || stderr.contains("-1728")
                || stderr.contains("-600")
                || stderr.to_lowercase().contains("application isn’t running")
                || stderr.to_lowercase().contains("application not running")
            {
                debug!(
                    "osascript: App '{}' reported as not running or not found (stderr: {}). Quit successful or unnecessary.",
                    script_target,
                    stderr.trim()
                );
                return Ok(());
            } else {
                warn!(
                    "osascript quit command for '{}' failed (attempt #{}). Status: {}. Stderr: {}",
                    script_target,
                    attempt + 1,
                    output.status,
                    stderr.trim()
                );
            }
        }

        // Wait briefly to allow the app to process the quit command
        thread::sleep(Duration::from_secs(*delay));

        // Check if the app is still running (if using bundle ID)
        if using_bundle_id {
            match is_app_running_by_bundle_id(&script_target) {
                Ok(true) => {
                    debug!(
                        "App '{}' still running after attempt #{}. Retrying.",
                        script_target,
                        attempt + 1
                    );
                }
                Ok(false) => {
                    debug!(
                        "App '{}' successfully quit after attempt #{}.",
                        script_target,
                        attempt + 1
                    );
                    return Ok(());
                }
                Err(e) => {
                    warn!(
                        "Could not verify if app '{}' quit after attempt #{}: {}. Assuming it might still be running.",
                        script_target,
                        attempt + 1,
                        e
                    );
                }
            }
        }
    }

    // Final attempt (the fourth, not covered by QUIT_DELAYS_SECS)
    let attempt = QUIT_DELAYS_SECS.len();
    debug!(
        "Quit attempt #{} for '{}' using command: {}",
        attempt + 1,
        script_target,
        quit_command
    );

    let output = Command::new("osascript")
        .arg("-e")
        .arg(&quit_command)
        .output()?; // Propagate IO errors from osascript execution

    if output.status.success() {
        debug!(
            "osascript quit command sent successfully for '{}' (attempt #{}).",
            script_target,
            attempt + 1
        );
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("-1712")
            || stderr.contains("-1728")
            || stderr.contains("-600")
            || stderr.to_lowercase().contains("application isn’t running")
            || stderr.to_lowercase().contains("application not running")
        {
            debug!(
                "osascript: App '{}' reported as not running or not found (stderr: {}). Quit successful or unnecessary.",
                script_target,
                stderr.trim()
            );
            return Ok(());
        } else {
            warn!(
                "osascript quit command for '{}' failed (attempt #{}). Status: {}. Stderr: {}",
                script_target,
                attempt + 1,
                output.status,
                stderr.trim()
            );
        }
    }

    // Final check if the app is still running (if using bundle ID)
    if using_bundle_id {
        match is_app_running_by_bundle_id(&script_target) {
            Ok(true) => {
                warn!(
                    "App '{}' still running after {} quit attempts.",
                    script_target, MAX_QUIT_ATTEMPTS
                );
            }
            Ok(false) => {
                debug!(
                    "App '{}' successfully quit after attempt #{}.",
                    script_target,
                    attempt + 1
                );
                return Ok(());
            }
            Err(e) => {
                warn!(
                    "Could not verify if app '{}' quit after attempt #{}: {}. Assuming it might still be running.",
                    script_target,
                    attempt + 1,
                    e
                );
            }
        }
    } else {
        warn!(
            "App '{}' (targeted by name) might still be running after {} quit attempts. Manual check may be needed.",
            script_target,
            MAX_QUIT_ATTEMPTS
        );
    }
    Ok(())
}
