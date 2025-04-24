// ===== sapphire-core/src/build/extract.rs =====
// Contains logic for extracting various archive formats.
// Compatible with tar v0.4.40+: `Entry::unpack` returns `io::Result<tar::Unpacked>`
// and `Entry::path` returns `io::Result<Cow<Path>>`.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek};
use std::path::{Component, Path, PathBuf};

use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use tar::Archive;
use tracing::{debug, error};
use xz2::read::XzDecoder;
use zip::read::ZipArchive;

use crate::utils::error::{Result, SapphireError};

/// Infers the single top-level directory within an archive, if one exists.
/// Returns Ok(Some(PathBuf)) if a single root dir is found (e.g., "foo-1.2/").
/// Returns Ok(None) if the archive is flat or has multiple top-level items.
/// Returns Err on I/O or parsing errors.
pub(crate) fn infer_archive_root_dir(
    archive_path: &Path,
    archive_type: &str,
) -> Result<Option<PathBuf>> {
    tracing::debug!(
        "Inferring root directory for archive: {}",
        archive_path.display()
    );
    let file = File::open(archive_path).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to open archive {}: {}", archive_path.display(), e),
        ))
    })?;

    match archive_type {
        "zip" => infer_zip_root(file, archive_path),
        "gz" | "tgz" => {
            let decompressed = GzDecoder::new(file);
            infer_tar_root(decompressed, archive_path)
        }
        "bz2" | "tbz" | "tbz2" => {
            let decompressed = BzDecoder::new(file);
            infer_tar_root(decompressed, archive_path)
        }
        "xz" | "txz" => {
            let decompressed = XzDecoder::new(file);
            infer_tar_root(decompressed, archive_path)
        }
        "tar" => infer_tar_root(file, archive_path),
        _ => Err(SapphireError::Generic(format!(
            "Cannot infer root dir for unsupported archive type '{}' in {}",
            archive_type,
            archive_path.display()
        ))),
    }
}

// Helper for TAR formats
fn infer_tar_root<R: Read>(reader: R, archive_path_for_log: &Path) -> Result<Option<PathBuf>> {
    let mut archive = Archive::new(reader);
    let mut unique_roots = HashSet::new();
    let mut non_empty_entry_found = false;
    let mut first_component_name: Option<PathBuf> = None;

    for entry_result in archive.entries()? {
        let entry = entry_result.map_err(|e| {
            SapphireError::Generic(format!(
                "Error reading TAR entry from {}: {}",
                archive_path_for_log.display(),
                e
            ))
        })?;
        let path = entry
            .path()
            .map_err(|e| {
                SapphireError::Generic(format!(
                    "Invalid path in TAR entry from {}: {}",
                    archive_path_for_log.display(),
                    e
                ))
            })?
            .into_owned();

        // Ignore metadata-only entries like pax headers if they have no components
        if path.components().next().is_none() {
            continue;
        }

        if let Some(first_comp) = path.components().next() {
            if let Component::Normal(name) = first_comp {
                non_empty_entry_found = true;
                let current_root = PathBuf::from(name);
                if first_component_name.is_none() {
                    first_component_name = Some(current_root.clone());
                }
                unique_roots.insert(current_root);

                // If we find more than one unique root, we can stop early
                if unique_roots.len() > 1 {
                    tracing::debug!(
                        "Multiple top-level items found in TAR {}, cannot infer single root.",
                        archive_path_for_log.display()
                    );
                    return Ok(None);
                }
            } else {
                // Found a non-Normal component at the top level (e.g., .., /)
                tracing::debug!(
                    "Non-standard top-level component ({:?}) found in TAR {}, cannot infer single root.",
                    first_comp, archive_path_for_log.display()
                );
                return Ok(None); // Archive is not structured under a single root dir
            }
        } else {
            // Path is empty or unusual, treat as non-standard structure
            tracing::debug!(
                "Empty or unusual path found in TAR {}, cannot infer single root.",
                archive_path_for_log.display()
            );
            return Ok(None);
        }
    }

    // After checking all entries:
    if unique_roots.len() == 1 && non_empty_entry_found {
        let inferred_root = first_component_name.unwrap(); // Safe unwrap as len == 1
        tracing::debug!(
            "Inferred single root directory in TAR {}: {}",
            archive_path_for_log.display(),
            inferred_root.display()
        );
        Ok(Some(inferred_root))
    } else if !non_empty_entry_found {
        tracing::warn!(
            "TAR archive {} appears to be empty or contain only metadata.",
            archive_path_for_log.display()
        );
        Ok(None) // Empty archive doesn't have a root dir
    } else {
        // This case (len == 0 but non_empty_entry_found is true) shouldn't happen
        // If len > 1, it was handled in the loop
        tracing::debug!(
            "No single common root directory found in TAR {}. unique_roots count: {}",
            archive_path_for_log.display(),
            unique_roots.len()
        );
        Ok(None) // Flat archive or multiple roots
    }
}

