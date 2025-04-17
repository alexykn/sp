// sapphire-core/src/build/formula/macho.rs
// Contains Mach-O specific patching logic for bottle relocation.
// Updated to use MachOFatFile32 and MachOFatFile64 for FAT binary parsing.
// Refactored to separate immutable analysis from mutable patching to fix borrow checker errors.

use crate::utils::error::Result; // Keep top-level Result
#[cfg(target_os = "macos")]
use crate::utils::error::SapphireError;
use log::{debug, error, warn};
use std::collections::HashMap;
use std::fs;
use std::io::Write; // Keep for write_patched_buffer
use std::path::Path;
use std::process::Command as StdCommand; // Keep for codesign
use tempfile::NamedTempFile; // Keep for write_patched_buffer

// --- Imports needed for Mach-O patching (macOS only) ---
#[cfg(target_os = "macos")]
use object::{
    self,
    macho::{MachHeader32, MachHeader64}, // Keep for Mach-O parsing
    read::macho::{
        FatArch,
        LoadCommandVariant, // Correct import path
        MachHeader,
        MachOFatFile32,
        MachOFatFile64, // Core Mach-O types + FAT types
        MachOFile,
    },
    read::ReadRef, // Import ReadRef trait
    Endianness,    // Import the Endianness enum
    FileKind,      // For checking FAT/single arch
};

// Constants for Mach-O header sizes
#[cfg(target_os = "macos")]
const MACHO_HEADER32_SIZE: usize = 28;
#[cfg(target_os = "macos")]
const MACHO_HEADER64_SIZE: usize = 32;

// Magic numbers for Mach-O files (little-endian)
#[cfg(target_os = "macos")]
const MH_MAGIC: u32 = 0xfeedface; // 32-bit
#[cfg(target_os = "macos")]
const MH_MAGIC_64: u32 = 0xfeedfacf; // 64-bit

/// Represents a patch to be applied to the buffer.
#[cfg(target_os = "macos")]
struct PatchInfo {
    absolute_offset: usize, // Offset from the start of the *original* buffer
    allocated_len: usize,
    new_path: String,
}

/// Main entry point for patching Mach-O files.
pub fn patch_macho_file(path: &Path, replacements: &HashMap<String, String>) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        patch_macho_file_macos(path, replacements)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // No-op on non-macOS platforms
        let _ = path;
        let _ = replacements;
        Ok(false)
    }
}

#[cfg(target_os = "macos")]
fn patch_macho_file_macos(
    path: &Path,
    replacements: &HashMap<String, String>,
) -> Result<bool> {
    debug!("Processing potential Mach-O file: {}", path.display());

    // 1) Read the entire file into memory
    let mut buffer = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            warn!("  Failed to read {}: {}. Skipping.", path.display(), e);
            return Ok(false);
        }
    };

    // 2) Quick size check: skip anything too small
    if buffer.len() < MACHO_HEADER32_SIZE {
        debug!("  Skipping too‑small file: {}", path.display());
        return Ok(false);
    }

    // 3) Classify via magic‑number (no extensions needed)
    let file_kind = match FileKind::parse(buffer.as_slice()) {
        Ok(kind) => kind,
        Err(_) => {
            debug!("  Not an object file: {}", path.display());
            return Ok(false);
        }
    };

    // 4) Only handle real Mach‑O variants here; bail on archives & others
    match file_kind {
        FileKind::MachO32
        | FileKind::MachO64
        | FileKind::MachOFat32
        | FileKind::MachOFat64 => {
            debug!("  Recognized Mach-O kind {:?}: {}", file_kind, path.display());
        }
        FileKind::Archive => {
            debug!("  Skipping static archive (not Mach‑O): {}", path.display());
            return Ok(false);
        }
        other => {
            debug!("  Not a Mach‑O binary (kind: {:?}), skipping: {}", other, path.display());
            return Ok(false);
        }
    }

    // 5) Phase 1/2: collect all needed patches (immutable)
    let patches = collect_macho_patches(&buffer, file_kind, replacements, path)?;
    if patches.is_empty() {
        debug!("  No patches needed for {}", path.display());
        return Ok(false);
    }

    // 6) Phase 3: apply each patch to the buffer (mutable)
    debug!("  Applying {} patches to {}", patches.len(), path.display());
    for patch in patches {
        patch_path_in_buffer(
            &mut buffer,
            patch.absolute_offset,
            patch.allocated_len,
            &patch.new_path,
            path,
        )?;
    }

    // 7) Write the modified buffer back to disk atomically
    write_patched_buffer(path, &buffer)?;
    debug!("  Wrote patched Mach-O: {}", path.display());

    // 8) Re‑sign on Apple Silicon
    #[cfg(target_arch = "aarch64")]
    {
        resign_binary(path)?;
        debug!("  Re‑signed patched binary: {}", path.display());
    }

    Ok(true)
}


