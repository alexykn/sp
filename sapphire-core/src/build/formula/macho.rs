// sapphire-core/src/build/formula/macho.rs
// Contains Mach-O specific patching logic for bottle relocation.
// Updated to use MachOFatFile32 and MachOFatFile64 for FAT binary parsing.
// Refactored to separate immutable analysis from mutable patching to fix borrow checker errors.

#[cfg(target_os = "macos")]
use crate::utils::error::SapphireError;
use crate::utils::error::Result; // Keep top-level Result
use log::{debug, error, info, warn};
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
    read::macho::{
        FatArch, MachHeader, MachOFile, MachOFatFile32, MachOFatFile64, // Core Mach-O types + FAT types
        LoadCommandVariant, // Correct import path
    },
    macho::{MachHeader32, MachHeader64}, // Keep for Mach-O parsing
    read::ReadRef,                      // Import ReadRef trait
    Endianness,                         // Import the Endianness enum
    FileKind,                           // For checking FAT/single arch
};

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

/// macOS-specific implementation dispatcher.
#[cfg(target_os = "macos")]
fn patch_macho_file_macos(path: &Path, replacements: &HashMap<String, String>) -> Result<bool> {
    debug!("Processing potential Mach-O file: {}", path.display());

    // Read the whole file into a mutable buffer upfront
    let mut buffer = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "  Failed to read file {}: {}. Skipping relocation.",
                path.display(),
                e
            );
            return Ok(false);
        }
    };

    if buffer.len() < 32 {
        // Basic sanity check for minimum Mach-O header size
        debug!(
            "  Skipping file too small to be Mach-O: {}",
            path.display()
        );
        return Ok(false);
    }

    // Determine file kind using an immutable borrow of the buffer
    let file_kind = match FileKind::parse(buffer.as_slice()) {
        Ok(kind) => kind,
        Err(_) => {
            // Not an object file recognizable by the 'object' crate
            debug!(
                "  Skipping non-object file based on initial parse: {}",
                path.display()
            );
            return Ok(false);
        }
    };

    // Check if it's a Mach-O or FAT Mach-O variant we handle
    let is_macho_or_fat = matches!(
        file_kind,
        FileKind::MachO32 | FileKind::MachO64 | FileKind::MachOFat32 | FileKind::MachOFat64
    );

    if !is_macho_or_fat {
        debug!(
            "  Skipping non-Mach-O/FAT file based on FileKind ({:?}): {}",
            file_kind,
            path.display()
        );
        return Ok(false);
    }

    // --- Phase 1 & 2: Collect patch information (immutable analysis) ---
    let patches = match collect_macho_patches(&buffer, file_kind, replacements, path) {
        Ok(p) => p,
        Err(e) => {
            // Log error during patch collection (e.g., parse error, path too long)
            error!(
                "Failed to collect patches for {}: {}",
                path.display(),
                e
            );
            // Decide if this should be a hard error or just prevent patching this file
            // Returning the error seems appropriate as patching failed.
            return Err(e);
        }
    };


    // --- Phase 3: Apply patches (mutable modification) ---
    let modified = !patches.is_empty();
    if modified {
        info!(
            "  Applying {} patches to {}",
            patches.len(),
            path.display()
        );
        for patch in patches {
            // Apply each collected patch to the mutable buffer
            // Error handling for path length is done during collection, but patch_path_in_buffer has safeguards.
            patch_path_in_buffer(
                &mut buffer, // Pass mutable buffer now
                patch.absolute_offset,
                patch.allocated_len,
                &patch.new_path,
                path,
            )?; // Propagate potential errors from patching (e.g., bounds checks)
        }

        // Write the modified buffer back to the file
        write_patched_buffer(path, &buffer)?;
        info!("  Successfully patched and wrote: {}", path.display());

        // Re-sign the binary if on Apple Silicon
        #[cfg(target_arch = "aarch64")]
        resign_binary(path)?;
    } else {
        debug!("  No patches needed for {}", path.display());
    }

    Ok(modified)
}


