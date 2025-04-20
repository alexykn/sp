// ===== sapphire-core/src/build/cask/app.rs =====
use crate::model::cask::Cask;
use crate::utils::config::Config; // Import Config
use crate::utils::error::{Result, SapphireError};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Install an app from a mounted DMG
// Added Config parameter
pub fn install_app_from_dmg(
    cask: &Cask,
    mount_point: &Path,
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    let app_path = find_app_in_directory(mount_point, get_app_name(cask))?;
    // Pass Config down
    install_app(&app_path, cask, cask_version_install_path, config)
}

/// Install an app from an extracted ZIP
// Added Config parameter
pub fn install_app_from_zip(
    cask: &Cask,
    extract_dir: &Path,
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    let app_path = find_app_in_directory(extract_dir, get_app_name(cask))?;
    // Pass Config down
    install_app(&app_path, cask, cask_version_install_path, config)
}

/// Find and install any app from a directory (for casks without specific app name)
// Added Config parameter
pub fn find_and_install_app(
    cask: &Cask,
    source_dir: &Path,
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    let app_paths = find_all_apps_in_directory(source_dir)?;
    if app_paths.is_empty() {
        return Err(SapphireError::Generic(format!(
            "No .app bundles found in {}",
            source_dir.display()
        )));
    }
    // Pass Config down
    install_app(&app_paths[0], cask, cask_version_install_path, config)
}


// ... (get_app_name, find_app_in_directory, find_all_apps_in_directory remain unchanged) ...
fn get_app_name(cask: &Cask) -> String {
    if let Some(ref name) = cask.name {
        if !name.is_empty() {
            let app_name = if name.len() > 0 { &name[0] } else { "" };
            if !app_name.ends_with(".app") { return format!("{}.app", app_name); }
            return app_name.to_string();
        }
    }
    format!("{}.app", cask.token)
}
fn find_app_in_directory(dir: &Path, app_name: String) -> Result<PathBuf> {
    let exact_path = dir.join(&app_name);
    if exact_path.exists() && exact_path.is_dir() { return Ok(exact_path); }
    let app_paths = find_all_apps_in_directory(dir)?;
    if app_paths.is_empty() {
        return Err(SapphireError::Generic(format!("App bundle '{}' not found in {}", app_name, dir.display())));
    }
    Ok(app_paths[0].clone())
}
fn find_all_apps_in_directory(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut app_paths = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => { return Err(SapphireError::Generic(format!("Failed to read directory {}: {}", dir.display(), e))) }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => { return Err(SapphireError::Generic(format!("Failed to read directory entry: {}", e))) }
        };
        let path = entry.path();
        if path.is_dir() {
            if let Some(extension) = path.extension() {
                if extension == "app" { app_paths.push(path.clone()); }
            }
            let sub_apps = find_all_apps_in_directory(&path)?;
            app_paths.extend(sub_apps);
        }
    }
    Ok(app_paths)
}


/// Actually install an app bundle to /Applications and create symlink in cask version path
// Added Config parameter
fn install_app(
    app_path: &Path,
    cask: &Cask, // Keep cask for receipt writing
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    use serde_json;

    let app_name = app_path
        .file_name()
        .ok_or_else(|| SapphireError::Generic("Invalid app path".to_string()))?
        .to_string_lossy();

    // Use Config method for Applications directory
    let applications_dir = config.applications_dir();
    let destination = applications_dir.join(&*app_name);

    println!(
        "==> Moving app '{}' to {}",
        app_name,
        applications_dir.display()
    );

    if destination.exists() {
        println!("==> Removing existing app at {}", destination.display());
        match fs::remove_dir_all(&destination) {
            Ok(_) => {}
            Err(_) => {
                println!("==> Failed to remove app directly, trying with sudo...");
                let output = Command::new("sudo")
                    .arg("rm")
                    .arg("-rf")
                    .arg(&destination)
                    .output()?;
                if !output.status.success() {
                    return Err(SapphireError::Generic(format!(
                        "Failed to remove existing app: {}",
                        String::from_utf8_lossy(&output.stderr)
                    )));
                }
            }
        }
    }

    let output = Command::new("cp")
        .arg("-R")
        .arg(app_path)
        .arg(&destination)
        .output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Failed to copy app: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    // Use consistent parameter name
    let caskroom_app_link_path = cask_version_install_path.join(&*app_name);

    if caskroom_app_link_path.exists() {
        fs::remove_file(&caskroom_app_link_path)?;
    }
    std::os::unix::fs::symlink(&destination, &caskroom_app_link_path)?;

    let _output = Command::new("chmod")
        .arg("-R")
        .arg("755")
        .arg(&destination)
        .output()?;

    // Write install manifest for uninstall
    let manifest = vec![
        destination.to_string_lossy().to_string(),
        caskroom_app_link_path.to_string_lossy().to_string(), // Record the link path too
    ];
    // Use consistent parameter name
    let manifest_path = cask_version_install_path.join("INSTALL_MANIFEST.json"); // Use consistent name
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| SapphireError::Generic(format!("Failed to serialize manifest: {}", e)))?;
    fs::write(&manifest_path, manifest_json)?;
    println!("Wrote cask install manifest: {}", manifest_path.display());

    // Write receipt (optional but good practice)
    super::write_receipt(cask, cask_version_install_path, manifest)?; // Pass manifest items as artifacts

    println!("==> Successfully installed {}", app_name);

    Ok(())
}