/// ASCII magic for the start of a static `ar` archive  (`!<arch>\n`)
#[cfg(target_os = "macos")]
const AR_MAGIC: &[u8; 8] = b"!<arch>\n";

/// Examine a buffer (Mach‑O or FAT) and return every patch we must apply.
#[cfg(target_os = "macos")]
fn collect_macho_patches<'data>(
    buffer: &'data [u8],
    kind: FileKind,
    replacements: &HashMap<String, String>,
    path_for_log: &Path,
) -> Result<Vec<PatchInfo>> {
    let mut patches = Vec::<PatchInfo>::new();

    match kind {
        /* ---------------------------------------------------------- */
        FileKind::MachO32 => {
            let m = MachOFile::<MachHeader32<Endianness>, _>::parse(buffer)?;
            patches.extend(find_patches_in_commands(
                &m, 0, MACHO_HEADER32_SIZE, replacements, path_for_log,
            )?);
        }
        /* ---------------------------------------------------------- */
        FileKind::MachO64 => {
            let m = MachOFile::<MachHeader64<Endianness>, _>::parse(buffer)?;
            patches.extend(find_patches_in_commands(
                &m, 0, MACHO_HEADER64_SIZE, replacements, path_for_log,
            )?);
        }
        /* ---------------------------------------------------------- */
        FileKind::MachOFat32 => {
            let fat = MachOFatFile32::parse(buffer)?;
            for (idx, arch) in fat.arches().iter().enumerate() {
                let (off, sz) = arch.file_range();
                let slice     = &buffer[off as usize .. (off + sz) as usize];

                /* short‑circuit: static .a archive inside FAT ---------- */
                if slice.starts_with(AR_MAGIC) {
                    debug!("    [slice {}] static archive – skipped", idx);
                    continue;
                }

                /* decide 32 / 64 by magic ------------------------------ */
                if slice.len() >= 4 {
                    let magic = u32::from_le_bytes(slice[0..4].try_into().unwrap());
                    if magic == MH_MAGIC_64 {
                        if let Ok(m) = MachOFile::<MachHeader64<Endianness>, _>::parse(slice) {
                            patches.extend(find_patches_in_commands(
                                &m, off as usize, MACHO_HEADER64_SIZE, replacements, path_for_log,
                            )?);
                        }
                    } else if magic == MH_MAGIC {
                        if let Ok(m) = MachOFile::<MachHeader32<Endianness>, _>::parse(slice) {
                            patches.extend(find_patches_in_commands(
                                &m, off as usize, MACHO_HEADER32_SIZE, replacements, path_for_log,
                            )?);
                        }
                    }
                }
            }
        }
        /* ---------------------------------------------------------- */
        FileKind::MachOFat64 => {
            let fat = MachOFatFile64::parse(buffer)?;
            for (idx, arch) in fat.arches().iter().enumerate() {
                let (off, sz) = arch.file_range();
                let slice     = &buffer[off as usize .. (off + sz) as usize];

                if slice.starts_with(AR_MAGIC) {
                    debug!("    [slice {}] static archive – skipped", idx);
                    continue;
                }

                if slice.len() >= 4 {
                    let magic = u32::from_le_bytes(slice[0..4].try_into().unwrap());
                    if magic == MH_MAGIC_64 {
                        if let Ok(m) = MachOFile::<MachHeader64<Endianness>, _>::parse(slice) {
                            patches.extend(find_patches_in_commands(
                                &m, off as usize, MACHO_HEADER64_SIZE, replacements, path_for_log,
                            )?);
                        }
                    } else if magic == MH_MAGIC {
                        if let Ok(m) = MachOFile::<MachHeader32<Endianness>, _>::parse(slice) {
                            patches.extend(find_patches_in_commands(
                                &m, off as usize, MACHO_HEADER32_SIZE, replacements, path_for_log,
                            )?);
                        }
                    }
                }
            }
        }
        /* ---------------------------------------------------------- */
        _ => { /* archives & unknown kinds are ignored */ }
    }

    Ok(patches)
}


