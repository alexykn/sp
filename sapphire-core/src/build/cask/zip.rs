// src/build/cask/zip.rs
// Contains logic for extracting ZIP files for cask installation

use crate::utils::error::{BrewRsError, Result};
use crate::model::cask::Cask;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::fs;
use tempfile::TempDir;

/// Install a cask from a ZIP file
pub fn install_from_zip(cask: &Cask, zip_path: &Path, caskroom_path: &Path) -> Result<()> {
    println!("==> Extracting ZIP file: {}", zip_path.display());

    // Create a temporary directory for extraction
    let temp_dir = TempDir::new()?;
    let extract_dir = temp_dir.path();

    // Extract the ZIP file
    let output = Command::new("unzip")
        .arg("-qq")
        .arg("-o")
        .arg(zip_path)
        .arg("-d")
        .arg(extract_dir)
        .output()?;

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "Failed to extract ZIP file: {}", String::from_utf8_lossy(&output.stderr)
        )));
    }

    println!("==> ZIP file extracted to: {}", extract_dir.display());

    // Process the extracted content
    process_zip_content(cask, extract_dir, caskroom_path)
}

/// Process the contents of an extracted ZIP file
fn process_zip_content(cask: &Cask, extract_dir: &Path, caskroom_path: &Path) -> Result<()> {
    // Try to install app first
    if let Ok(()) = super::app::install_app_from_zip(cask, extract_dir, caskroom_path) {
        return Ok(());
    }

    // If no app, try to install pkg
    if let Ok(()) = super::pkg::install_pkg_from_zip(cask, extract_dir, caskroom_path) {
        return Ok(());
    }

    // If neither app nor pkg, try to find any executable or binary files
    if let Ok(binary_paths) = find_executable_files(extract_dir) {
        if !binary_paths.is_empty() {
            return install_binary_files(cask, &binary_paths, caskroom_path);
        }
    }

    // If we couldn't find anything to install, return an error
    Err(BrewRsError::Generic(format!(
        "Couldn't find any installable artifacts in ZIP: {}", extract_dir.display()
    )))
}

/// Find executable files in a directory
fn find_executable_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut executable_paths = Vec::new();

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
            // Check if file is executable
            let metadata = fs::metadata(&path)?;
            let permissions = metadata.permissions();

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if permissions.mode() & 0o111 != 0 {
                    executable_paths.push(path);
                }
            }

            #[cfg(not(unix))]
            {
                // On non-unix systems, check file extension
                if let Some(extension) = path.extension() {
                    if extension == "exe" || extension == "bin" {
                        executable_paths.push(path);
                    }
                }
            }
        } else if path.is_dir() {
            // Recursively search subdirectories
            let sub_executables = find_executable_files(&path)?;
            executable_paths.extend(sub_executables);
        }
    }

    Ok(executable_paths)
}

/// Install binary files to the appropriate location
fn install_binary_files(cask: &Cask, binary_paths: &[PathBuf], caskroom_path: &Path) -> Result<()> {
    println!("==> Installing binary files");

    // Create a bin directory in the caskroom path
    let bin_dir = caskroom_path.join("bin");
    fs::create_dir_all(&bin_dir)?;

    // Copy each binary to the bin directory
    for binary_path in binary_paths {
        let binary_name = binary_path.file_name()
            .ok_or_else(|| BrewRsError::Generic("Invalid binary path".to_string()))?;

        let destination = bin_dir.join(binary_name);

        println!("==> Copying binary '{}' to {}", binary_name.to_string_lossy(), bin_dir.display());
        fs::copy(binary_path, &destination)?;

        // Set execute permission
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&destination)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&destination, permissions)?;
        }
    }

    // Create symlinks in /usr/local/bin if needed
    let homebrew_bin = PathBuf::from("/usr/local/bin");
    if homebrew_bin.exists() {
        for binary_path in binary_paths {
            let binary_name = binary_path.file_name()
                .ok_or_else(|| BrewRsError::Generic("Invalid binary path".to_string()))?;

            let source = bin_dir.join(binary_name);
            let link_path = homebrew_bin.join(binary_name);

            // Remove existing symlink if it exists
            if link_path.exists() {
                fs::remove_file(&link_path)?;
            }

            // Create the symlink
            println!("==> Linking binary '{}' to {}", binary_name.to_string_lossy(), homebrew_bin.display());
            std::os::unix::fs::symlink(&source, &link_path)?;
        }
    }

    // Create receipt file with installation info
    let artifacts = binary_paths.iter().map(|p| {
        format!("binary:{}", p.file_name().unwrap().to_string_lossy())
    }).collect();

    super::write_receipt(cask, caskroom_path, artifacts)?;

    println!("==> Successfully installed binary files");

    Ok(())
}
