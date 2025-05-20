// Path: sps-core/src/install/extract.rs
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek};
#[cfg(unix)]
use std::os::unix::fs as unix_fs;
use std::path::{Component, Path, PathBuf};

use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use sps_common::error::{Result, SpsError};
use tar::{Archive, EntryType};
use tracing::{debug, error, warn};
use xz2::read::XzDecoder;
use zip::read::ZipArchive;

#[cfg(target_os = "macos")]
use crate::utils::xattr;

pub(crate) fn infer_archive_root_dir(
    archive_path: &Path,
    archive_type: &str,
) -> Result<Option<PathBuf>> {
    tracing::debug!(
        "Inferring root directory for archive: {}",
        archive_path.display()
    );
    let file = File::open(archive_path).map_err(|e| {
        SpsError::Io(std::sync::Arc::new(std::io::Error::new(
            e.kind(),
            format!("Failed to open archive {}: {}", archive_path.display(), e),
        )))
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
        _ => Err(SpsError::Generic(format!(
            "Cannot infer root dir for unsupported archive type '{}' in {}",
            archive_type,
            archive_path.display()
        ))),
    }
}

fn infer_tar_root<R: Read>(reader: R, archive_path_for_log: &Path) -> Result<Option<PathBuf>> {
    let mut archive = Archive::new(reader);
    let mut unique_roots = HashSet::new();
    let mut non_empty_entry_found = false;
    let mut first_component_name: Option<PathBuf> = None;

    for entry_result in archive.entries()? {
        let entry = entry_result.map_err(|e| {
            SpsError::Generic(format!(
                "Error reading TAR entry from {}: {}",
                archive_path_for_log.display(),
                e
            ))
        })?;
        let path = entry
            .path()
            .map_err(|e| {
                SpsError::Generic(format!(
                    "Invalid path in TAR entry from {}: {}",
                    archive_path_for_log.display(),
                    e
                ))
            })?
            .into_owned();

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
                        "Multiple top-level items found in TAR {}, cannot infer single root.",
                        archive_path_for_log.display()
                    );
                    return Ok(None);
                }
            } else {
                tracing::debug!(
                    "Non-standard top-level component ({:?}) found in TAR {}, cannot infer single root.",
                    first_comp,
                    archive_path_for_log.display()
                );
                return Ok(None);
            }
        } else {
            tracing::debug!(
                "Empty or unusual path found in TAR {}, cannot infer single root.",
                archive_path_for_log.display()
            );
            return Ok(None);
        }
    }

    if unique_roots.len() == 1 && non_empty_entry_found {
        let inferred_root = first_component_name.unwrap();
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
        Ok(None)
    } else {
        tracing::debug!(
            "No single common root directory found in TAR {}. unique_roots count: {}",
            archive_path_for_log.display(),
            unique_roots.len()
        );
        Ok(None)
    }
}