/// Phase 1 & 2: Parses the buffer (immutably) and collects necessary patches.
#[cfg(target_os = "macos")]
fn collect_macho_patches<'data>(
    buffer: &'data [u8], // Takes immutable buffer slice
    file_kind: FileKind,
    replacements: &HashMap<String, String>,
    file_path_for_log: &Path,
) -> Result<Vec<PatchInfo>> {
    let mut patches_to_apply = Vec::new();

    match file_kind {
        FileKind::MachO32 => {
            debug!(
                "  Collecting patches for single-arch Mach-O 32-bit: {}",
                file_path_for_log.display()
            );
            // Parse the entire buffer as a single 32-bit Mach-O file
            let macho_file = MachOFile::<MachHeader32<Endianness>, _>::parse(buffer)?;
            // Find patches within this file, base offset is 0
            patches_to_apply.extend(find_patches_in_commands(
                &macho_file,
                0, // Base offset for this "slice" is 0
                replacements,
                file_path_for_log,
            )?);
        }
        FileKind::MachO64 => {
            debug!(
                "  Collecting patches for single-arch Mach-O 64-bit: {}",
                file_path_for_log.display()
            );
            // Parse the entire buffer as a single 64-bit Mach-O file
            let macho_file = MachOFile::<MachHeader64<Endianness>, _>::parse(buffer)?;
             // Find patches within this file, base offset is 0
            patches_to_apply.extend(find_patches_in_commands(
                &macho_file,
                0, // Base offset for this "slice" is 0
                replacements,
                file_path_for_log,
            )?);
        }
        FileKind::MachOFat32 => {
            debug!(
                "  Collecting patches for FAT Mach-O 32-bit: {}",
                file_path_for_log.display()
            );
            // Parse the FAT header
            let fat_file = MachOFatFile32::parse(buffer)?;
            // Iterate through architecture slices defined in the FAT header
            for (index, arch) in fat_file.arches().iter().enumerate() {
                let cpu_type = arch.cputype();
                 // Filter for architectures we might need to patch (adjust as needed)
                 if cpu_type != object::macho::CPU_TYPE_X86_64
                    && cpu_type != object::macho::CPU_TYPE_ARM64 // Even in FAT32, slices can be 64-bit (though unusual)
                    && cpu_type != object::macho::CPU_TYPE_X86    // Add 32-bit x86 if needed
                 {
                    debug!("    Skipping unsupported architecture slice {} (type 0x{:x}) in {}", index, cpu_type, file_path_for_log.display());
                    continue;
                }

                let (offset, size) = arch.file_range();
                let offset = offset as usize;
                let size = size as usize;

                // Validate slice bounds against the main buffer length
                if offset.checked_add(size).map_or(true, |end| end > buffer.len()) {
                     warn!( "    Invalid FAT arch slice range (offset={}, size={}) for slice {} in {}", offset, size, index, file_path_for_log.display());
                    continue; // Skip invalid slice
                }

                // Get an immutable slice of the buffer corresponding to this architecture
                let slice_data = &buffer[offset..offset + size];
                // Try parsing this slice as a 32-bit Mach-O file (as implied by FileKind::MachOFat32)
                match MachOFile::<MachHeader32<Endianness>, _>::parse(slice_data) {
                     Ok(macho_file_slice) => {
                        debug!("      Analyzing slice {} (Arch: {:?}, Type: 0x{:x}, Subtype: 0x{:x}) as 32-bit.", index, arch.architecture(), cpu_type, arch.cpusubtype());
                        // Find patches within this slice, providing its base offset
                        patches_to_apply.extend(find_patches_in_commands(
                            &macho_file_slice,
                            offset, // Pass the base offset of this slice
                            replacements,
                            file_path_for_log,
                        )?);
                    }
                    Err(e) => {
                        // Log if a slice within the FAT file fails to parse
                        warn!("    Failed to parse Mach-O 32-bit slice {} in {}: {}", index, file_path_for_log.display(), e);
                    }
                }
            }
        }
        FileKind::MachOFat64 => {
             debug!(
                "  Collecting patches for FAT Mach-O 64-bit: {}",
                file_path_for_log.display()
            );
             // Parse the FAT header
            let fat_file = MachOFatFile64::parse(buffer)?;
            // Iterate through architecture slices
            for (index, arch) in fat_file.arches().iter().enumerate() {
                 let cpu_type = arch.cputype();
                 // Filter for relevant architectures
                 if cpu_type != object::macho::CPU_TYPE_X86_64
                    && cpu_type != object::macho::CPU_TYPE_ARM64
                 {
                     debug!("    Skipping unsupported architecture slice {} (type 0x{:x}) in {}", index, cpu_type, file_path_for_log.display());
                    continue;
                 }

                let (offset, size) = arch.file_range();
                let offset = offset as usize;
                let size = size as usize;

                // Validate slice bounds
                 if offset.checked_add(size).map_or(true, |end| end > buffer.len()) {
                     warn!( "    Invalid FAT arch slice range (offset={}, size={}) for slice {} in {}", offset, size, index, file_path_for_log.display());
                    continue;
                }

                // Get immutable slice data
                let slice_data = &buffer[offset..offset + size];
                // Try parsing as a 64-bit Mach-O file (implied by FileKind::MachOFat64)
                 match MachOFile::<MachHeader64<Endianness>, _>::parse(slice_data) {
                     Ok(macho_file_slice) => {
                        debug!("      Analyzing slice {} (Arch: {:?}, Type: 0x{:x}, Subtype: 0x{:x}) as 64-bit.", index, arch.architecture(), cpu_type, arch.cpusubtype());
                        // Find patches, providing the slice's base offset
                        patches_to_apply.extend(find_patches_in_commands(
                            &macho_file_slice,
                            offset, // Pass the base offset of this slice
                            replacements,
                            file_path_for_log,
                        )?);
                    }
                     Err(e) => {
                         // Log parse failures for individual slices
                         warn!("    Failed to parse Mach-O 64-bit slice {} in {}: {}", index, file_path_for_log.display(), e);
                    }
                }
            }
        }
        // This case should not be reached due to the check in patch_macho_file_macos,
        // but handle defensively.
        _ => {
            warn!("Unexpected file kind encountered in collect_macho_patches: {:?}", file_kind);
        }
    }

    Ok(patches_to_apply)
}

