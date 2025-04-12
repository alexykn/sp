// src/build/cask/pkg.rs
// Contains logic for installing .pkg packages from casks

use crate::utils::error::{BrewRsError, Result};
use crate::model::cask::Cask;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::fs;

/// Install a pkg from a mounted DMG
pub fn install_pkg_from_dmg(cask: &Cask, mount_point: &Path, caskroom_path: &Path) -> Result<()> {
    // Find the .pkg file in the DMG
    let pkg_path = find_pkg_in_directory(mount_point)?;

    // Install the pkg
    install_pkg(&pkg_path, cask, caskroom_path)
}

/// Install a pkg from an extracted ZIP
pub fn install_pkg_from_zip(cask: &Cask, extract_dir: &Path, caskroom_path: &Path) -> Result<()> {
    // Find the .pkg file in the extracted directory
    let pkg_path = find_pkg_in_directory(extract_dir)?;

    // Install the pkg
    install_pkg(&pkg_path, cask, caskroom_path)
}

/// Install a pkg directly from a file path
pub fn install_pkg_from_path(cask: &Cask, pkg_path: &Path, caskroom_path: &Path) -> Result<()> {
    install_pkg(pkg_path, cask, caskroom_path)
}

/// Find a pkg file in a directory
fn find_pkg_in_directory(dir: &Path) -> Result<PathBuf> {
    let mut pkg_paths = Vec::new();

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return Err(BrewRsError::Generic(format!(
            "Failed to read directory {}: {}", dir.display(), e
        ))),
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => return Err(BrewRsError::Generic(format!(
                "Failed to read directory entry: {}", e
            ))),
        };

        let path = entry.path();

        if path.is_file() {
            if let Some(extension) = path.extension() {
                if extension == "pkg" || extension == "mpkg" {
                    pkg_paths.push(path);
                }
            }
        } else if path.is_dir() {
            // Recursively search subdirectories
            if let Ok(sub_pkg) = find_pkg_in_directory(&path) {
                pkg_paths.push(sub_pkg);
            }
        }
    }

    if pkg_paths.is_empty() {
        return Err(BrewRsError::Generic(format!(
            "No .pkg files found in {}", dir.display()
        )));
    }

    // Return the first pkg found
    Ok(pkg_paths[0].clone())
}

/// Install a pkg file using the installer tool
fn install_pkg(pkg_path: &Path, cask: &Cask, caskroom_path: &Path) -> Result<()> {
    println!("==> Installing pkg file: {}", pkg_path.display());

    // Create a copy of the pkg in the caskroom for reference
    let pkg_name = pkg_path.file_name()
        .ok_or_else(|| BrewRsError::Generic("Invalid pkg path".to_string()))?;

    let caskroom_pkg_path = caskroom_path.join(pkg_name);

    // Copy the pkg to caskroom for reference
    println!("==> Copying pkg to caskroom for reference");
    fs::copy(pkg_path, &caskroom_pkg_path)?;

    // Use installer to install the pkg
    println!("==> Running installer (this may require sudo)");
    let output = Command::new("sudo")
        .arg("installer")
        .arg("-pkg")
        .arg(pkg_path)
        .arg("-target")
        .arg("/")
        .output()?;

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "Package installation failed: {}", String::from_utf8_lossy(&output.stderr)
        )));
    }

    // Create receipt file with installation info
    super::write_receipt(cask, caskroom_path, vec![format!("pkg:{}", pkg_name.to_string_lossy())])?;

    println!("==> Successfully installed pkg");

    Ok(())
}

/// Check if a cask has pkg artifacts
pub fn has_pkg_artifact(cask: &Cask) -> bool {
    // A cask has pkg artifacts if it has any URLs ending with .pkg
    // or if it explicitly defines pkg artifacts in its definition
    if let Some(ref urls) = cask.url {
        for url in urls {
            if url.contains(".pkg") {
                return true;
            }
        }
    }

    // For simplicity, assume any cask that doesn't have .app artifacts
    // and is downloaded as a .pkg file has pkg artifacts
    if let Some(ref urls) = cask.url {
        for url in urls {
            if url.ends_with(".pkg") {
                return true;
            }
        }
    }

    false
}