fn infer_zip_root<R: Read + Seek>(
    reader: R,
    archive_path_for_log: &Path,
) -> Result<Option<PathBuf>> {
    let mut archive = ZipArchive::new(reader).map_err(|e| {
        SpsError::Generic(format!(
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
            SpsError::Generic(format!(
                "Error reading ZIP index {} in {}: {}",
                i,
                archive_path_for_log.display(),
                e
            ))
        })?;

        let path_str = file.name();
        let path = PathBuf::from(path_str);

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
                tracing::debug!(
                    "Non-standard top-level component ({:?}) found in ZIP {}, cannot infer single root.",
                    first_comp,
                    archive_path_for_log.display()
                );
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

    if unique_roots.len() == 1 && non_empty_entry_found {
        let inferred_root = first_component_name.unwrap();
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
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
pub fn quarantine_extracted_apps_in_stage(stage_dir: &Path, agent_name: &str) -> Result<()> {
    use std::fs;

    use tracing::{debug, warn};
    debug!(
        "Searching for .app bundles in {} to apply quarantine.",
        stage_dir.display()
    );
    if stage_dir.is_dir() {
        for entry_result in fs::read_dir(stage_dir)? {
            let entry = entry_result?;
            let entry_path = entry.path();
            if entry_path.is_dir() && entry_path.extension().is_some_and(|ext| ext == "app") {
                debug!(
                    "Found app bundle in stage: {}. Applying quarantine.",
                    entry_path.display()
                );
                if let Err(e) = xattr::set_quarantine_attribute(&entry_path, agent_name) {
                    warn!(
                        "Failed to set quarantine attribute on staged app {}: {}. Installation will continue.",
                        entry_path.display(),
                        e
                    );
                }
            }
        }
    }
    Ok(())
}

pub fn extract_archive(
    archive_path: &Path,
    target_dir: &Path,
    strip_components: usize,
    archive_type: &str,
) -> Result<()> {
    debug!(
        "Extracting archive '{}' (type: {}) to '{}' (strip_components={}) using native Rust crates.",
        archive_path.display(),
        archive_type,
        target_dir.display(),
        strip_components
    );

    fs::create_dir_all(target_dir).map_err(|e| {
        SpsError::Io(std::sync::Arc::new(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to create target directory {}: {}",
                target_dir.display(),
                e
            ),
        )))
    })?;

    let file = File::open(archive_path).map_err(|e| {
        SpsError::Io(std::sync::Arc::new(std::io::Error::new(
            e.kind(),
            format!("Failed to open archive {}: {}", archive_path.display(), e),
        )))
    })?;

    let result = match archive_type {
        "zip" => extract_zip_archive(file, target_dir, strip_components, archive_path),
        "gz" | "tgz" => {
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
        "tar" => extract_tar_archive(file, target_dir, strip_components, archive_path),
        _ => Err(SpsError::Generic(format!(
            "Unsupported archive type provided for extraction: '{}' for file {}",
            archive_type,
            archive_path.display()
        ))),
    };
    #[cfg(target_os = "macos")]
    {
        if result.is_ok() {
            // Only quarantine if main extraction was successful
            if let Err(e) = quarantine_extracted_apps_in_stage(target_dir, "sps-extractor") {
                tracing::warn!(
                    "Error during post-extraction quarantine scan for {}: {}",
                    archive_path.display(),
                    e
                );
            }
        }
    }
    result
}

/// Represents a hardlink operation that was deferred.
#[cfg(unix)]
struct DeferredHardLink {
    link_path_in_archive: PathBuf,
    target_name_in_archive: PathBuf,
}

fn extract_tar_archive<R: Read>(
    reader: R,
    target_dir: &Path,
    strip_components: usize,
    archive_path_for_log: &Path,
) -> Result<()> {
    let mut archive = Archive::new(reader);
    archive.set_preserve_permissions(true);
    archive.set_unpack_xattrs(true);
    archive.set_overwrite(true);

    debug!(
        "Starting TAR extraction for {}",
        archive_path_for_log.display()
    );

    #[cfg(unix)]
    let mut deferred_hardlinks: Vec<DeferredHardLink> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for entry_result in archive.entries()? {
        let mut entry = entry_result.map_err(|e| {
            SpsError::Generic(format!(
                "Error reading TAR entry from {}: {}",
                archive_path_for_log.display(),
                e
            ))
        })?;

        let original_path_in_archive: PathBuf = entry
            .path()
            .map_err(|e| {
                SpsError::Generic(format!(
                    "Invalid path in TAR entry from {}: {}",
                    archive_path_for_log.display(),
                    e
                ))
            })?
            .into_owned();

        let stripped_components_iter: Vec<Component<'_>> = original_path_in_archive
            .components()
            .skip(strip_components)
            .collect();

        if stripped_components_iter.is_empty() {
            debug!(
                "Skipping entry due to strip_components: {:?}",
                original_path_in_archive
            );
            continue;
        }

        let mut final_target_path_on_disk = target_dir.to_path_buf();
        for comp in stripped_components_iter {
            match comp {
                Component::Normal(p) => final_target_path_on_disk.push(p),
                Component::CurDir => {}
                Component::ParentDir => {
                    let msg = format!(
                        "Unsafe '..' in TAR path {} after stripping in {}",
                        original_path_in_archive.display(),
                        archive_path_for_log.display()
                    );
                    error!("{}", msg);
                    errors.push(msg);
                    continue;
                }
                Component::Prefix(_) | Component::RootDir => {
                    let msg = format!(
                        "Disallowed component {:?} in TAR path {}",
                        comp,
                        original_path_in_archive.display()
                    );
                    error!("{}", msg);
                    errors.push(msg);
                    continue;
                }
            }
        }

        if !final_target_path_on_disk.starts_with(target_dir) {
            let msg = format!(
                "Path traversal {} -> {} detected in {}",
                original_path_in_archive.display(),
                final_target_path_on_disk.display(),
                archive_path_for_log.display()
            );
            error!("{}", msg);
            errors.push(msg);
            continue;
        }

        if let Some(parent) = final_target_path_on_disk.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    SpsError::Io(std::sync::Arc::new(io::Error::new(
                        e.kind(),
                        format!("Failed create parent dir {}: {}", parent.display(), e),
                    )))
                })?;
            }
        }

        #[cfg(unix)]
        if entry.header().entry_type() == EntryType::Link {
            if let Ok(Some(link_name_in_archive)) = entry.link_name() {
                let deferred_link = DeferredHardLink {
                    link_path_in_archive: original_path_in_archive.clone(),
                    target_name_in_archive: link_name_in_archive.into_owned(),
                };
                debug!(
                    "Deferring hardlink: archive path '{}' -> archive target '{}'",
                    original_path_in_archive.display(),
                    deferred_link.target_name_in_archive.display()
                );
                deferred_hardlinks.push(deferred_link);
                continue;
            } else {
                let msg = format!(
                    "Hardlink entry '{}' in {} has no link target name.",
                    original_path_in_archive.display(),
                    archive_path_for_log.display()
                );
                warn!("{}", msg);
                errors.push(msg);
                continue;
            }
        }

        match entry.unpack(&final_target_path_on_disk) {
            Ok(_) => debug!(
                "Unpacked TAR entry to: {}",
                final_target_path_on_disk.display()
            ),
            Err(e) => {
                if e.kind() != io::ErrorKind::AlreadyExists {
                    let msg = format!(
                        "Failed to unpack entry {:?} to {}: {}. Entry type: {:?}",
                        original_path_in_archive,
                        final_target_path_on_disk.display(),
                        e,
                        entry.header().entry_type()
                    );
                    error!("{}", msg);
                    errors.push(msg);
                } else {
                    debug!("Entry already exists at {}, skipping unpack (tar crate overwrite=true handles this).", final_target_path_on_disk.display());
                }
            }
        }
    }

    #[cfg(unix)]
    for deferred in deferred_hardlinks {
        let mut disk_link_path = target_dir.to_path_buf();
        for comp in deferred
            .link_path_in_archive
            .components()
            .skip(strip_components)
        {
            if let Component::Normal(p) = comp {
                disk_link_path.push(p);
            }
            // Other components should have been caught by safety checks above
        }

        let mut disk_target_path = target_dir.to_path_buf();
        // The link_name_in_archive is relative to the archive root *before* stripping.
        // We need to apply stripping to it as well to find its final disk location.
        for comp in deferred
            .target_name_in_archive
            .components()
            .skip(strip_components)
        {
            if let Component::Normal(p) = comp {
                disk_target_path.push(p);
            }
        }

        if !disk_target_path.starts_with(target_dir) || !disk_link_path.starts_with(target_dir) {
            let msg = format!("Skipping deferred hardlink due to path traversal attempt: link '{}' -> target '{}'", disk_link_path.display(), disk_target_path.display());
            error!("{}", msg);
            errors.push(msg);
            continue;
        }

        debug!(
            "Attempting deferred hardlink: disk link path '{}' -> disk target path '{}'",
            disk_link_path.display(),
            disk_target_path.display()
        );

        if disk_target_path.exists() {
            if let Some(parent) = disk_link_path.parent() {
                if !parent.exists() {
                    if let Err(e) = fs::create_dir_all(parent) {
                        let msg = format!(
                            "Failed to create parent directory for deferred hardlink {}: {}",
                            disk_link_path.display(),
                            e
                        );
                        error!("{}", msg);
                        errors.push(msg);
                        continue;
                    }
                }
            }

            if disk_link_path.symlink_metadata().is_ok() {
                // Check if something (file or symlink) exists at the link creation spot
                if let Err(e) = fs::remove_file(&disk_link_path) {
                    // Attempt to remove it
                    warn!("Could not remove existing file/symlink at hardlink destination {}: {}. Hardlink creation may fail.", disk_link_path.display(), e);
                }
            }

            if let Err(e) = fs::hard_link(&disk_target_path, &disk_link_path) {
                let msg = format!(
                    "Failed to create deferred hardlink '{}' -> '{}': {}. Target exists: {}",
                    disk_link_path.display(),
                    disk_target_path.display(),
                    e,
                    disk_target_path.exists()
                );
                error!("{}", msg);
                errors.push(msg);
            } else {
                debug!(
                    "Successfully created deferred hardlink: '{}' -> '{}'",
                    disk_link_path.display(),
                    disk_target_path.display()
                );
            }
        } else {
            let msg = format!(
                "Target '{}' for deferred hardlink '{}' does not exist. Hardlink not created.",
                disk_target_path.display(),
                disk_link_path.display()
            );
            error!("{}", msg);
            errors.push(msg);
        }
    }

    if !errors.is_empty() {
        return Err(SpsError::InstallError(format!(
            "Failed during TAR extraction for {} with {} error(s): {}",
            archive_path_for_log.display(),
            errors.len(),
            errors.join("; ")
        )));
    }

    debug!(
        "Finished TAR extraction for {}",
        archive_path_for_log.display()
    );
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = quarantine_extracted_apps_in_stage(target_dir, "sps-tar-extractor") {
            tracing::warn!(
                "Error during post-tar extraction quarantine scan for {}: {}",
                archive_path_for_log.display(),
                e
            );
        }
    }
    Ok(())
}

