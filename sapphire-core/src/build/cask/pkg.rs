// ===== sapphire-core/src/build/cask/pkg.rs =====
use crate::model::cask::Cask;
use crate::utils::config::Config; // Import Config
use crate::utils::error::{Result, SapphireError};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Install a pkg from a mounted DMG
// Added Config parameter
pub fn install_pkg_from_dmg(
    cask: &Cask,
    mount_point: &Path,
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    let pkg_path = find_pkg_in_directory(mount_point)?;
    // Pass Config down
    install_pkg(&pkg_path, cask, cask_version_install_path, config)
}

/// Install a pkg from an extracted ZIP
// Added Config parameter
pub fn install_pkg_from_zip(
    cask: &Cask,
    extract_dir: &Path,
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    let pkg_path = find_pkg_in_directory(extract_dir)?;
    // Pass Config down
    install_pkg(&pkg_path, cask, cask_version_install_path, config)
}

/// Install a pkg directly from a file path
// Added Config parameter
pub fn install_pkg_from_path(
    cask: &Cask,
    pkg_path: &Path,
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    // Pass Config down
    install_pkg(pkg_path, cask, cask_version_install_path, config)
}


// ... (find_pkg_in_directory remains unchanged) ...
fn find_pkg_in_directory(dir: &Path) -> Result<PathBuf> {
    let mut pkg_paths = Vec::new();
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
        if path.is_file() {
            if let Some(extension) = path.extension() {
                if extension == "pkg" || extension == "mpkg" { pkg_paths.push(path); }
            }
        } else if path.is_dir() {
            if let Ok(sub_pkg) = find_pkg_in_directory(&path) { pkg_paths.push(sub_pkg); }
        }
    }
    if pkg_paths.is_empty() {
        return Err(SapphireError::Generic(format!("No .pkg files found in {}", dir.display())));
    }
    Ok(pkg_paths[0].clone())
}

/// Install a pkg file using the installer tool
// Added Config parameter (though not directly used in this func, passed for consistency)
fn install_pkg(
    pkg_path: &Path,
    cask: &Cask,
    cask_version_install_path: &Path,
    _config: &Config, // Added config (unused for now)
) -> Result<()> {
    println!("==> Installing pkg file: {}", pkg_path.display());

    let pkg_name = pkg_path
        .file_name()
        .ok_or_else(|| SapphireError::Generic("Invalid pkg path".to_string()))?;
    // Use consistent parameter name
    let caskroom_pkg_path = cask_version_install_path.join(pkg_name);

    println!("==> Copying pkg to caskroom for reference");
    fs::copy(pkg_path, &caskroom_pkg_path)?;

    println!("==> Running installer (this may require sudo)");
    let output = Command::new("sudo")
        .arg("installer")
        .arg("-pkg")
        .arg(pkg_path)
        .arg("-target")
        .arg("/")
        .output()?;
    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Package installation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let mut artifacts_to_record = vec![
        caskroom_pkg_path.to_string_lossy().to_string(),
    ];

    if let Some(uninstall_stanzas) = &cask.uninstall {
        if let Some(pkgutil_id_value) = uninstall_stanzas.get("pkgutil") {
            if let Some(pkgutil_id) = pkgutil_id_value.as_str() {
                artifacts_to_record.push(format!("pkgutil:{}", pkgutil_id));
                println!("Found pkgutil ID for manifest: {}", pkgutil_id);
            }
        }
        // Add handling for other uninstall types (launchctl, script, etc.) if needed
    }

    // Use consistent parameter name
    super::write_receipt(cask, cask_version_install_path, artifacts_to_record)?;

    println!("==> Successfully installed pkg");
    Ok(())
}