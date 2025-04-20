// ===== sapphire-core/src/build/cask/zip.rs =====
// Corrected E0502

use crate::model::cask::Cask;
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use log::{debug, warn};
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

/// Install a cask from a ZIP file
pub fn install_from_zip(
    cask: &Cask,
    zip_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<()> {
    println!("==> Extracting ZIP file: {}", zip_path.display());

    let temp_dir = TempDir::new().map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to create temp directory for ZIP extraction: {}", e),
        ))
    })?;
    let extract_dir = temp_dir.path();
    debug!("Extracting ZIP to temp dir: {}", extract_dir.display());

    match crate::build::extract::extract_archive(zip_path, extract_dir, 0) {
        Ok(_) => {
            println!("==> ZIP file extracted to: {}", extract_dir.display());
        }
        Err(e) => {
            return Err(SapphireError::InstallError(format!(
                "Failed to extract ZIP file '{}': {}",
                zip_path.display(),
                e
            )));
        }
    }

    process_zip_content(cask, extract_dir, cask_version_install_path, config)
}

/// Process the contents of an extracted ZIP file
fn process_zip_content(
    cask: &Cask,
    extract_dir: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<()> {
    // Try installing an .app found in the extracted directory
    if let Ok(()) = super::app::install_app_from_zip(cask, extract_dir, cask_version_install_path, config) {
        debug!("Installed .app artifact found in ZIP content.");
        return Ok(());
    }
     // Try installing a .pkg found in the extracted directory
    if let Ok(()) = super::pkg::install_pkg_from_zip(cask, extract_dir, cask_version_install_path, config) {
        debug!("Installed .pkg artifact found in ZIP content.");
        return Ok(());
    }

    // Fallback: Check for executable files if no .app or .pkg was handled
    match find_executable_files(extract_dir) {
         Ok(binary_paths) if !binary_paths.is_empty() => {
             debug!("Found {} executable file(s) in ZIP content, installing as binaries.", binary_paths.len());
             install_binary_files(cask, &binary_paths, cask_version_install_path, config)
         }
         Ok(_) => { // No binaries found either
            Err(SapphireError::InstallError(format!(
                "Couldn't find any installable artifacts (.app, .pkg, or binaries) in extracted ZIP content at {}",
                extract_dir.display()
            )))
         }
         Err(e) => { // Error during binary search
             Err(SapphireError::InstallError(format!(
                 "Failed to search for binaries in extracted ZIP content at {}: {}",
                 extract_dir.display(), e
            )))
         }
    }
}

/// Find executable files in a directory
fn find_executable_files(dir: &Path) -> Result<Vec<PathBuf>> {
    // (Implementation remains the same as provided previously)
     let mut executable_paths = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => { return Err(SapphireError::Generic(format!("Failed to read directory {}: {}", dir.display(), e))) }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                warn!("Failed to read directory entry in {}: {}", dir.display(), e);
                continue;
            }
        };
        let path = entry.path();
        if path.is_file() {
            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    warn!("Failed to get metadata for {}: {}", path.display(), e);
                    continue;
                }
            };
            let permissions = metadata.permissions();
            #[cfg(unix)] {
                if permissions.mode() & 0o111 != 0 {
                    debug!("Found executable file: {}", path.display());
                    executable_paths.push(path);
                }
            }
            #[cfg(not(unix))] {
                if let Some(extension) = path.extension().and_then(|s| s.to_str()) {
                    match extension.to_lowercase().as_str() {
                        "exe" | "bat" | "cmd" | "com" | "bin" | "sh" => {
                             debug!("Found potential executable file (by extension): {}", path.display());
                             executable_paths.push(path);
                        }
                        _ => {}
                    }
                }
            }
        } else if path.is_dir() {
            match find_executable_files(&path) {
                 Ok(sub_executables) => executable_paths.extend(sub_executables),
                 Err(e) => warn!("Failed to search subdirectory {}: {}", path.display(), e),
             }
        }
    }
    Ok(executable_paths)
}


