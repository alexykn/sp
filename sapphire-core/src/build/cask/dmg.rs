// In sapphire-core/src/build/cask/dmg.rs

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{debug, error};

use crate::utils::error::{Result, SapphireError}; // Added log imports

// --- Keep Existing Helpers ---
pub fn mount_dmg(dmg_path: &Path) -> Result<PathBuf> {
    debug!("Mounting DMG: {}", dmg_path.display());
    let output = Command::new("hdiutil")
        .arg("attach")
        .arg("-plist")
        .arg("-nobrowse")
        .arg("-readonly")
        .arg("-mountrandom")
        .arg("/tmp") // Consider making mount location configurable or more robust
        .arg(dmg_path)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!(
            "hdiutil attach failed for {}: {}",
            dmg_path.display(),
            stderr
        );
        return Err(SapphireError::Generic(format!(
            "Failed to mount DMG '{}': {}",
            dmg_path.display(),
            stderr
        )));
    }

    let mount_point = parse_mount_point(&output.stdout)?;
    debug!("DMG mounted at: {}", mount_point.display());
    Ok(mount_point)
}

pub fn unmount_dmg(mount_point: &Path) -> Result<()> {
    debug!("Unmounting DMG from: {}", mount_point.display());
    // Add logging for commands
    debug!("Executing: hdiutil detach -force {}", mount_point.display());
    let output = Command::new("hdiutil")
        .arg("detach")
        .arg("-force")
        .arg(mount_point)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!(
            "hdiutil detach failed ({}): {}. Trying diskutil...",
            output.status, stderr
        );
        // Add logging for fallback
        debug!(
            "Executing: diskutil unmount force {}",
            mount_point.display()
        );
        let diskutil_output = Command::new("diskutil")
            .arg("unmount")
            .arg("force")
            .arg(mount_point)
            .output()?;

        if !diskutil_output.status.success() {
            let diskutil_stderr = String::from_utf8_lossy(&diskutil_output.stderr);
            error!(
                "diskutil unmount force failed ({}): {}",
                diskutil_output.status, diskutil_stderr
            );
            // Consider returning error only if both fail? Or always error on diskutil fail?
            return Err(SapphireError::Generic(format!(
                "Failed to unmount DMG '{}' using hdiutil and diskutil: {}",
                mount_point.display(),
                diskutil_stderr
            )));
        }
    }
    debug!("DMG successfully unmounted");
    Ok(())
}

fn parse_mount_point(output: &[u8]) -> Result<PathBuf> {
    // ... (existing implementation) ...
    // Use plist crate for more robust parsing if possible in the future
    let cursor = std::io::Cursor::new(output);
    let reader = BufReader::new(cursor);
    let mut in_sys_entities = false;
    let mut in_mount_point = false;
    let mut mount_path_str: Option<String> = None;

    for line_res in reader.lines() {
        let line = line_res?;
        let trimmed = line.trim();

        if trimmed == "<key>system-entities</key>" {
            in_sys_entities = true;
            continue;
        }
        if !in_sys_entities {
            continue;
        }

        if trimmed == "<key>mount-point</key>" {
            in_mount_point = true;
            continue;
        }

        if in_mount_point && trimmed.starts_with("<string>") && trimmed.ends_with("</string>") {
            mount_path_str = Some(
                trimmed
                    .trim_start_matches("<string>")
                    .trim_end_matches("</string>")
                    .to_string(),
            );
            break; // Found the first mount point, assume it's the main one
        }

        // Reset flags if we encounter closing tags for structures containing mount-point
        if trimmed == "</dict>" {
            in_mount_point = false;
        }
        if trimmed == "</array>" && in_sys_entities {
            // End of system-entities
            // break; // Stop searching if we leave the system-entities array
            in_sys_entities = false; // Reset this flag too
        }
    }

    match mount_path_str {
        Some(path_str) if !path_str.is_empty() => {
            debug!("Parsed mount point from plist: {}", path_str);
            Ok(PathBuf::from(path_str))
        }
        _ => {
            error!("Failed to parse mount point from hdiutil plist output.");
            // Optionally log the raw output for debugging
            // error!("Raw hdiutil output:\n{}", String::from_utf8_lossy(output));
            Err(SapphireError::Generic(
                "Failed to determine mount point from hdiutil output".to_string(),
            ))
        }
    }
}

// --- NEW Function ---
/// Extracts the contents of a mounted DMG to a staging directory using `ditto`.
pub fn extract_dmg_to_stage(dmg_path: &Path, stage_dir: &Path) -> Result<()> {
    let mount_point = mount_dmg(dmg_path)?;

    // Ensure the stage directory exists (though TempDir should handle it)
    if !stage_dir.exists() {
        fs::create_dir_all(stage_dir).map_err(SapphireError::Io)?;
    }

    debug!(
        "Copying contents from DMG mount {} to stage {} using ditto...",
        mount_point.display(),
        stage_dir.display()
    );
    // Use ditto for robust copying, preserving metadata
    // ditto <source> <destination>
    debug!(
        "Executing: ditto {} {}",
        mount_point.display(),
        stage_dir.display()
    );
    let ditto_output = Command::new("ditto")
        .arg(&mount_point) // Source first
        .arg(stage_dir) // Then destination
        .output()?;

    let unmount_result = unmount_dmg(&mount_point); // Unmount regardless of ditto success

    if !ditto_output.status.success() {
        let stderr = String::from_utf8_lossy(&ditto_output.stderr);
        error!("ditto command failed ({}): {}", ditto_output.status, stderr);
        // Also log stdout which might contain info on specific file errors
        let stdout = String::from_utf8_lossy(&ditto_output.stdout);
        if !stdout.trim().is_empty() {
            error!("ditto stdout: {}", stdout);
        }
        unmount_result?; // Ensure we still return unmount error if it happened
        return Err(SapphireError::Generic(format!(
            "Failed to copy DMG contents using ditto: {stderr}"
        )));
    }

    unmount_result // Return the result of unmounting
}
