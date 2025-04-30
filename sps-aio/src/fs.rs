// sps-aio/src/fs.rs
// Provides async filesystem operations using tokio::fs.
// Keeps sync versions for potential use or easier transition.

use std::future::Future;
use std::io; // Keep for ErrorKind checks
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use libc;
use sps_common::error::{Result, SpsError};
use tokio::fs; // Use tokio::fs for async operations
use tokio::io::AsyncWriteExt; // For async read/write traits
use tracing::{debug, error, warn};

// --- Async Versions ---

/// Asynchronously checks if a path exists (resolving symlinks).
pub async fn check_path_exists_async(path: &Path) -> Result<bool> {
    fs::try_exists(path)
        .await
        .map_err(|e| SpsError::Io(Arc::new(e)))
}

/// Asynchronously checks if a path points to a directory (resolving symlinks).
pub async fn is_directory_async(path: &Path) -> Result<bool> {
    match fs::metadata(path).await {
        Ok(meta) => Ok(meta.is_dir()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false), // Not found is not a dir
        Err(e) => Err(SpsError::Io(Arc::new(e))),
    }
}

/// Asynchronously checks if a path points to a regular file (resolving symlinks).
pub async fn is_file_async(path: &Path) -> Result<bool> {
    match fs::metadata(path).await {
        Ok(meta) => Ok(meta.is_file()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false), // Not found is not a file
        Err(e) => Err(SpsError::Io(Arc::new(e))),
    }
}

/// Asynchronously returns metadata for a path, following symlinks.
pub async fn get_metadata_async(path: &Path) -> Result<std::fs::Metadata> {
    fs::metadata(path)
        .await
        .map_err(|e| SpsError::Io(Arc::new(e)))
}

/// Asynchronously returns metadata for a path, *without* following symlinks.
pub async fn get_symlink_metadata_async(path: &Path) -> Result<std::fs::Metadata> {
    fs::symlink_metadata(path)
        .await
        .map_err(|e| SpsError::Io(Arc::new(e)))
}

/// Asynchronously creates a directory and all its parent components if they are missing.
pub async fn create_dir_all_async(path: &Path) -> Result<()> {
    debug!("Async Creating directory recursively: {}", path.display());
    fs::create_dir_all(path).await.map_err(|e| {
        error!("Async Failed create dir {}: {}", path.display(), e);
        SpsError::Io(Arc::new(e))
    })
}

/// Asynchronously removes a file. Handles NotFound gracefully.
pub async fn remove_file_async(path: &Path) -> Result<()> {
    debug!("Async Removing file: {}", path.display());
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            debug!(
                "Async File not found (already removed?): {}",
                path.display()
            );
            Ok(()) // Treat NotFound as success for idempotency
        }
        Err(e) => {
            error!("Async Failed remove file {}: {}", path.display(), e);
            Err(SpsError::Io(Arc::new(e)))
        }
    }
}

/// Asynchronously removes an empty directory. Handles NotFound gracefully.
pub async fn remove_dir_async(path: &Path) -> Result<()> {
    debug!("Async Removing directory: {}", path.display());
    match fs::remove_dir(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            debug!(
                "Async Directory not found (already removed?): {}",
                path.display()
            );
            Ok(()) // Treat NotFound as success for idempotency
        }
        Err(e) => {
            error!("Async Failed remove dir {}: {}", path.display(), e);
            Err(SpsError::Io(Arc::new(e)))
        }
    }
}

/// Asynchronously removes a directory and all its contents recursively. Handles NotFound
/// gracefully.
pub async fn remove_directory_recursive_async(path: &Path) -> Result<()> {
    debug!("Async Removing directory recursively: {}", path.display());
    match fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            debug!(
                "Async Directory not found (already removed?): {}",
                path.display()
            );
            Ok(()) // Treat NotFound as success for idempotency
        }
        Err(e) => {
            error!("Async Failed remove dir_all {}: {}", path.display(), e);
            Err(SpsError::Io(Arc::new(e)))
        }
    }
}

/// Asynchronously opens an existing file for reading.
pub async fn open_file_async(path: &Path) -> Result<fs::File> {
    debug!("Async Opening file: {}", path.display());
    fs::File::open(path).await.map_err(|e| {
        error!("Async Failed open file {}: {}", path.display(), e);
        SpsError::Io(Arc::new(e))
    })
}

