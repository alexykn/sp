/*
File: sp-aio/src/fs.rs (New File)
Purpose: Primitive synchronous filesystem operations.
*/
use std::{
    fs::{self, File, Permissions},
    io::{self, Read, Write},
    os::unix::fs::{symlink, PermissionsExt}, // Keep symlink here
    path::{Path, PathBuf},
    sync::Arc,
};

use sps_common::error::{Result, SpsError};
use tempfile::NamedTempFile;
use tracing::{debug, error, warn};

/// Checks if a path exists (resolving symlinks).
pub fn check_path_exists(path: &Path) -> bool {
    path.exists()
}

/// Checks if a path exists without following symlinks.
pub fn check_symlink_exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// Checks if a path points to a directory (resolving symlinks).
pub fn is_directory(path: &Path) -> bool {
    path.is_dir()
}

/// Checks if a path points to a regular file (resolving symlinks).
pub fn is_file(path: &Path) -> bool {
    path.is_file()
}

/// Returns metadata for a path, following symlinks.
pub fn get_metadata(path: &Path) -> Result<fs::Metadata> {
    fs::metadata(path).map_err(SpsError::from)
}

/// Returns metadata for a path, *without* following symlinks.
pub fn get_symlink_metadata(path: &Path) -> Result<fs::Metadata> {
    fs::symlink_metadata(path).map_err(SpsError::from)
}

/// Creates a directory and all its parent components if they are missing.
pub fn create_dir_all(path: &Path) -> Result<()> {
    debug!("Creating directory recursively: {}", path.display());
    fs::create_dir_all(path).map_err(|e| {
        error!("Failed create dir {}: {}", path.display(), e);
        SpsError::from(e)
    })
}

/// Removes a file.
pub fn remove_file(path: &Path) -> Result<()> {
    debug!("Removing file: {}", path.display());
    fs::remove_file(path).map_err(|e| {
        if e.kind() != io::ErrorKind::NotFound {
            error!("Failed remove file {}: {}", path.display(), e);
        }
        SpsError::from(e)
    })
}

/// Removes an empty directory.
pub fn remove_dir(path: &Path) -> Result<()> {
    debug!("Removing directory: {}", path.display());
    fs::remove_dir(path).map_err(|e| {
        if e.kind() != io::ErrorKind::NotFound {
            error!("Failed remove dir {}: {}", path.display(), e);
        }
        SpsError::from(e)
    })
}

/// Removes a directory and all its contents recursively.
pub fn remove_directory_recursive(path: &Path) -> Result<()> {
    debug!("Removing directory recursively: {}", path.display());
    fs::remove_dir_all(path).map_err(|e| {
        if e.kind() != io::ErrorKind::NotFound {
            error!("Failed remove dir_all {}: {}", path.display(), e);
        }
        SpsError::from(e)
    })
}

/// Creates a new file.
pub fn create_file(path: &Path) -> Result<File> {
    debug!("Creating file: {}", path.display());
    File::create(path).map_err(|e| {
        error!("Failed create file {}: {}", path.display(), e);
        SpsError::from(e)
    })
}

/// Opens an existing file for reading.
pub fn open_file(path: &Path) -> Result<File> {
    debug!("Opening file: {}", path.display());
    File::open(path).map_err(|e| {
        error!("Failed open file {}: {}", path.display(), e);
        SpsError::from(e)
    })
}

/// Reads the entire contents of a file into a string.
pub fn read_to_string(path: &Path) -> Result<String> {
    debug!("Reading file to string: {}", path.display());
    fs::read_to_string(path).map_err(|e| {
        error!("Failed read file {}: {}", path.display(), e);
        SpsError::from(e)
    })
}

/// Reads the entire contents of a file into a byte vector.
pub fn read_to_bytes(path: &Path) -> Result<Vec<u8>> {
    debug!("Reading file to bytes: {}", path.display());
    fs::read(path).map_err(|e| {
        error!("Failed read file {}: {}", path.display(), e);
        SpsError::from(e)
    })
}

/// Copies the entire contents of a reader to a writer.
pub fn copy_stream<R: Read + ?Sized, W: Write + ?Sized>(reader: &mut R, writer: &mut W) -> Result<u64> {
    io::copy(reader, writer).map_err(SpsError::from)
}

/// Creates a symbolic link. Unix only.
#[cfg(unix)]
pub fn create_symlink(target: &Path, link: &Path) -> Result<()> {
    debug!("Creating symlink {} -> {}", link.display(), target.display());
    symlink(target, link).map_err(|e| {
        error!(
            "Failed create symlink {} -> {}: {}",
            link.display(),
            target.display(),
            e
        );
        SpsError::from(e)
    })
}