// Helper for ZIP format
fn infer_zip_root<R: Read + Seek>(
    reader: R,
    archive_path_for_log: &Path,
) -> Result<Option<PathBuf>> {
    let mut archive = ZipArchive::new(reader).map_err(|e| {
        SapphireError::Generic(format!(
            "Failed to open ZIP {}: {}",
            archive_path_for_log.display(),
            e
        ))
    })?;
    let mut unique_roots = HashSet::new();
    let mut non_empty_entry_found = false;
    let mut first_component_name: Option<PathBuf> = None;

    for i in 0..archive.len() {
        let file = archive.by_index_raw(i).map_err(|e| {
            // Use by_index_raw to avoid full decompression
            SapphireError::Generic(format!(
                "Error reading ZIP index {} in {}: {}",
                i,
                archive_path_for_log.display(),
                e
            ))
        })?;

        // Use file.name() which is the raw path string from the central directory
        let path_str = file.name();
        let path = PathBuf::from(path_str); // Convert raw string to PathBuf

        // Ignore metadata-only entries if they have no components
        if path.components().next().is_none() {
            continue;
        }

        if let Some(first_comp) = path.components().next() {
            if let Component::Normal(name) = first_comp {
                non_empty_entry_found = true;
                let current_root = PathBuf::from(name);
                if first_component_name.is_none() {
                    first_component_name = Some(current_root.clone());
                }
                unique_roots.insert(current_root);

                if unique_roots.len() > 1 {
                    tracing::debug!(
                        "Multiple top-level items found in ZIP {}, cannot infer single root.",
                        archive_path_for_log.display()
                    );
                    return Ok(None);
                }
            } else {
                tracing::debug!("Non-standard top-level component ({:?}) found in ZIP {}, cannot infer single root.", first_comp, archive_path_for_log.display());
                return Ok(None);
            }
        } else {
            tracing::debug!(
                "Empty or unusual path ('{}') found in ZIP {}, cannot infer single root.",
                path_str,
                archive_path_for_log.display()
            );
            return Ok(None);
        }
    }

    // After checking all entries:
    if unique_roots.len() == 1 && non_empty_entry_found {
        let inferred_root = first_component_name.unwrap(); // Safe unwrap
        tracing::debug!(
            "Inferred single root directory in ZIP {}: {}",
            archive_path_for_log.display(),
            inferred_root.display()
        );
        Ok(Some(inferred_root))
    } else if !non_empty_entry_found {
        tracing::warn!(
            "ZIP archive {} appears to be empty or contain only metadata.",
            archive_path_for_log.display()
        );
        Ok(None)
    } else {
        tracing::debug!(
            "No single common root directory found in ZIP {}. unique_roots count: {}",
            archive_path_for_log.display(),
            unique_roots.len()
        );
        Ok(None) // Flat archive or multiple roots
    }
}

