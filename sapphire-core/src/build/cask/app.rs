// src/build/cask/app.rs
// Contains logic for installing .app bundles from casks

use crate::utils::error::{BrewRsError, Result};
use crate::model::cask::Cask;
use std::path::{Path, PathBuf};
use std::fs;
use std::process::Command;

/// Install an app from a mounted DMG
pub fn install_app_from_dmg(cask: &Cask, mount_point: &Path, caskroom_path: &Path) -> Result<()> {
    // Find the .app bundle in the DMG
    let app_path = find_app_in_directory(mount_point, get_app_name(cask))?;

    // Install the app
    install_app(&app_path, cask, caskroom_path)
}

/// Install an app from an extracted ZIP
pub fn install_app_from_zip(cask: &Cask, extract_dir: &Path, caskroom_path: &Path) -> Result<()> {
    // Find the .app bundle in the extracted directory
    let app_path = find_app_in_directory(extract_dir, get_app_name(cask))?;

    // Install the app
    install_app(&app_path, cask, caskroom_path)
}

/// Find and install any app from a directory (for casks without specific app name)
pub fn find_and_install_app(cask: &Cask, source_dir: &Path, caskroom_path: &Path) -> Result<()> {
    // Just find any .app bundle in the directory
    let app_paths = find_all_apps_in_directory(source_dir)?;

    if app_paths.is_empty() {
        return Err(BrewRsError::Generic(format!(
            "No .app bundles found in {}", source_dir.display()
        )));
    }

    // Install the first app found
    install_app(&app_paths[0], cask, caskroom_path)
}

/// Get the expected app name for a cask
fn get_app_name(cask: &Cask) -> String {
    // Try to get app name from cask first
    if let Some(ref name) = cask.name {
        if !name.is_empty() {
            // Use the first name in the array if it's an array
            let app_name = if name.len() > 0 {
                &name[0]
            } else {
                ""
            };

            // Add .app extension if not present
            if !app_name.ends_with(".app") {
                return format!("{}.app", app_name);
            }
            return app_name.to_string();
        }
    }

    // Fall back to token-based name if no specific name provided
    format!("{}.app", cask.token)
}

/// Find an app bundle in a directory
fn find_app_in_directory(dir: &Path, app_name: String) -> Result<PathBuf> {
    // First try the exact app name
    let exact_path = dir.join(&app_name);
    if exact_path.exists() && exact_path.is_dir() {
        return Ok(exact_path);
    }

    // If exact match not found, try to find any .app bundle
    let app_paths = find_all_apps_in_directory(dir)?;

    if app_paths.is_empty() {
        return Err(BrewRsError::Generic(format!(
            "App bundle '{}' not found in {}", app_name, dir.display()
        )));
    }

    // Return the first app found
    Ok(app_paths[0].clone())
}

/// Find all .app bundles in a directory
fn find_all_apps_in_directory(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut app_paths = Vec::new();

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return Err(BrewRsError::Generic(format!(
            "Failed to read directory {}: {}", dir.display(), e
        ))),
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_e) => return Err(BrewRsError::Generic(format!(
                "Failed to read directory entry: {}", _e
            ))),
        };

        let path = entry.path();

        if path.is_dir() {
            if let Some(extension) = path.extension() {
                if extension == "app" {
                    // Clone the path before pushing
                    app_paths.push(path.clone());
                }
            }

            // Recursively search subdirectories
            let sub_apps = find_all_apps_in_directory(&path)?;
            app_paths.extend(sub_apps);
        }
    }

    Ok(app_paths)
}

/// Actually install an app bundle to /Applications and create symlink in caskroom
fn install_app(app_path: &Path, _cask: &Cask, caskroom_path: &Path) -> Result<()> {
    // Get the application name
    let app_name = app_path.file_name()
        .ok_or_else(|| BrewRsError::Generic("Invalid app path".to_string()))?
        .to_string_lossy();

    // Destination in /Applications
    let applications_dir = super::get_applications_dir();
    let destination = applications_dir.join(&*app_name);

    println!("==> Moving app '{}' to {}", app_name, applications_dir.display());

    // Remove existing app if it exists
    if destination.exists() {
        println!("==> Removing existing app at {}", destination.display());
        match fs::remove_dir_all(&destination) {
            Ok(_) => {},
            Err(_e) => {
                // If we can't remove it directly, try with sudo
                println!("==> Failed to remove app directly, trying with sudo...");
                let output = Command::new("sudo")
                    .arg("rm")
                    .arg("-rf")
                    .arg(&destination)
                    .output()?;

                if !output.status.success() {
                    return Err(BrewRsError::Generic(format!(
                        "Failed to remove existing app: {}", String::from_utf8_lossy(&output.stderr)
                    )));
                }
            }
        }
    }

    // Copy app to Applications directory
    // We use cp -R because fs::copy doesn't handle directories
    let output = Command::new("cp")
        .arg("-R")
        .arg(app_path)
        .arg(&destination)
        .output()?;

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "Failed to copy app: {}", String::from_utf8_lossy(&output.stderr)
        )));
    }

    // Store app path in caskroom
    let caskroom_app_path = caskroom_path.join(&*app_name);

    // Create a symlink in caskroom to the app in /Applications
    if caskroom_app_path.exists() {
        fs::remove_file(&caskroom_app_path)?;
    }

    std::os::unix::fs::symlink(&destination, &caskroom_app_path)?;

    // Set proper permissions
    let _output = Command::new("chmod")
        .arg("-R")
        .arg("755")
        .arg(&destination)
        .output()?;

    println!("==> Successfully installed {}", app_name);

    Ok(())
}
