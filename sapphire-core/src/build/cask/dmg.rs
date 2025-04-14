// src/build/cask/dmg.rs
// Contains logic for mounting DMG files for cask installation

use crate::model::cask::Cask;
use crate::utils::error::{Result, SapphireError};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Mount a DMG file and return the mount point
pub fn mount_dmg(dmg_path: &Path) -> Result<PathBuf> {
    println!("==> Mounting DMG: {}", dmg_path.display());

    // Run hdiutil to mount the DMG
    let output = Command::new("hdiutil")
        .arg("attach")
        .arg("-plist")
        .arg("-nobrowse")
        .arg("-readonly")
        .arg("-mountrandom")
        .arg("/tmp")
        .arg(dmg_path)
        .output()?;

    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Failed to mount DMG: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    // Parse the output to find the mount point
    let mount_point = parse_mount_point(&output.stdout)?;

    println!("==> DMG mounted at: {}", mount_point.display());

    Ok(mount_point)
}

/// Unmount a DMG
pub fn unmount_dmg(mount_point: &Path) -> Result<()> {
    println!("==> Unmounting DMG from: {}", mount_point.display());

    // Run hdiutil to unmount the DMG
    let output = Command::new("hdiutil")
        .arg("detach")
        .arg("-force")
        .arg(mount_point)
        .output()?;

    if !output.status.success() {
        // Try with diskutil if hdiutil fails
        println!("==> hdiutil detach failed, trying diskutil...");
        let diskutil_output = Command::new("diskutil")
            .arg("unmount")
            .arg("force")
            .arg(mount_point)
            .output()?;

        if !diskutil_output.status.success() {
            return Err(SapphireError::Generic(format!(
                "Failed to unmount DMG: {}",
                String::from_utf8_lossy(&diskutil_output.stderr)
            )));
        }
    }

    println!("==> DMG successfully unmounted");

    Ok(())
}

/// Install a cask from a DMG file
pub fn install_from_dmg(cask: &Cask, dmg_path: &Path, caskroom_path: &Path) -> Result<()> {
    // Mount the DMG
    let mount_point = mount_dmg(dmg_path)?;

    // Make sure we unmount the DMG when we're done
    let result = process_dmg(cask, &mount_point, caskroom_path);

    // Always try to unmount, even if there was an error
    let unmount_result = unmount_dmg(&mount_point);

    // Return the original error if there was one
    result?;

    // Otherwise return the unmount result
    unmount_result
}

/// Process the contents of a mounted DMG
fn process_dmg(cask: &Cask, mount_point: &Path, caskroom_path: &Path) -> Result<()> {
    // Try to install app first
    if let Ok(()) = super::app::install_app_from_dmg(cask, mount_point, caskroom_path) {
        return Ok(());
    }

    // If no app, try to install pkg
    if let Ok(()) = super::pkg::install_pkg_from_dmg(cask, mount_point, caskroom_path) {
        return Ok(());
    }

    // If we couldn't find anything to install, return an error
    Err(SapphireError::Generic(format!(
        "Couldn't find any installable artifacts in DMG: {}",
        mount_point.display()
    )))
}

/// Parse the mount point from the hdiutil output
fn parse_mount_point(output: &[u8]) -> Result<PathBuf> {
    // For simplicity, we'll just look for lines containing /Volumes/ or /private/tmp/
    // in the output and assume the last one is our mount point
    let reader = BufReader::new(output);

    let mut mount_point = None;

    for line in reader.lines() {
        let line = line?;

        if line.contains("/Volumes/") || line.contains("/private/tmp/") {
            // Extract the path portion
            if let Some(path_start) = line.find('/') {
                let mut path = line[path_start..].trim().to_string();

                // Remove any trailing characters
                if let Some(end_idx) = path.find(|c| c == '"' || c == '<' || c == ' ') {
                    path = path[..end_idx].to_string();
                }

                // Store this as our mount point
                if !path.is_empty() {
                    mount_point = Some(PathBuf::from(path));
                }
            }
        }
    }

    mount_point.ok_or_else(|| {
        SapphireError::Generic("Failed to determine mount point from hdiutil output".to_string())
    })
}