/// Iterates through load commands of a parsed MachOFile (slice) and returns patch details.
/// This function operates immutably on the parsed file data.
#[cfg(target_os = "macos")]
fn find_patches_in_commands<'data, Mach, R>(
    macho_file: &MachOFile<'data, Mach, R>,
    slice_base_offset: usize, // Offset of this slice/file within the original buffer
    header_size: usize,       // Size of the Mach-O header
    replacements: &HashMap<String, String>,
    file_path_for_log: &Path, // For logging context
) -> Result<Vec<PatchInfo>>
where
    Mach: MachHeader,  // Generic over 32/64-bit Mach headers
    R: ReadRef<'data>, // Generic over the reference type used by 'object'
{
    let endian = macho_file.endian();
    let mut patches = Vec::new();
    let mut current_offset = header_size; // Start after the header

    // Get an iterator over the load commands in the Mach-O file/slice
    let mut command_iter = match macho_file.macho_load_commands() {
        Ok(iter) => iter,
        Err(e) => {
            // If we can't even get the iterator, something is wrong with the file structure
            warn!(
                "  Failed to get load command iterator for a slice/file in {}: {}",
                file_path_for_log.display(),
                e
            );
            // Return Ok with empty vec, as no patches can be found.
            return Ok(Vec::new());
        }
    };

    // Iterate through each load command
    while let Some(cmd) = command_iter.next()? {
        let command_offset = current_offset; // Offset of this command within the slice
        let cmd_size = cmd.cmdsize() as usize; // Total size of the command structure in bytes

        // Try to interpret the command variant (e.g., LC_LOAD_DYLIB, LC_RPATH)
        let cmd_variant = match cmd.variant() {
            Ok(v) => v,
            Err(e) => {
                // Log if a specific command is malformed
                warn!(
                    "    Error getting command variant in {}: {}",
                    file_path_for_log.display(),
                    e
                );
                current_offset += cmd_size; // Skip to next command
                continue;
            }
        };

        // Check if the command variant contains a path we might need to patch
        let path_info_opt: Option<(u32, &[u8])> = match cmd_variant {
            // LC_ID_DYLIB, LC_LOAD_DYLIB
            LoadCommandVariant::Dylib(dylib_command)
            | LoadCommandVariant::IdDylib(dylib_command) => {
                // Get the offset of the path string relative *to the start of the command struct*
                let path_offset_in_cmd_struct = dylib_command.dylib.name.offset.get(endian);
                // Try to read the null-terminated string at that offset
                cmd.string(endian, dylib_command.dylib.name)
                    .ok() // Convert Result<&[u8]> to Option<&[u8]>
                    .map(|bytes| (path_offset_in_cmd_struct, bytes)) // Pair offset with bytes if successful
            }
            // LC_RPATH
            LoadCommandVariant::Rpath(rpath_command) => {
                // Get the offset of the path string relative *to the start of the command struct*
                let path_offset_in_cmd_struct = rpath_command.path.offset.get(endian);
                // Try to read the null-terminated string
                cmd.string(endian, rpath_command.path)
                    .ok()
                    .map(|bytes| (path_offset_in_cmd_struct, bytes))
            }
            // Other command types are ignored for path patching
            _ => None,
        };

        if let Some((path_offset_in_cmd_struct, path_bytes)) = path_info_opt {
            match std::str::from_utf8(path_bytes) {
                Ok(current_path_str) => {
                    if let Some(new_path_str) =
                        find_and_replace_placeholders(current_path_str, replacements)
                    {
                        // Calculate the total space allocated for the path string within the command structure
                        let allocated_len =
                            cmd_size.saturating_sub(path_offset_in_cmd_struct as usize);

                        if allocated_len == 0 {
                            warn!(
                                "  Calculated zero allocated length for path in command {:?} for {}",
                                cmd.cmd(), file_path_for_log.display()
                            );
                            current_offset += cmd_size;
                            continue;
                        }

                        // Check if the new path (plus null terminator) fits
                        if new_path_str.as_bytes().len() >= allocated_len {
                            error!(
                                "New path '{}' ({} bytes + null) is too long for allocated space ({} bytes) in command {:?} in {}",
                                new_path_str, new_path_str.as_bytes().len(), allocated_len, cmd.cmd(), file_path_for_log.display()
                            );
                            return Err(SapphireError::PathTooLongError(format!(
                                "Relocation failed for {}: new path '{}' too long for binary structure (max {} bytes)",
                                file_path_for_log.display(),
                                new_path_str,
                                allocated_len.saturating_sub(1)
                            )));
                        }

                        // Calculate the absolute offset in the original buffer
                        let absolute_patch_offset =
                            slice_base_offset + command_offset + path_offset_in_cmd_struct as usize;
                        debug!(
                            "  Planning patch: Replace '{}' with '{}' at absolute offset {} (allocated len {})",
                            current_path_str, new_path_str, absolute_patch_offset, allocated_len
                        );

                        patches.push(PatchInfo {
                            absolute_offset: absolute_patch_offset,
                            allocated_len,
                            new_path: new_path_str,
                        });
                    }
                }
                Err(_) => {
                    warn!(
                        "  Path bytes are not valid UTF-8 for command {:?} in {}. Skipping patch.",
                        cmd.cmd(),
                        file_path_for_log.display()
                    );
                }
            }
        }
        current_offset += cmd_size; // Move to the next command
    }

    Ok(patches)
}