fn extract_zip_archive<R: Read + Seek>(
    reader: R,
    target_dir: &Path,
    strip_components: usize,
    archive_path_for_log: &Path,
) -> Result<()> {
    let mut archive = ZipArchive::new(reader).map_err(|e| {
        SpsError::Generic(format!(
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
            SpsError::Generic(format!(
                "Error reading ZIP index {} in {}: {}",
                i,
                archive_path_for_log.display(),
                e
            ))
        })?;

        let original_path_in_archive = match file.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => {
                debug!("Skipping unsafe ZIP entry name {}", file.name());
                continue;
            }
        };
        let stripped_components_iter: Vec<Component<'_>> = original_path_in_archive
            .components()
            .skip(strip_components)
            .collect();
        if stripped_components_iter.is_empty() {
            debug!(
                "Skipping ZIP entry {} due to strip_components",
                original_path_in_archive.display()
            );
            continue;
        }

        let mut final_target_path_on_disk = target_dir.to_path_buf();
        for comp in stripped_components_iter {
            match comp {
                Component::Normal(p) => final_target_path_on_disk.push(p),
                Component::CurDir => {}
                Component::ParentDir => {
                    error!(
                        "Unsafe '..' in ZIP path {} after strip_components",
                        original_path_in_archive.display()
                    );
                    return Err(SpsError::Generic(format!(
                        "Unsafe '..' component in ZIP path {}",
                        original_path_in_archive.display()
                    )));
                }
                Component::Prefix(_) | Component::RootDir => {
                    error!(
                        "Disallowed component {:?} in ZIP path {}",
                        comp,
                        original_path_in_archive.display()
                    );
                    return Err(SpsError::Generic(format!(
                        "Disallowed component in ZIP path {}",
                        original_path_in_archive.display()
                    )));
                }
            }
        }
        if !final_target_path_on_disk.starts_with(target_dir) {
            error!(
                "ZIP path traversal detected: {} -> {}",
                original_path_in_archive.display(),
                final_target_path_on_disk.display()
            );
            return Err(SpsError::Generic(format!(
                "ZIP path traversal detected in {}",
                archive_path_for_log.display()
            )));
        }

        if let Some(parent) = final_target_path_on_disk.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    SpsError::Io(std::sync::Arc::new(io::Error::new(
                        e.kind(),
                        format!("Failed create dir {}: {}", parent.display(), e),
                    )))
                })?;
            }
        }

        if file.is_dir() {
            if !final_target_path_on_disk.exists() {
                // Only create if it doesn't exist
                fs::create_dir_all(&final_target_path_on_disk).map_err(|e| {
                    SpsError::Io(std::sync::Arc::new(io::Error::new(
                        e.kind(),
                        format!(
                            "Failed create dir {}: {}",
                            final_target_path_on_disk.display(),
                            e
                        ),
                    )))
                })?;
            }
        } else if file.is_symlink() {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            let link_target_str = String::from_utf8_lossy(&buf).to_string();
            let link_target_path = PathBuf::from(link_target_str);

            #[cfg(unix)]
            {
                if final_target_path_on_disk.exists()
                    || final_target_path_on_disk.symlink_metadata().is_ok()
                {
                    let _ = fs::remove_file(&final_target_path_on_disk); // Attempt to remove
                                                                         // existing item
                }
                unix_fs::symlink(&link_target_path, &final_target_path_on_disk).map_err(|e| {
                    debug!(
                        "Failed to create symlink {} -> {}: {}",
                        final_target_path_on_disk.display(),
                        link_target_path.display(),
                        e
                    );
                    SpsError::Io(std::sync::Arc::new(e))
                })?;
            }
            #[cfg(not(unix))]
            {
                warn!(
                    "Cannot create symlink on non-unix system: {} -> {}",
                    final_target_path_on_disk.display(),
                    link_target_path.display()
                );
            }
        } else {
            // Regular file
            if final_target_path_on_disk.exists() {
                // Overwrite if exists
                match fs::remove_file(&final_target_path_on_disk) {
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {} // Fine if not found
                    Err(e) => return Err(SpsError::Io(std::sync::Arc::new(e))), /* Other errors are
                                                                          * problematic */
                }
            }
            let mut out_file = File::create(&final_target_path_on_disk).map_err(|e| {
                SpsError::Io(std::sync::Arc::new(io::Error::new(
                    e.kind(),
                    format!(
                        "Failed create file {}: {}",
                        final_target_path_on_disk.display(),
                        e
                    ),
                )))
            })?;
            io::copy(&mut file, &mut out_file)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                if !file.is_symlink() && final_target_path_on_disk.is_file() {
                    // Check if it's a file before setting permissions
                    fs::set_permissions(
                        &final_target_path_on_disk,
                        fs::Permissions::from_mode(mode),
                    )?;
                }
            }
        }
    }
    debug!(
        "Finished ZIP extraction for {}",
        archive_path_for_log.display()
    );
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = quarantine_extracted_apps_in_stage(target_dir, "sps-zip-extractor") {
            tracing::warn!(
                "Error during post-zip extraction quarantine scan for {}: {}",
                archive_path_for_log.display(),
                e
            );
        }
    }
    Ok(())
}
