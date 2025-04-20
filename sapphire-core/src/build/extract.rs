// ===== sapphire-core/src/build/extract.rs =====
// Contains logic for extracting various archive formats.
// Compatible with tar v0.4.40+: `Entry::unpack` returns `io::Result<tar::Unpacked>`
// and `Entry::path` returns `io::Result<Cow<Path>>`.

use crate::utils::error::{Result, SapphireError};
use log::{debug, error, warn};
use std::fs::{self, File};
use std::io::{self, Read, Seek};
use std::path::{Component, Path, PathBuf};

use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use xz2::read::XzDecoder;
use zip::read::ZipArchive;

/// Extracts an archive to the target directory using native Rust crates.
/// Supports `.tar`, `.tar.gz`, `.tar.bz2`, `.tar.xz`, and `.zip`.
/// `strip_components` behaves like the GNU tar `--strip-components` flag.
pub fn extract_archive(
    archive_path: &Path,
    target_dir: &Path,
    strip_components: usize,
) -> Result<()> {
    debug!(
        "Extracting archive '{}' to '{}' (strip_components={}) using native Rust crates.",
        archive_path.display(),
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

    let extension = archive_path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");

    let filename_str = archive_path.file_name().unwrap_or_default().to_string_lossy();

    // --- Determine archive type and extract ---
    if filename_str.ends_with(".tar.gz") || filename_str.ends_with(".tgz") {
        let tar = GzDecoder::new(file);
        extract_tar_archive(tar, target_dir, strip_components, archive_path)
    } else if filename_str.ends_with(".tar.bz2")
        || filename_str.ends_with(".tbz")
        || filename_str.ends_with(".tbz2")
    {
        let tar = BzDecoder::new(file);
        extract_tar_archive(tar, target_dir, strip_components, archive_path)
    } else if filename_str.ends_with(".tar.xz") || filename_str.ends_with(".txz") {
        let tar = XzDecoder::new(file);
        extract_tar_archive(tar, target_dir, strip_components, archive_path)
    } else if filename_str.ends_with(".tar") {
        // No decompression needed
        extract_tar_archive(file, target_dir, strip_components, archive_path)
    } else if extension == "zip" {
        extract_zip_archive(file, target_dir, strip_components, archive_path)
    } else {
        Err(SapphireError::Generic(format!(
            "Unsupported archive format for native extraction: {}",
            filename_str
        )))
    }
}

// --- Tar Extraction Helper ---
fn extract_tar_archive<R: Read>(
    reader: R,
    target_dir: &Path,
    strip_components: usize,
    archive_path_for_log: &Path, // For logging only
) -> Result<()> {

    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true); // Preserve permissions
    archive.set_unpack_xattrs(true);        // Preserve xattrs
    archive.set_overwrite(false);           // Do not overwrite existing files

    debug!("Starting TAR extraction for {}", archive_path_for_log.display());

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
            debug!("Skipping entry due to strip_components: {:?}", original_path);
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

        // Unpack entry
        match entry.unpack(&target_path) {
            Ok(_) => debug!("Unpacked TAR entry to: {}", target_path.display()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                warn!(
                    "Entry exists, skipping unpack {}: {}",
                    target_path.display(), e
                );
            }
            Err(e) => {
                error!(
                    "Failed to unpack {:?} to {}: {}",
                    original_path, target_path.display(), e
                );
                return Err(SapphireError::Generic(format!(
                    "Failed unpack {:?}: {}",
                    original_path, e
                )));
            }
        }
    }
    debug!("Finished TAR extraction for {}", archive_path_for_log.display());
    Ok(())
}

// --- Zip Extraction Helper ---
fn extract_zip_archive<R: Read + Seek>(
    reader: R,
    target_dir: &Path,
    strip_components: usize,
    archive_path_for_log: &Path,
) -> Result<()> {
    let mut archive = ZipArchive::new(reader).map_err(|e| {
        SapphireError::Generic(format!(
            "Failed to open ZIP {}: {}",
            archive_path_for_log.display(), e
        ))
    })?;
    debug!("Starting ZIP extraction for {}", archive_path_for_log.display());

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| {
            SapphireError::Generic(format!(
                "Error reading ZIP index {} in {}: {}",
                i, archive_path_for_log.display(), e
            ))
        })?;

        let original = match file.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => {
                warn!("Skipping unsafe ZIP name {}", file.name());
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
                original.display(), target_path.display()
            );
            return Err(SapphireError::Generic(format!(
                "ZIP traversal in {}",
                archive_path_for_log.display()
            )));
        }

        if file.is_dir() {
            fs::create_dir_all(&target_path).map_err(|e| SapphireError::Io(io::Error::new(
                e.kind(), format!("Failed create dir {}: {}", target_path.display(), e)
            )))?;
        } else if file.is_symlink() {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).ok();
            }
            let mut buf = Vec::new(); file.read_to_end(&mut buf)?;
            let link = PathBuf::from(String::from_utf8_lossy(&buf).to_string());
            #[cfg(unix)] std::os::unix::fs::symlink(&link, &target_path).ok();
        } else {
            if let Some(p) = target_path.parent() { fs::create_dir_all(p).ok(); }
            let mut out = File::create(&target_path)?;
            io::copy(&mut file, &mut out)?;
        }
    }
    debug!("Finished ZIP extraction for {}", archive_path_for_log.display());
    Ok(())
}