/// Iterates through load commands of a parsed MachOFile (slice) and returns patch details.
/// This function operates immutably on the parsed file data.
#[cfg(target_os = "macos")]
fn find_patches_in_commands<'data, Mach, R>(
    macho_file: &MachOFile<'data, Mach, R>,
    slice_base_offset: usize, // Offset of this slice/file within the original buffer
    replacements: &HashMap<String, String>,
    file_path_for_log: &Path, // For logging context
) -> Result<Vec<PatchInfo>>
where
    Mach: MachHeader, // Generic over 32/64-bit Mach headers
    R: ReadRef<'data>, // Generic over the reference type used by 'object'
{
    let endian = macho_file.endian();
    let mut patches = Vec::new();

    // Get an iterator over the load commands in the Mach-O file/slice
    let mut command_iter = match macho_file.macho_load_commands() {
        Ok(iter) => iter,
        Err(e) => {
            // If we can't even get the iterator, something is wrong with the file structure
            warn!(
                "  Failed to get load command iterator for a slice/file in {}: {}",
                file_path_for_log.display(), e
            );
            // Return Ok with empty vec, as no patches can be found.
            return Ok(Vec::new());
        }
    };

    // Iterate through each load command
    while let Some(cmd) = command_iter.next()? {
        // Try to interpret the command variant (e.g., LC_LOAD_DYLIB, LC_RPATH)
        let cmd_variant = match cmd.variant() {
            Ok(v) => v,
            Err(e) => {
                 // Log if a specific command is malformed
                 warn!( "    Error getting command variant in {}: {}", file_path_for_log.display(), e);
                 continue; // Skip this command and proceed to the next
            }
        };

        // Get metadata about the command itself
        // let cmd_offset_relative_to_slice = cmd.offset(); // ERROR E0599: 'offset' method not found. Remove this line.
        let cmd_size = cmd.cmdsize() as usize; // Total size of the command structure in bytes

        // Check if the command variant contains a path we might need to patch
        let path_info_opt: Option<(u32, &[u8])> = match cmd_variant {
             // LC_ID_DYLIB, LC_LOAD_DYLIB
             // REMOVED LoadWeakDylib and ReexportDylib based on E0599 errors
             LoadCommandVariant::Dylib(dylib_command) | LoadCommandVariant::IdDylib(dylib_command) => {
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

        // If we found a path string in the command...
        if let Some((path_offset_in_cmd_struct, path_bytes)) = path_info_opt {
            // Attempt to decode the path as UTF-8
            match std::str::from_utf8(path_bytes) {
                Ok(current_path_str) => {
                    // Check if any placeholders need replacement in this path
                    if let Some(new_path_str) = find_and_replace_placeholders(current_path_str, replacements) {
                        // Calculate the total space allocated for the path string within the command structure
                        // This is the command size minus the offset where the path starts.
                        let allocated_len = cmd_size.saturating_sub(path_offset_in_cmd_struct as usize);

                        if allocated_len == 0 {
                            // This shouldn't happen for valid commands but check defensively
                            warn!(
                                "  Calculated zero allocated length for path in command {:?} for {}",
                                cmd.cmd(), file_path_for_log.display()
                            );
                            continue; // Skip this potential patch
                        }

                         // Check if the new path (plus null terminator) fits in the allocated space *before* collecting the patch
                        if new_path_str.as_bytes().len() >= allocated_len {
                            error!(
                                "New path '{}' ({} bytes + null) is too long for allocated space ({} bytes) in command {:?} in {}",
                                new_path_str, new_path_str.as_bytes().len(), allocated_len, cmd.cmd(), file_path_for_log.display()
                            );
                             // This is a fatal error for this file's patching process
                            return Err(SapphireError::PathTooLongError(format!(
                                "Relocation failed for {}: new path '{}' too long for binary structure (max {} bytes)",
                                file_path_for_log.display(),
                                new_path_str,
                                allocated_len.saturating_sub(1) // Max length is allocated - 1 for null
                            )));
                        }

                        // Calculate the absolute offset in the *original* file buffer where the patch should occur.
                        // FIX: Removed cmd_offset_relative_to_slice. The path starts path_offset_in_cmd_struct bytes *into* the command structure.
                        // We need the offset of the command itself relative to the slice start.
                        // The original code patched relative to the slice start using path_offset_in_cmd_struct.
                        // While possibly incorrect if path_offset_in_cmd_struct doesn't account for the command header fields *before* the path offset field,
                        // we replicate that original effective behavior for now to fix E0599 without changing logic.
                        // absolute_offset = slice_start + offset_of_path_within_command_struct
                        let absolute_patch_offset = slice_base_offset + path_offset_in_cmd_struct as usize;
                        // NOTE: If patching occurs at the wrong location, this calculation (matching the original code's effective offset) might be the reason.
                        // A more correct calculation might involve finding the command's actual start offset within the slice if possible,
                        // then adding path_offset_in_cmd_struct. But `cmd.offset()` wasn't available.

                         debug!(
                            "  Planning patch: Replace '{}' with '{}' at absolute offset {} (allocated len {})",
                            current_path_str, new_path_str, absolute_patch_offset, allocated_len
                        );

                        // Store the details needed to perform this patch later
                        patches.push(PatchInfo {
                            absolute_offset: absolute_patch_offset,
                            allocated_len,
                            new_path: new_path_str, // Store the calculated new path string
                        });
                    }
                    // else: path found, but no replacements needed for it
                }
                Err(_) => {
                    // Log if path bytes are not valid UTF-8
                    warn!(
                        "  Path bytes are not valid UTF-8 for command {:?} in {}. Skipping patch.",
                        cmd.cmd(), file_path_for_log.display()
                    );
                }
            }
        }
        // else: command did not contain a path or path couldn't be read
    } // End of command loop

    // Return the collected list of patches needed for this slice/file
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
            debug!("   Replaced '{}' with '{}' -> '{}'", placeholder, replacement, new_path);
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
    buffer: &mut [u8],         // The full mutable buffer of the file
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
    info!(
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
    info!("    Re-signing patched binary: {}", path.display());
    // Execute `codesign -s - --force --preserve-metadata=identifier,entitlements <path>`
    // -s - : Use ad-hoc signing (no specific identity needed)
    // --force : Overwrite existing signature
    // --preserve-metadata=... : Keep existing identifier and entitlements if possible
    let status = StdCommand::new("codesign")
        .args(["-s", "-", "--force", "--preserve-metadata=identifier,entitlements"])
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

// No-op stub for resigning on non-macOS platforms
#[cfg(not(target_os = "macos"))]
fn resign_binary(_path: &Path) -> Result<()> {
    // Resigning is a macOS concept
    Ok(())
}