/// Extracts an archive to the target directory using native Rust crates.
/// Supports `.tar`, `.tar.gz`, `.tar.bz2`, `.tar.xz`, and `.zip`.
/// `strip_components` behaves like the GNU tar `--strip-components` flag.
/// `archive_type` should be the determined extension (e.g., "zip", "gz", "bz2", "xz", "tar").
pub fn extract_archive(
    archive_path: &Path,
    target_dir: &Path,
    strip_components: usize,
    archive_type: &str, // <-- Parameter added
) -> Result<()> {
    debug!(
        "Extracting archive '{}' (type: {}) to '{}' (strip_components={}) using native Rust crates.",
        archive_path.display(),
        archive_type,
        target_dir.display(),
        strip_components
    );

    fs::create_dir_all(target_dir).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to create target directory {}: {}",
                target_dir.display(),
                e
            ),
        ))
    })?;

    let file = File::open(archive_path).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to open archive {}: {}", archive_path.display(), e),
        ))
    })?;

    // --- Determine archive type and extract ---
    // Use the provided archive_type instead of inspecting filename/extension here
    match archive_type {
        "zip" => extract_zip_archive(file, target_dir, strip_components, archive_path),
        "gz" | "tgz" => {
            // infer often returns "gz" for .tar.gz
            let tar = GzDecoder::new(file);
            extract_tar_archive(tar, target_dir, strip_components, archive_path)
        }
        "bz2" | "tbz" | "tbz2" => {
            let tar = BzDecoder::new(file);
            extract_tar_archive(tar, target_dir, strip_components, archive_path)
        }
        "xz" | "txz" => {
            let tar = XzDecoder::new(file);
            extract_tar_archive(tar, target_dir, strip_components, archive_path)
        }
        "tar" => {
            // No decompression needed
            extract_tar_archive(file, target_dir, strip_components, archive_path)
        }
        // Add other types like "7z" here if you add support
        _ => Err(SapphireError::Generic(format!(
            "Unsupported archive type provided for extraction: '{}' for file {}",
            archive_type,
            archive_path.display()
        ))),
    }
}

// --- Tar Extraction Helper (unchanged) ---
fn extract_tar_archive<R: Read>(
    reader: R,
    target_dir: &Path,
    strip_components: usize,
    archive_path_for_log: &Path, // For logging only
) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true); // Preserve permissions
    archive.set_unpack_xattrs(true); // Preserve xattrs
    archive.set_overwrite(false); // Do not overwrite existing files

    debug!(
        "Starting TAR extraction for {}",
        archive_path_for_log.display()
    );

    for entry_result in archive.entries()? {
        let mut entry = entry_result.map_err(|e| {
            SapphireError::Generic(format!(
                "Error reading TAR entry from {}: {}",
                archive_path_for_log.display(),
                e
            ))
        })?;

        // Obtain an owned copy of the path to drop the borrow.
        let original_path: PathBuf = entry
            .path()
            .map_err(|e| {
                SapphireError::Generic(format!(
                    "Invalid path in TAR entry from {}: {}",
                    archive_path_for_log.display(),
                    e
                ))
            })?
            .into_owned();

        // Strip leading components
        let stripped: Vec<_> = original_path.components().skip(strip_components).collect();
        if stripped.is_empty() {
            debug!(
                "Skipping entry due to strip_components: {:?}",
                original_path
            );
            continue;
        }

        let mut target_path = target_dir.to_path_buf();
        for comp in stripped {
            match comp {
                Component::Normal(p) => target_path.push(p),
                Component::CurDir => {}
                Component::ParentDir => {
                    error!(
                        "Unsafe '..' in TAR path {} after stripping in {}",
                        original_path.display(),
                        archive_path_for_log.display()
                    );
                    return Err(SapphireError::Generic(format!(
                        "Unsafe '..' component in {}",
                        original_path.display()
                    )));
                }
                Component::Prefix(_) | Component::RootDir => {
                    error!(
                        "Disallowed component {:?} in TAR path {}",
                        comp,
                        original_path.display()
                    );
                    return Err(SapphireError::Generic(format!(
                        "Disallowed component in {}",
                        original_path.display()
                    )));
                }
            }
        }
        if !target_path.starts_with(target_dir) {
            error!(
                "Path traversal {} -> {} detected in {}",
                original_path.display(),
                target_path.display(),
                archive_path_for_log.display()
            );
            return Err(SapphireError::Generic(format!(
                "Path traversal detected in {}",
                archive_path_for_log.display()
            )));
        }

        // Ensure parent directory exists before unpacking
        if let Some(parent) = target_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    SapphireError::Io(io::Error::new(
                        e.kind(),
                        format!("Failed create parent dir {}: {}", parent.display(), e),
                    ))
                })?;
            }
        }

        // Unpack entry
        match entry.unpack(&target_path) {
            Ok(_) => debug!("Unpacked TAR entry to: {}", target_path.display()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                debug!(
                    "Entry exists, skipping unpack {}: {}",
                    target_path.display(),
                    e
                );
            }
            Err(e) => {
                error!(
                    "Failed to unpack {:?} to {}: {}",
                    original_path,
                    target_path.display(),
                    e
                );
                return Err(SapphireError::Generic(format!(
                    "Failed unpack {original_path:?}: {e}"
                )));
            }
        }
    }
    debug!(
        "Finished TAR extraction for {}",
        archive_path_for_log.display()
    );
    Ok(())
}