#[cfg(not(unix))]
pub fn create_symlink(target: &Path, link: &Path) -> Result<()> {
    warn!(
        "Symlink creation not supported on this platform: {} -> {}",
        link.display(),
        target.display()
    );
    Err(SpsError::Generic(
        "Symlinks not supported on this platform".to_string(),
    ))
}

/// Sets file permissions (Unix only). Mode is standard Unix octal mode.
#[cfg(unix)]
pub fn set_permissions(path: &Path, mode: u32) -> Result<()> {
    debug!("Setting permissions on {}: {:o}", path.display(), mode);
    fs::set_permissions(path, Permissions::from_mode(mode)).map_err(|e| {
        error!("Failed set permissions on {}: {}", path.display(), e);
        SpsError::from(e)
    })
}

#[cfg(not(unix))]
pub fn set_permissions(path: &Path, _mode: u32) -> Result<()> {
    warn!(
        "Setting permissions not fully supported on this platform: {}",
        path.display()
    );
    // No-op on non-unix, return Ok
    Ok(())
}

/// Atomically writes data to a file using a temporary file.
/// Preserves original permissions if possible.
pub fn atomic_write_file(original_path: &Path, content: &[u8]) -> Result<()> {
    let dir = original_path.parent().ok_or_else(|| {
        SpsError::IoError(format!(
            "Cannot get parent directory for {}",
            original_path.display()
        ))
    })?;

    // Ensure the directory exists
    create_dir_all(dir)?;

    // Preserve original permissions if the file exists
    let original_perms = fs::metadata(original_path).map(|m| m.permissions()).ok();

    let mut temp_file = NamedTempFile::new_in(dir)?;
    let temp_path = temp_file.path().to_path_buf(); // Store path before consuming temp_file

    debug!(
        "Atomically writing {} bytes to {} via temp file {}",
        content.len(),
        original_path.display(),
        temp_path.display()
    );

    // Write content
    temp_file.write_all(content)?;
    temp_file.flush()?; // Ensure data is flushed from application buffer to OS buffer
    temp_file.as_file().sync_all()?; // Attempt to sync data from OS buffer to disk

    // Atomically replace the original file with the temporary file
    temp_file.persist(original_path).map_err(|e| {
        error!(
            "Failed to persist/rename temporary text file {} over {}: {}",
            temp_path.display(), // Use stored path for logging
            original_path.display(),
            e.error // Log the underlying IO error
        );
        SpsError::Io(Arc::new(e.error)) // Return the IO error wrapped
    })?;

    // Restore original permissions if we captured them and platform supports it
    if let Some(perms) = original_perms {
        #[cfg(unix)]
        {
            if let Err(e) = fs::set_permissions(original_path, perms) {
                warn!(
                    "Failed to restore original permissions on {}: {}",
                    original_path.display(),
                    e
                );
            }
        }
        // Non-unix: permissions might not be fully preserved, but we don't error.
    } else if cfg!(unix) {
        // If file didn't exist before, set default permissions (e.g., 644)
        if let Err(e) = set_permissions(original_path, 0o644) {
             warn!("Failed to set default permissions on new file {}: {}", original_path.display(), e);
        }
    }

    Ok(())
}

/// Lists directory entries, returning basic info.
/// Skips entries that cause errors during reading.
pub fn list_directory_entries(
    dir_path: &Path,
) -> Result<Vec<(String, PathBuf, bool /* is_dir */)>> {
    debug!("Listing directory entries for: {}", dir_path.display());
    let mut entries = Vec::new();
    let dir_path_str = dir_path.to_string_lossy().to_string(); // For logging

    match fs::read_dir(dir_path) {
        Ok(read_dir) => {
            for entry_res in read_dir {
                match entry_res {
                    Ok(entry) => {
                        let path = entry.path();
                        let name = entry.file_name().to_string_lossy().to_string();
                        match entry.file_type() {
                            Ok(file_type) => {
                                entries.push((name, path, file_type.is_dir()));
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to get file type for {} in {}: {}",
                                    path.display(),
                                    dir_path_str,
                                    e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Error reading entry in {}: {}", dir_path_str, e);
                    }
                }
            }
            Ok(entries)
        }
        Err(e) => {
            error!("Failed to read directory {}: {}", dir_path.display(), e);
            Err(SpsError::from(e))
        }
    }
}