/// Asynchronously reads the entire contents of a file into a string.
pub async fn read_to_string_async(path: &Path) -> Result<String> {
    debug!("Async Reading file to string: {}", path.display());
    fs::read_to_string(path).await.map_err(|e| {
        error!("Async Failed read file {}: {}", path.display(), e);
        SpsError::Io(Arc::new(e))
    })
}

/// Asynchronously reads the entire contents of a file into a byte vector.
pub async fn read_to_bytes_async(path: &Path) -> Result<Vec<u8>> {
    debug!("Async Reading file to bytes: {}", path.display());
    fs::read(path).await.map_err(|e| {
        error!("Async Failed read file {}: {}", path.display(), e);
        SpsError::Io(Arc::new(e))
    })
}

/// Asynchronously copies the contents of one file to another.
pub async fn copy_file_async(from: &Path, to: &Path) -> Result<u64> {
    debug!("Async Copying file {} -> {}", from.display(), to.display());
    // Ensure target directory exists
    if let Some(parent) = to.parent() {
        create_dir_all_async(parent).await?;
    }
    fs::copy(from, to).await.map_err(|e| {
        error!(
            "Async Failed copy file {} -> {}: {}",
            from.display(),
            to.display(),
            e
        );
        SpsError::Io(Arc::new(e))
    })
}

/// Asynchronously copies a directory recursively.
/// NOTE: This is a basic implementation. For robustness (symlinks, permissions),
/// consider using `spawn_blocking` with `fs_extra` or a dedicated async crate.
pub fn copy_recursive_async<'a>(
    from: &'a Path,
    to: &'a Path,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        debug!(
            "Async Copying directory recursively {} -> {}",
            from.display(),
            to.display()
        );
        if !is_directory_async(from).await? {
            return Err(SpsError::IoError(format!(
                "Source path is not a directory: {}",
                from.display()
            )));
        }
        create_dir_all_async(to).await?;

        let mut entries = fs::read_dir(from).await?;
        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();
            let dest_path = to.join(entry.file_name());

            if is_directory_async(&entry_path).await? {
                copy_recursive_async(&entry_path, &dest_path).await?;
            } else {
                copy_file_async(&entry_path, &dest_path).await?;
            }
        }
        Ok(())
    })
}

/// Asynchronously moves/renames a file or directory.
/// Tries `tokio::fs::rename` first. If it fails with cross-device error,
/// falls back to `copy_recursive_async` + `remove_directory_recursive_async`.
pub async fn move_path_async(from: &Path, to: &Path) -> Result<()> {
    debug!("Async Moving path {} -> {}", from.display(), to.display());
    // Ensure target directory exists
    if let Some(parent) = to.parent() {
        create_dir_all_async(parent).await?;
    }

    match fs::rename(from, to).await {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            // Cross-device link error, fallback to copy + remove
            warn!(
                "Rename failed (cross-device), falling back to copy+remove for {} -> {}",
                from.display(),
                to.display()
            );
            copy_recursive_async(from, to).await?; // Use copy_recursive for dirs too
            remove_directory_recursive_async(from).await // Remove original
        }
        Err(e) => {
            error!(
                "Async Failed move {} -> {}: {}",
                from.display(),
                to.display(),
                e
            );
            Err(SpsError::Io(Arc::new(e)))
        }
    }
}

/// Asynchronously creates a symbolic link. Unix only.
#[cfg(unix)]
pub async fn create_symlink_async(target: &Path, link: &Path) -> Result<()> {
    debug!(
        "Async Creating symlink {} -> {}",
        link.display(),
        target.display()
    );
    // Ensure parent directory for the link exists
    if let Some(parent) = link.parent() {
        create_dir_all_async(parent).await?;
    }
    // Remove existing file/link at the destination first
    let _ = remove_file_async(link).await; // Ignore error if it doesn't exist

    fs::symlink(target, link).await.map_err(|e| {
        error!(
            "Async Failed create symlink {} -> {}: {}",
            link.display(),
            target.display(),
            e
        );
        SpsError::Io(Arc::new(e))
    })
}