/// Helper to replace placeholders in a string based on the replacements map.
/// Returns `Some(String)` with replacements if any were made, `None` otherwise.
fn find_and_replace_placeholders(
    current_path: &str,
    replacements: &HashMap<String, String>,
) -> Option<String> {
    let mut new_path = current_path.to_string();
    let mut path_modified = false;
    // Iterate through all placeholder/replacement pairs
    for (placeholder, replacement) in replacements {
        // Check if the current path string contains the placeholder
        if new_path.contains(placeholder) {
            // Replace all occurrences of the placeholder
            new_path = new_path.replace(placeholder, replacement);
            path_modified = true; // Mark that a change was made
            debug!(
                "   Replaced '{}' with '{}' -> '{}'",
                placeholder, replacement, new_path
            );
        }
    }
    // Return the modified string only if changes occurred
    if path_modified {
        Some(new_path)
    } else {
        None
    }
}

/// Writes the new path (null-padded) into the *original buffer* at the specified *absolute* offset.
/// This function performs the mutation.
#[cfg(target_os = "macos")]
fn patch_path_in_buffer(
    buffer: &mut [u8],        // The full mutable buffer of the file
    absolute_offset: usize,   // Absolute offset within the buffer where the path string starts
    allocated_len: usize,     // The total space reserved for the path string (including null)
    new_path_str: &str,       // The new path string (without null terminator)
    file_path_for_log: &Path, // For logging context
) -> Result<()> {
    let new_path_bytes = new_path_str.as_bytes();

    // This length check is crucial and should have been done before calling,
    // but it serves as a final safeguard here. The length must be strictly less
    // than allocated_len to allow space for the null terminator.
    if new_path_bytes.len() >= allocated_len {
        error!(
            "Internal Error: New path '{}' ({} bytes) is too long for allocated space ({} bytes) at absolute offset {} in {}",
            new_path_str, new_path_bytes.len(), allocated_len, absolute_offset, file_path_for_log.display()
        );
        // Indicate internal error because the check should have happened earlier
        return Err(SapphireError::PathTooLongError(format!(
             "Internal Error during patching: Relocation failed for {}: new path '{}' too long (max {} bytes)",
            file_path_for_log.display(), new_path_str, allocated_len.saturating_sub(1)
        )));
    }

    // Create a buffer of the exact allocated length, filled with null bytes
    let mut padded_bytes = vec![0u8; allocated_len];
    // Copy the new path bytes into the beginning of the padded buffer
    padded_bytes[..new_path_bytes.len()].copy_from_slice(new_path_bytes);
    // The rest of padded_bytes remains null bytes, ensuring null termination

    // Check that the target slice [absolute_offset..absolute_offset + allocated_len]
    // is within the bounds of the main buffer.
    if absolute_offset
        .checked_add(allocated_len)
        .map_or(true, |end| end > buffer.len())
    {
        error!(
             "Internal relocation error: Calculated patch range ({}+{}) exceeds buffer size ({}) for {}",
             absolute_offset, allocated_len, buffer.len(), file_path_for_log.display()
        );
        return Err(SapphireError::MachOModificationError(format!(
            "Internal relocation error: invalid patch offset/length for {}",
            file_path_for_log.display()
        )));
    }

    // Perform the patch: copy the null-padded new path into the target location in the main buffer
    buffer[absolute_offset..absolute_offset + allocated_len].copy_from_slice(&padded_bytes);

    // Log the successful patch operation
    // Log statement adjusted slightly for clarity in previous refactoring, keeping it:
    debug!(
        "    Patched Mach-O path at absolute offset {} in {}",
        absolute_offset,
        file_path_for_log.display()
    );
    Ok(())
}

