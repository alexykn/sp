// ===== sapphire-core/src/build/cask/dmg.rs =====
use crate::model::cask::Cask;
use crate::utils::config::Config; // Import Config
use crate::utils::error::{Result, SapphireError};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

// ... (mount_dmg, unmount_dmg, parse_mount_point remain unchanged) ...
pub fn mount_dmg(dmg_path: &Path) -> Result<PathBuf> {
    println!("==> Mounting DMG: {}", dmg_path.display());
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
        return Err(SapphireError::Generic(format!("Failed to mount DMG: {}", String::from_utf8_lossy(&output.stderr))));
    }
    let mount_point = parse_mount_point(&output.stdout)?;
    println!("==> DMG mounted at: {}", mount_point.display());
    Ok(mount_point)
}
pub fn unmount_dmg(mount_point: &Path) -> Result<()> {
    println!("==> Unmounting DMG from: {}", mount_point.display());
    let output = Command::new("hdiutil").arg("detach").arg("-force").arg(mount_point).output()?;
    if !output.status.success() {
        println!("==> hdiutil detach failed, trying diskutil...");
        let diskutil_output = Command::new("diskutil").arg("unmount").arg("force").arg(mount_point).output()?;
        if !diskutil_output.status.success() {
            return Err(SapphireError::Generic(format!("Failed to unmount DMG: {}", String::from_utf8_lossy(&diskutil_output.stderr))));
        }
    }
    println!("==> DMG successfully unmounted");
    Ok(())
}
fn parse_mount_point(output: &[u8]) -> Result<PathBuf> {
    let reader = BufReader::new(output);
    let mut mount_point = None;
    for line in reader.lines() {
        let line = line?;
        if line.contains("/Volumes/") || line.contains("/private/tmp/") {
            if let Some(path_start) = line.find('/') {
                let mut path = line[path_start..].trim().to_string();
                if let Some(end_idx) = path.find(|c| c == '"' || c == '<' || c == ' ') {
                    path = path[..end_idx].to_string();
                }
                if !path.is_empty() { mount_point = Some(PathBuf::from(path)); }
            }
        }
    }
    mount_point.ok_or_else(|| SapphireError::Generic("Failed to determine mount point from hdiutil output".to_string()))
}


/// Install a cask from a DMG file
// Added Config parameter
pub fn install_from_dmg(
    cask: &Cask,
    dmg_path: &Path,
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    let mount_point = mount_dmg(dmg_path)?;
    // Pass Config down
    let result = process_dmg(cask, &mount_point, cask_version_install_path, config);
    let unmount_result = unmount_dmg(&mount_point);
    result?;
    unmount_result
}

/// Process the contents of a mounted DMG
// Added Config parameter
fn process_dmg(
    cask: &Cask,
    mount_point: &Path,
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    // Pass Config down
    if let Ok(()) = super::app::install_app_from_dmg(cask, mount_point, cask_version_install_path, config) {
        return Ok(());
    }
    if let Ok(()) = super::pkg::install_pkg_from_dmg(cask, mount_point, cask_version_install_path, config) {
        return Ok(());
    }
    Err(SapphireError::Generic(format!(
        "Couldn't find any installable artifacts in DMG: {}",
        mount_point.display()
    )))
}