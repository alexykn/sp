// TODO: ensure `futures-util` and `async-zip` are added to Cargo.toml
// sps-aio/src/extract.rs (NEW FILE)
// Handles archive extraction asynchronously.

use std::io::{Cursor, Read, Seek}; // Needed for zip and trait bounds
use std::path::{Component, Path, PathBuf};

use async_zip::base::read::mem::ZipFileReader;
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use futures_util::stream::StreamExt;
use sps_common::error::{Result, SpsError};
use tar;
use tokio::fs::{self, File};
use tokio::io::{AsyncRead, AsyncWriteExt, BufReader}; // Use tokio IO traits
use tokio_tar; // Use tokio_tar
use tracing::{debug, error, warn};
use xz2::read::XzDecoder;
use zip::ZipArchive;

/// Asynchronously extracts an archive file to a target directory, stripping leading path
/// components. Infers archive type from path extension.
pub async fn extract_archive_async(
    archive_path: &Path,
    target_dir: &Path,
    strip_components: usize,
) -> Result<()> {
    let archive_type = archive_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    debug!(
        "Async Extracting archive '{}' (type: {}) to '{}' (strip_components={})",
        archive_path.display(),
        archive_type,
        target_dir.display(),
        strip_components
    );

    fs::create_dir_all(target_dir).await?; // Ensure target dir exists

    let file = File::open(archive_path).await?;
    let reader = BufReader::new(file); // Use BufReader for efficiency

    match archive_type.as_str() {
        "zip" => {
            // async_zip requires Read + Seek, which BufReader<File> doesn't implement directly
            // async. Read the whole file into memory for zip processing.
            // Alternative: Use spawn_blocking with std::fs::File and sync zip crate if files are
            // huge.
            debug!("Reading ZIP file into memory for async processing...");
            let zip_bytes = fs::read(archive_path).await?;
            let cursor = Cursor::new(zip_bytes);
            let target_dir = target_dir.to_path_buf();
            tokio::task::spawn_blocking(move || {
                let mut archive = ZipArchive::new(cursor)
                    .map_err(|e| SpsError::Generic(format!("Failed to open ZIP: {e}")))?;
                for i in 0..archive.len() {
                    let mut file = archive.by_index(i).map_err(|e| {
                        SpsError::Generic(format!("Failed to access ZIP entry: {e}"))
                    })?;
                    let outpath = {
                        let path = file.enclosed_name().ok_or_else(|| {
                            SpsError::Generic("Invalid ZIP entry path".to_string())
                        })?;
                        let mut out = target_dir.clone();
                        for comp in path.components().skip(strip_components) {
                            out.push(comp);
                        }
                        out
                    };
                    if file.name().ends_with('/') {
                        std::fs::create_dir_all(&outpath).map_err(|e| {
                            SpsError::Generic(format!("Failed to create ZIP dir: {e}"))
                        })?;
                    } else {
                        if let Some(p) = outpath.parent() {
                            std::fs::create_dir_all(p).map_err(|e| {
                                SpsError::Generic(format!("Failed to create ZIP parent dir: {e}"))
                            })?;
                        }
                        let mut outfile = std::fs::File::create(&outpath).map_err(|e| {
                            SpsError::Generic(format!("Failed to create ZIP file: {e}"))
                        })?;
                        std::io::copy(&mut file, &mut outfile).map_err(|e| {
                            SpsError::Generic(format!("Failed to write ZIP file: {e}"))
                        })?;
                    }
                }
                Ok(())
            })
            .await
            .map_err(|e| SpsError::Generic(format!("JoinError in ZIP extraction: {e}")))?
        }
        "gz" | "tgz" => {
            let bytes = fs::read(archive_path).await?;
            let target_dir = target_dir.to_path_buf();
            tokio::task::spawn_blocking(move || {
                let decoder = GzDecoder::new(&bytes[..]);
                let mut archive = tar::Archive::new(decoder);
                archive
                    .unpack(&target_dir)
                    .map_err(|e| SpsError::Generic(format!("Failed to unpack GZipped TAR: {e}")))
            })
            .await
            .map_err(|e| SpsError::Generic(format!("JoinError in GZipped TAR extraction: {e}")))?
        }
        "bz2" | "tbz" | "tbz2" => {
            let bytes = fs::read(archive_path).await?;
            let target_dir = target_dir.to_path_buf();
            tokio::task::spawn_blocking(move || {
                let decoder = BzDecoder::new(&bytes[..]);
                let mut archive = tar::Archive::new(decoder);
                archive
                    .unpack(&target_dir)
                    .map_err(|e| SpsError::Generic(format!("Failed to unpack BZipped TAR: {e}")))
            })
            .await
            .map_err(|e| SpsError::Generic(format!("JoinError in BZipped TAR extraction: {e}")))?
        }
        "xz" | "txz" => {
            let bytes = fs::read(archive_path).await?;
            let target_dir = target_dir.to_path_buf();
            tokio::task::spawn_blocking(move || {
                let decoder = XzDecoder::new(&bytes[..]);
                let mut archive = tar::Archive::new(decoder);
                archive
                    .unpack(&target_dir)
                    .map_err(|e| SpsError::Generic(format!("Failed to unpack XZipped TAR: {e}")))
            })
            .await
            .map_err(|e| SpsError::Generic(format!("JoinError in XZipped TAR extraction: {e}")))?
        }
        "tar" => extract_tar_async(reader, target_dir, strip_components, archive_path).await,
        _ => Err(SpsError::Generic(format!(
            "Unsupported archive type for async extraction: '{archive_type}'"
        ))),
    }
}