/// Install binary files to the appropriate location
fn install_binary_files(
    cask: &Cask,
    binary_paths: &[PathBuf],
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<()> {
    println!("==> Installing binary files for cask: {}", cask.token);

    let caskroom_bin_dir = cask_version_install_path.join("bin");
    fs::create_dir_all(&caskroom_bin_dir)?;

    let mut created_artifacts = vec![caskroom_bin_dir.to_string_lossy().to_string()];

    for temp_binary_path in binary_paths {
        let binary_name = temp_binary_path
            .file_name()
            .ok_or_else(|| SapphireError::Generic(format!("Invalid temporary binary path: {}", temp_binary_path.display())))?;
        let caskroom_dest = caskroom_bin_dir.join(binary_name);

        debug!(
            "Copying binary '{}' from temp dir to {}",
            binary_name.to_string_lossy(),
            caskroom_bin_dir.display()
        );

        fs::copy(temp_binary_path, &caskroom_dest).map_err(|e| SapphireError::Io(e))?;
        created_artifacts.push(caskroom_dest.to_string_lossy().to_string());

        #[cfg(unix)]
        {
            match fs::metadata(&caskroom_dest) {
                 Ok(metadata) => {
                     let mut permissions = metadata.permissions();
                     let current_mode = permissions.mode();
                     let new_mode = current_mode | 0o111;
                     if new_mode != current_mode {
                         permissions.set_mode(new_mode);
                         if let Err(e) = fs::set_permissions(&caskroom_dest, permissions) {
                             warn!("Failed to set executable permissions on {}: {}", caskroom_dest.display(), e);
                         } else {
                              debug!("Set executable permissions on {}", caskroom_dest.display());
                         }
                     }
                 }
                 Err(e) => {
                     warn!("Failed to get metadata for copied binary {}: {}", caskroom_dest.display(), e);
                 }
             }
        }
         #[cfg(not(unix))]
         {
            debug!("Skipping permission setting on non-Unix for {}", caskroom_dest.display());
         }
    }

    let target_bin_dir = config.bin_dir();
    fs::create_dir_all(&target_bin_dir)?;

    // *** FIX for E0502: Collect links to create *after* iterating ***
    let mut links_to_create = Vec::new();
    let mut links_created_paths = Vec::new(); // To record for the manifest

    for caskroom_bin_path_str in &created_artifacts { // Iterate immutably
         let caskroom_bin_path = PathBuf::from(caskroom_bin_path_str);
         if caskroom_bin_path.is_dir() { continue; } // Skip the directory itself

         if let Some(binary_name) = caskroom_bin_path.file_name() {
            let link_path = target_bin_dir.join(binary_name);

            if link_path.exists() || link_path.symlink_metadata().is_ok() {
                 debug!("Removing existing item at link location: {}", link_path.display());
                 if let Err(rm_err) = fs::remove_file(&link_path) {
                     if rm_err.kind() == std::io::ErrorKind::IsADirectory || rm_err.kind() == std::io::ErrorKind::PermissionDenied {
                         if let Err(rm_dir_err) = fs::remove_dir_all(&link_path) {
                             warn!("Failed to remove existing directory at link location {}: {}", link_path.display(), rm_dir_err);
                             continue;
                          }
                     } else if rm_err.kind() != std::io::ErrorKind::NotFound {
                         warn!("Failed to remove existing item at link location {}: {}", link_path.display(), rm_err);
                         continue;
                     }
                 }
             }

             // Store details needed to create the link after the loop
             links_to_create.push((caskroom_bin_path.clone(), link_path.clone()));
        }
    }

    // Now create the links
    for (source_path, link_path) in links_to_create {
        println!(
            "==> Linking binary '{}' to {}",
            link_path.file_name().map(|s|s.to_string_lossy()).unwrap_or_default(),
            target_bin_dir.display()
        );
        #[cfg(unix)]
        {
            if let Err(e) = std::os::unix::fs::symlink(&source_path, &link_path) {
                warn!(
                   "Warning: Failed to create symlink {} -> {}: {}",
                   link_path.display(),
                   source_path.display(),
                   e
               );
               continue; // Skip recording if link failed
            }
            links_created_paths.push(link_path.to_string_lossy().to_string()); // Record the symlink path
         }
         #[cfg(not(unix))]
         {
             warn!("Symlink creation skipped on non-Unix platform for: {}", link_path.display());
             // Handle copy or other alternative here if needed, and record path in links_created_paths
         }
    }

    // Extend the original artifacts list with the successfully created link paths
    created_artifacts.extend(links_created_paths);


    // Write receipt with all created artifacts
    super::write_receipt(cask, cask_version_install_path, created_artifacts)?;

    println!("==> Successfully installed binary files for cask: {}", cask.token);

    Ok(())
}