// --- Zip Extraction Helper (unchanged) ---
fn extract_zip_archive<R: Read + Seek>(
    reader: R,
    target_dir: &Path,
    strip_components: usize,
    archive_path_for_log: &Path,
) -> Result<()> {
    let mut archive = ZipArchive::new(reader).map_err(|e| {
        SapphireError::Generic(format!(
            "Failed to open ZIP {}: {}",
            archive_path_for_log.display(),
            e
        ))
    })?;
    debug!(
        "Starting ZIP extraction for {}",
        archive_path_for_log.display()
    );

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| {
            SapphireError::Generic(format!(
                "Error reading ZIP index {} in {}: {}",
                i,
                archive_path_for_log.display(),
                e
            ))
        })?;

        let original = match file.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => {
                debug!("Skipping unsafe ZIP name {}", file.name());
                continue;
            }
        };
        let stripped: Vec<_> = original.components().skip(strip_components).collect();
        if stripped.is_empty() {
            debug!("Skipping ZIP {} due to strip", original.display());
            continue;
        }

        let mut target_path = target_dir.to_path_buf();
        for comp in stripped {
            match comp {
                Component::Normal(p) => target_path.push(p),
                Component::CurDir => {}
                Component::ParentDir => {
                    error!("Unsafe '..' in ZIP {} after strip", original.display());
                    return Err(SapphireError::Generic(format!(
                        "Unsafe '..' in ZIP {}",
                        original.display()
                    )));
                }
                Component::Prefix(_) | Component::RootDir => {
                    error!("Disallowed comp {:?} in ZIP {}", comp, original.display());
                    return Err(SapphireError::Generic(format!(
                        "Disallowed comp in ZIP {}",
                        original.display()
                    )));
                }
            }
        }
        if !target_path.starts_with(target_dir) {
            error!(
                "ZIP traversal {} -> {}",
                original.display(),
                target_path.display()
            );
            return Err(SapphireError::Generic(format!(
                "ZIP traversal in {}",
                archive_path_for_log.display()
            )));
        }

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    SapphireError::Io(io::Error::new(
                        e.kind(),
                        format!("Failed create dir {}: {}", parent.display(), e),
                    ))
                })?;
            }
        }

        if file.is_dir() {
            // Directory entry in zip - ensure it exists on filesystem
            if !target_path.exists() {
                fs::create_dir_all(&target_path).map_err(|e| {
                    SapphireError::Io(io::Error::new(
                        e.kind(),
                        format!("Failed create dir {}: {}", target_path.display(), e),
                    ))
                })?;
            }
        } else if file.is_symlink() {
            // Symlink entry in zip
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            let link_target = PathBuf::from(String::from_utf8_lossy(&buf).to_string());
            #[cfg(unix)]
            {
                // Remove existing file/link at target first
                if target_path.exists() || target_path.symlink_metadata().is_ok() {
                    let _ = fs::remove_file(&target_path); // Ignore error if it doesn't exist
                }
                std::os::unix::fs::symlink(&link_target, &target_path).map_err(|e| {
                    debug!(
                        "Failed to create symlink {} -> {}: {}",
                        target_path.display(),
                        link_target.display(),
                        e
                    );
                    SapphireError::Io(e) // Propagate error
                })?;
            }
            #[cfg(not(unix))]
            {
                debug!(
                    "Cannot create symlink on non-unix system: {} -> {}",
                    target_path.display(),
                    link_target.display()
                );
                // Potentially write a file with the link target path?
            }
        } else {
            // Regular file entry in zip
            // Remove existing file at target first to avoid errors
            if target_path.exists() {
                let _ = fs::remove_file(&target_path);
            }
            let mut out_file = File::create(&target_path).map_err(|e| {
                SapphireError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed create file {}: {}", target_path.display(), e),
                ))
            })?;
            io::copy(&mut file, &mut out_file)?;
        }

        // Set permissions if available (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                // Check if it's a symlink; don't chmod symlinks directly
                if !file.is_symlink() {
                    fs::set_permissions(&target_path, fs::Permissions::from_mode(mode))?;
                }
            }
        }
    }
    debug!(
        "Finished ZIP extraction for {}",
        archive_path_for_log.display()
    );
    Ok(())
}