/// Asynchronously extracts a tar archive stream.
async fn extract_tar_async<R: AsyncRead + Unpin>(
    reader: R,
    target_dir: &Path,
    strip_components: usize,
    archive_path_for_log: &Path,
) -> Result<()> {
    let mut archive = tokio_tar::Archive::new(reader);
    // Consider adding configuration for permissions, etc. if needed
    // archive.set_preserve_permissions(true);

    debug!(
        "Async Starting TAR extraction for {}",
        archive_path_for_log.display()
    );

    let mut entries = archive.entries()?;
    while let Some(entry_result) = entries.next().await {
        let mut entry = entry_result.map_err(|e| {
            SpsError::Generic(format!(
                "Error reading async TAR entry from {}: {}",
                archive_path_for_log.display(),
                e
            ))
        })?;

        // --- Path stripping and validation (same logic as sync version) ---
        let original_path: PathBuf = entry
            .path()
            .map_err(|e| {
                SpsError::Generic(format!(
                    "Invalid path in async TAR entry from {}: {}",
                    archive_path_for_log.display(),
                    e
                ))
            })?
            .into_owned();

        let stripped: Vec<_> = original_path.components().skip(strip_components).collect();
        if stripped.is_empty() {
            continue; // Skip entry
        }
        let mut target_path = target_dir.to_path_buf();
        for comp in stripped {
            match comp {
                Component::Normal(p) => target_path.push(p),
                Component::CurDir => {}
                _ => {
                    // Handle ParentDir, Prefix, RootDir as errors
                    error!(
                        "Disallowed/unsafe component {:?} in TAR path {} within {}",
                        comp,
                        original_path.display(),
                        archive_path_for_log.display()
                    );
                    return Err(SpsError::Generic(format!(
                        "Unsafe path component in {}",
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
            return Err(SpsError::Generic(format!(
                "Path traversal detected in {}",
                archive_path_for_log.display()
            )));
        }
        // --- End Path stripping ---

        // Unpack the entry asynchronously
        if let Err(e) = entry.unpack_in(&target_dir).await {
            // tokio_tar unpack_in handles directory creation implicitly
            // Check if error is AlreadyExists and ignore if so? tokio-tar might not return that
            // specific error kind easily. For now, log other errors.
            error!(
                "Failed to unpack async TAR entry {:?} to {}: {}",
                original_path,
                target_path.display(),
                e
            );
            // Don't return error immediately, try to continue? Or fail fast? Failing fast for now.
            return Err(SpsError::Generic(format!(
                "Failed to unpack async TAR entry {original_path:?}: {e}"
            )));
        } else {
            debug!("Async Unpacked TAR entry to: {}", target_path.display());
            // Set permissions if needed, requires reading entry header data
            #[cfg(unix)]
            {
                if let Ok(mode) = entry.header().mode() {
                    if let Err(e) = crate::fs::set_permissions_async(&target_path, mode).await {
                        warn!(
                            "Failed to set permissions on {}: {}",
                            target_path.display(),
                            e
                        );
                    }
                }
            }
        }
    }

    debug!(
        "Async Finished TAR extraction for {}",
        archive_path_for_log.display()
    );
    Ok(())
}

/// Asynchronously extracts a zip archive stream.
/// Requires reading the whole zip into memory first due to async_zip constraints.
async fn _extract_zip_async<R: Read + Seek>(
    reader: R, // Note: Still takes sync Read + Seek
    target_dir: &Path,
    strip_components: usize,
    archive_path_for_log: &Path,
) -> Result<()> {
    debug!(
        "Async Starting ZIP extraction for {}",
        archive_path_for_log.display()
    );

    // Read the entire ZIP archive into memory as Vec<u8>
    let mut buf_reader = std::io::BufReader::new(reader);
    let mut zip_bytes = Vec::new();
    buf_reader.read_to_end(&mut zip_bytes).map_err(|e| {
        SpsError::Generic(format!(
            "Failed to read ZIP archive into memory for {}: {}",
            archive_path_for_log.display(),
            e
        ))
    })?;
    let archive = ZipFileReader::new(zip_bytes).await.map_err(|e| {
        SpsError::Generic(format!(
            "Failed to open async ZIP {}: {:?}", // Use debug format for zip error
            archive_path_for_log.display(),
            e
        ))
    })?;

    for index in 0..archive.file().entries().len() {
        let entries = archive.file().entries();
        if index >= entries.len() {
            return Err(SpsError::Generic(format!(
                "Failed to access ZIP entry at index {} in {}",
                index,
                archive_path_for_log.display()
            )));
        }
        let entry = &entries[index];
        let filename = match std::str::from_utf8(entry.filename().as_bytes()) {
            Ok(name) => name,
            Err(e) => {
                warn!("Skipping ZIP entry with invalid UTF-8 filename: {}", e);
                continue;
            }
        };

        // --- Path stripping and validation (same logic as sync version) ---
        let original_path = PathBuf::from(filename);
        let stripped: Vec<_> = original_path.components().skip(strip_components).collect();
        if stripped.is_empty() {
            continue; // Skip entry
        }
        let mut target_path = target_dir.to_path_buf();
        for comp in stripped {
            match comp {
                Component::Normal(p) => target_path.push(p),
                Component::CurDir => {}
                _ => {
                    error!(
                        "Disallowed/unsafe component {:?} in ZIP path {} within {}",
                        comp,
                        original_path.display(),
                        archive_path_for_log.display()
                    );
                    return Err(SpsError::Generic(format!(
                        "Unsafe path component in {}",
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
            return Err(SpsError::Generic(format!(
                "Path traversal detected in {}",
                archive_path_for_log.display()
            )));
        }
        // --- End Path stripping ---

        if entry
            .dir()
            .map_err(|e| SpsError::Generic(format!("Failed to check if ZIP entry is dir: {e}")))?
        {
            fs::create_dir_all(&target_path).await?;
        } else {
            // Ensure parent dir exists
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).await?;
            }

            // Read entry content async
            let mut reader = archive.reader_with_entry(index).await.map_err(|e| {
                SpsError::Generic(format!("Failed to get reader for ZIP entry: {e}"))
            })?;
            let mut buffer = Vec::new(); // Read into memory first
            use futures::io::AsyncReadExt as FuturesAsyncReadExt;
            FuturesAsyncReadExt::read_to_end(&mut reader, &mut buffer).await?;

            // Write file async
            let mut outfile = File::create(&target_path).await?;
            outfile.write_all(&buffer).await?;

            // Set permissions if applicable
            #[cfg(unix)]
            {
                if let Some(mode) = entry.unix_permissions() {
                    if let Err(e) =
                        crate::fs::set_permissions_async(&target_path, mode as u32).await
                    {
                        warn!(
                            "Failed set permissions on ZIP entry {}: {}",
                            target_path.display(),
                            e
                        );
                    }
                }
            }
        }
        debug!("Async Extracted ZIP entry to: {}", target_path.display());
    }

    debug!(
        "Async Finished ZIP extraction for {}",
        archive_path_for_log.display()
    );
    Ok(())
}