#[cfg(not(unix))]
pub async fn create_symlink_async(target: &Path, link: &Path) -> Result<()> {
    warn!(
        "Symlink creation not supported on this platform: {} -> {}",
        link.display(),
        target.display()
    );
    Err(SpsError::Generic(
        "Symlinks not supported on this platform".to_string(),
    ))
}

/// Asynchronously sets file permissions (Unix only). Mode is standard Unix octal mode.
#[cfg(unix)]
pub async fn set_permissions_async(path: &Path, mode: u32) -> Result<()> {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;
    debug!(
        "Async Setting permissions on {}: {:o}",
        path.display(),
        mode
    );
    let permissions = Permissions::from_mode(mode);
    fs::set_permissions(path, permissions).await.map_err(|e| {
        error!("Async Failed set permissions on {}: {}", path.display(), e);
        SpsError::Io(Arc::new(e))
    })
}

#[cfg(not(unix))]
pub async fn set_permissions_async(path: &Path, _mode: u32) -> Result<()> {
    warn!(
        "Setting permissions not fully supported on this platform: {}",
        path.display()
    );
    Ok(())
}

/// Asynchronously writes data to a file atomically using a temporary file.
/// Preserves original permissions if possible.
pub async fn atomic_write_file_async(original_path: &Path, content: &[u8]) -> Result<()> {
    let dir = original_path.parent().ok_or_else(|| {
        SpsError::IoError(format!(
            "Cannot get parent directory for {}",
            original_path.display()
        ))
    })?;

    // Ensure the directory exists
    create_dir_all_async(dir).await?;

    // Preserve original permissions if the file exists
    let original_perms = fs::metadata(original_path)
        .await
        .ok()
        .map(|m| m.permissions());

    // Use tokio's temp file capabilities or spawn_blocking for sync tempfile
    // Using spawn_blocking for simplicity here with std tempfile
    let temp_path = original_path.with_extension(format!(
        "{}.tmp",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    debug!(
        "Async Atomically writing {} bytes to {} via temp file {}",
        content.len(),
        original_path.display(),
        temp_path.display()
    );

    // Write content to temp file
    let mut temp_file = fs::File::create(&temp_path).await?;
    temp_file.write_all(content).await?;
    temp_file.flush().await?;
    temp_file.sync_all().await?; // Ensure data is physically written
    drop(temp_file); // Close the file before renaming

    // Atomically replace the original file
    if let Err(e) = fs::rename(&temp_path, original_path).await {
        error!(
            "Failed to rename temporary file {} over {}: {}",
            temp_path.display(),
            original_path.display(),
            e
        );
        // Attempt to clean up temp file on failure
        let _ = fs::remove_file(&temp_path).await;
        return Err(SpsError::Io(Arc::new(e)));
    }

    // Restore original permissions if we captured them and platform supports it
    if let Some(perms) = original_perms {
        #[cfg(unix)]
        {
            if let Err(e) = fs::set_permissions(original_path, perms).await {
                warn!(
                    "Failed to restore original permissions on {}: {}",
                    original_path.display(),
                    e
                );
            }
        }
    } else if cfg!(unix) {
        // If file didn't exist before, set default permissions (e.g., 644)
        if let Err(e) = set_permissions_async(original_path, 0o644).await {
            warn!(
                "Failed to set default permissions on new file {}: {}",
                original_path.display(),
                e
            );
        }
    }

    Ok(())
}

/// Asynchronously lists directory entries, returning basic info.
/// Skips entries that cause errors during reading.
pub async fn list_directory_entries_async(
    dir_path: &Path,
) -> Result<Vec<(String, PathBuf, bool /* is_dir */)>> {
    debug!(
        "Async Listing directory entries for: {}",
        dir_path.display()
    );
    let mut entries = Vec::new();
    let dir_path_str = dir_path.to_string_lossy().to_string(); // For logging

    let mut read_dir = match fs::read_dir(dir_path).await {
        Ok(rd) => rd,
        Err(e) => {
            error!(
                "Async Failed to read directory {}: {}",
                dir_path.display(),
                e
            );
            return Err(SpsError::Io(Arc::new(e)));
        }
    };

    while let Some(entry_res) = read_dir.next_entry().await? {
        let path = entry_res.path();
        let name = entry_res.file_name().to_string_lossy().to_string();
        match entry_res.file_type().await {
            Ok(file_type) => {
                entries.push((name, path, file_type.is_dir()));
            }
            Err(e) => {
                warn!(
                    "Async Failed to get file type for {} in {}: {}",
                    path.display(),
                    dir_path_str,
                    e
                );
                // Decide whether to skip or return error. Skipping for now.
            }
        }
    }

    Ok(entries)
}

// --- Sync Versions (Kept for reference or potential use) ---

pub fn check_path_exists_sync(path: &Path) -> bool {
    path.exists()
}
pub fn is_directory_sync(path: &Path) -> bool {
    path.is_dir()
}
pub fn is_file_sync(path: &Path) -> bool {
    path.is_file()
}
pub fn get_metadata_sync(path: &Path) -> Result<std::fs::Metadata> {
    std::fs::metadata(path).map_err(|e| SpsError::Io(Arc::new(e)))
}
pub fn get_symlink_metadata_sync(path: &Path) -> Result<std::fs::Metadata> {
    std::fs::symlink_metadata(path).map_err(|e| SpsError::Io(Arc::new(e)))
}
pub fn create_dir_all_sync(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(|e| SpsError::Io(Arc::new(e)))
}
pub fn remove_file_sync(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SpsError::Io(Arc::new(e))),
    }
}
pub fn remove_dir_sync(path: &Path) -> Result<()> {
    match std::fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SpsError::Io(Arc::new(e))),
    }
}
pub fn remove_directory_recursive_sync(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SpsError::Io(Arc::new(e))),
    }
}
pub fn read_to_string_sync(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| SpsError::Io(Arc::new(e)))
}
pub fn read_to_bytes_sync(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| SpsError::Io(Arc::new(e)))
}
pub fn copy_file_sync(from: &Path, to: &Path) -> Result<u64> {
    if let Some(parent) = to.parent() {
        create_dir_all_sync(parent)?;
    }
    std::fs::copy(from, to).map_err(|e| SpsError::Io(Arc::new(e)))
}
// Sync copy_recursive would likely use fs_extra crate
// Sync move_path would use std::fs::rename

