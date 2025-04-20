// ===== sapphire-core/src/build/cask/zip.rs =====
use crate::model::cask::Cask;
use crate::utils::config::Config; // Import Config
use crate::utils::error::{Result, SapphireError};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Install a cask from a ZIP file
// Added Config parameter
pub fn install_from_zip(
    cask: &Cask,
    zip_path: &Path,
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    println!("==> Extracting ZIP file: {}", zip_path.display());

    let temp_dir = TempDir::new()?;
    let extract_dir = temp_dir.path();

    let output = Command::new("unzip")
        .arg("-qq")
        .arg("-o")
        .arg(zip_path)
        .arg("-d")
        .arg(extract_dir)
        .output()?;

    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Failed to extract ZIP file: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    println!("==> ZIP file extracted to: {}", extract_dir.display());

    // Pass Config down
    process_zip_content(cask, extract_dir, cask_version_install_path, config)
}

/// Process the contents of an extracted ZIP file
// Added Config parameter
fn process_zip_content(
    cask: &Cask,
    extract_dir: &Path,
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    // Pass Config down
    if let Ok(()) = super::app::install_app_from_zip(cask, extract_dir, cask_version_install_path, config) {
        return Ok(());
    }
    if let Ok(()) = super::pkg::install_pkg_from_zip(cask, extract_dir, cask_version_install_path, config) {
        return Ok(());
    }

    if let Ok(binary_paths) = find_executable_files(extract_dir) {
        if !binary_paths.is_empty() {
            // Pass Config down
            return install_binary_files(cask, &binary_paths, cask_version_install_path, config);
        }
    }

    Err(SapphireError::Generic(format!(
        "Couldn't find any installable artifacts in ZIP: {}",
        extract_dir.display()
    )))
}

/// Find executable files in a directory
// ... (find_executable_files remains unchanged) ...
fn find_executable_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut executable_paths = Vec::new();
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
            let metadata = fs::metadata(&path)?;
            let permissions = metadata.permissions();
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                if permissions.mode() & 0o111 != 0 { executable_paths.push(path); }
            }
            #[cfg(not(unix))] {
                if let Some(extension) = path.extension() {
                    if extension == "exe" || extension == "bin" { executable_paths.push(path); }
                }
            }
        } else if path.is_dir() {
            let sub_executables = find_executable_files(&path)?;
            executable_paths.extend(sub_executables);
        }
    }
    Ok(executable_paths)
}


/// Install binary files to the appropriate location
// Added Config parameter
fn install_binary_files(
    cask: &Cask,
    binary_paths: &[PathBuf],
    cask_version_install_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    println!("==> Installing binary files");

    let bin_dir = cask_version_install_path.join("bin");
    fs::create_dir_all(&bin_dir)?;

    for binary_path in binary_paths {
        let binary_name = binary_path
            .file_name()
            .ok_or_else(|| SapphireError::Generic("Invalid binary path".to_string()))?;
        let destination = bin_dir.join(binary_name);
        println!(
            "==> Copying binary '{}' to {}",
            binary_name.to_string_lossy(),
            bin_dir.display()
        );
        fs::copy(binary_path, &destination)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&destination)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&destination, permissions)?;
        }
    }

    // Use Config method for bin directory
    let target_bin_dir = config.bin_dir();
    fs::create_dir_all(&target_bin_dir)?;

    let mut created_symlinks: Vec<String> = Vec::new();

    for binary_path in binary_paths {
        let binary_name = binary_path
            .file_name()
            .ok_or_else(|| SapphireError::Generic("Invalid binary path".to_string()))?;
        let source = bin_dir.join(binary_name);
        let link_path = target_bin_dir.join(binary_name);

        if link_path.exists() {
            if link_path
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                fs::remove_file(&link_path)?;
            } else {
                eprintln!("Warning: Existing file at link location {} is not a symlink. Skipping removal.", link_path.display());
                continue;
            }
        }

        println!(
            "==> Linking binary '{}' to {}",
            binary_name.to_string_lossy(),
            target_bin_dir.display()
        );
        if let Err(e) = std::os::unix::fs::symlink(&source, &link_path) {
            eprintln!(
                "Warning: Failed to create symlink {} -> {}: {}",
                link_path.display(),
                source.display(),
                e
            );
            continue;
        }
        created_symlinks.push(link_path.to_string_lossy().to_string());
    }

    let mut artifacts_to_record = created_symlinks;
    for binary_path in binary_paths {
        let binary_name = binary_path.file_name().unwrap();
        let caskroom_bin_path = bin_dir.join(binary_name);
        artifacts_to_record.push(caskroom_bin_path.to_string_lossy().to_string());
    }

    // Use consistent parameter name
    super::write_receipt(cask, cask_version_install_path, artifacts_to_record)?;

    println!("==> Successfully installed binary files");

    Ok(())
}