/// Writes the patched buffer to the original path atomically using a temporary file.
#[cfg(target_os = "macos")]
fn write_patched_buffer(original_path: &Path, buffer: &[u8]) -> Result<()> {
    // Get the directory containing the original file
    let dir = original_path.parent().ok_or_else(|| {
        SapphireError::Generic(format!(
            "Cannot get parent directory for {}",
            original_path.display()
        ))
    })?;
    // Ensure the directory exists
    fs::create_dir_all(dir).map_err(|e| SapphireError::Io(e))?;

    // Create a named temporary file in the same directory to facilitate atomic rename
    let mut temp_file = NamedTempFile::new_in(dir)?;
    debug!(
        "    Writing patched buffer ({} bytes) to temporary file: {:?}",
        buffer.len(),
        temp_file.path()
    );
    // Write the entire modified buffer to the temporary file
    temp_file.write_all(buffer)?;
    // Ensure data is flushed to the OS buffer
    temp_file.flush()?;
    // Attempt to sync data to the disk
    temp_file.as_file().sync_all()?; // Ensure data is physically written

    // Atomically replace the original file with the temporary file
    // persist() renames the temp file over the original path.
    temp_file.persist(original_path).map_err(|e| {
        // If persist fails, the temporary file might still exist.
        // The error 'e' contains both the temp file and the underlying IO error.
        error!(
            "    Failed to persist/rename temporary file over {}: {}",
            original_path.display(),
            e.error // Log the underlying IO error
        );
        // Return the IO error wrapped in our error type
        SapphireError::Io(e.error)
    })?;
    debug!(
        "    Atomically replaced {} with patched version",
        original_path.display()
    );
    Ok(())
}

/// Re-signs the binary using the `codesign` command-line tool.
/// This is typically necessary on Apple Silicon (aarch64) after modifying executables.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn resign_binary(path: &Path) -> Result<()> {
    debug!("    Re-signing patched binary: {}", path.display());
    // Execute `codesign -s - --force --preserve-metadata=identifier,entitlements <path>`
    // -s - : Use ad-hoc signing (no specific identity needed)
    // --force : Overwrite existing signature
    // --preserve-metadata=... : Keep existing identifier and entitlements if possible
    let status = StdCommand::new("codesign")
        .args([
            "-s",
            "-",
            "--force",
            "--preserve-metadata=identifier,entitlements",
        ])
        .arg(path)
        .status() // Execute the command and get its exit status
        .map_err(|e| {
            // Handle errors in *executing* the command (e.g., codesign not found)
            error!(
                "    Failed to execute codesign command for {}: {}",
                path.display(),
                e
            );
            SapphireError::Io(e) // Wrap the OS error
        })?;

    // Check if the command executed successfully (exit code 0)
    if status.success() {
        debug!("    Successfully re-signed {}", path.display());
        Ok(())
    } else {
        // Log if codesign command returned a non-zero exit status
        error!(
            "    codesign command failed for {} with status: {}",
            path.display(),
            status
        );
        // Return a specific error indicating codesign failure
        Err(SapphireError::CodesignError(format!(
            "Failed to re-sign patched binary {}, it may not be executable. Exit status: {}",
            path.display(),
            status
        )))
    }
}

// No-op stub for resigning on non-Apple Silicon macOS (e.g., x86_64)
#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
fn resign_binary(_path: &Path) -> Result<()> {
    // No re-signing typically needed on Intel Macs after ad-hoc patching
    Ok(())
}

// No-op stub for resigning Innovations on non-macOS platforms
#[cfg(not(target_os = "macos"))]
fn resign_binary(_path: &Path) -> Result<()> {
    // Resigning is a macOS concept
    Ok(())
}