#[cfg(unix)]
pub fn create_symlink_sync(target: &Path, link: &Path) -> Result<()> {
    if let Some(parent) = link.parent() {
        create_dir_all_sync(parent)?;
    }
    let _ = remove_file_sync(link); // Ignore error
    std::os::unix::fs::symlink(target, link).map_err(|e| SpsError::Io(Arc::new(e)))
}
#[cfg(not(unix))]
pub fn create_symlink_sync(_target: &Path, _link: &Path) -> Result<()> {
    Err(SpsError::Generic("Symlinks not supported".into()))
}

#[cfg(unix)]
pub fn set_permissions_sync(path: &Path, mode: u32) -> Result<()> {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, Permissions::from_mode(mode))
        .map_err(|e| SpsError::Io(Arc::new(e)))
}
#[cfg(not(unix))]
pub fn set_permissions_sync(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

pub fn atomic_write_file_sync(original_path: &Path, content: &[u8]) -> Result<()> {
    use std::io::Write;

    use tempfile::NamedTempFile;

    let dir = original_path.parent().ok_or_else(|| {
        SpsError::IoError(format!(
            "Cannot get parent directory for {}",
            original_path.display()
        ))
    })?;
    create_dir_all_sync(dir)?;
    let original_perms = std::fs::metadata(original_path)
        .map(|m| m.permissions())
        .ok();
    let mut temp_file = NamedTempFile::new_in(dir)?;
    temp_file.write_all(content)?;
    temp_file.flush()?;
    temp_file.as_file().sync_all()?;
    temp_file
        .persist(original_path)
        .map_err(|e| SpsError::Io(Arc::new(e.error)))?;
    if let Some(perms) = original_perms {
        #[cfg(unix)]
        {
            let _ = std::fs::set_permissions(original_path, perms);
        }
    } else if cfg!(unix) {
        let _ = set_permissions_sync(original_path, 0o644);
    }
    Ok(())
}

pub fn list_directory_entries_sync(dir_path: &Path) -> Result<Vec<(String, PathBuf, bool)>> {
    let mut entries = Vec::new();
    for entry_res in std::fs::read_dir(dir_path)? {
        let entry = entry_res?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type()?.is_dir();
        entries.push((name, path, is_dir));
    }
    Ok(entries)
}
