// sapphire-core/src/build/formula/macho.rs
// Contains Mach-O specific patching logic for bottle relocation.
// Corrected MachHeader bounds, FatHeader usage, and other errors.

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
        FatArch, MachHeader,
        MachOFile, // Core Mach-O types
        LoadCommandVariant, // Correct import path
    },
    macho::{MachHeader32, MachHeader64},
    read::{Object, ReadRef}, // Import ObjectHeader trait
    Endianness,   // Import the Endianness enum
    FileKind,     // For checking FAT/single arch
};


/// Main entry point for patching Mach-O files.
pub fn patch_macho_file(path: &Path, replacements: &HashMap<String, String>) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        patch_macho_file_macos(path, replacements)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        let _ = replacements;
        Ok(false)
    }
}

/// macOS-specific implementation dispatcher.
#[cfg(target_os = "macos")]
fn patch_macho_file_macos(path: &Path, replacements: &HashMap<String, String>) -> Result<bool> {
    debug!("Processing potential Mach-O file: {}", path.display());

    let buffer = match fs::read(path) {
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
        debug!(
            "  Skipping file too small to be Mach-O: {}",
            path.display()
        );
        return Ok(false);
    }

    let file_kind = match FileKind::parse(buffer.as_slice()) {
        Ok(kind) => kind,
        Err(_) => {
            debug!(
                "  Skipping non-object file based on initial parse: {}",
                path.display()
            );
            return Ok(false);
        }
    };

    let is_macho_or_fat = matches!(
        file_kind,
        FileKind::MachO32
            | FileKind::MachO64
            | FileKind::MachOFat32
            | FileKind::MachOFat64
    );

    if !is_macho_or_fat {
        debug!(
            "  Skipping non-Mach-O/FAT file based on FileKind: {}",
            path.display()
        );
        return Ok(false);
    }

    let mut mut_buffer = buffer;
    let mut modified = false;

    // --- Call the variant processing function ---
    // This function now needs to handle the dispatch based on FileKind internally
    // because MachOFile::parse requires knowing the header type beforehand.
    match parse_and_patch_macho_variants(&mut mut_buffer, replacements, path) {
        Ok(was_modified) => modified = was_modified,
        Err(e) => {
            error!("  Error processing Mach-O file {}: {}", path.display(), e);
            return Err(e);
        }
    }
    // --- End variant processing call ---

    if modified {
        write_patched_buffer(path, &mut_buffer)?;
        info!("  Successfully patched and wrote: {}", path.display());
        #[cfg(target_arch = "aarch64")]
        resign_binary(path)?;
    }

    Ok(modified)
}


/// Handles both single-architecture and FAT binaries by dispatching to specific handlers.
#[cfg(target_os = "macos")]
fn parse_and_patch_macho_variants<'data>(
    buffer: &'data mut Vec<u8>,
    replacements: &HashMap<String, String>,
    file_path_for_log: &Path,
) -> Result<bool> {
    // Use fully qualified paths
    use object::read::macho::{FatArch, FatHeader, MachOFile}; // Add FatArch trait
    use object::read::Bytes; // Need Bytes for FatHeader::parseXX

    let file_kind = FileKind::parse(buffer.as_slice())?;

    match file_kind {
        FileKind::MachO32 => {
            debug!(
                "  Processing as single-arch Mach-O 32-bit: {}",
                file_path_for_log.display()
            );
            // Parse specifically as MachO32
            let macho_file = MachOFile::<MachHeader32<Endianness>, _>::parse(buffer.as_slice())?;
             process_macho_commands::<MachHeader32<Endianness>, _>(
                &macho_file,
                buffer, // Pass the whole mutable buffer
                replacements,
                file_path_for_log,
            )
        }
         FileKind::MachO64 => {
            debug!(
                "  Processing as single-arch Mach-O 64-bit: {}",
                file_path_for_log.display()
            );
             // Parse specifically as MachO64
            let macho_file = MachOFile::<MachHeader64<Endianness>, _>::parse(buffer.as_slice())?;
             process_macho_commands::<MachHeader64<Endianness>, _>(
                &macho_file,
                buffer, // Pass the whole mutable buffer
                replacements,
                file_path_for_log,
            )
        }
        FileKind::MachOFat32 => {
            debug!(
                "  Processing as FAT Mach-O 32-bit: {}",
                file_path_for_log.display()
            );
            let mut modified_any = false;
            let (_fat_header, arches) = FatHeader::parse32(Bytes(buffer.as_slice()))?;
            
            for (index, arch) in arches.iter().enumerate() {
                // Use FatArch trait methods
                let endian = arch.endian()?;
                let cpu_type = arch.cputype();
                let cpu_subtype = arch.cpusubtype();
                 debug!(
                    "    Slice {}: Arch={:?}, Type=0x{:x}, Subtype=0x{:x}",
                    index,
                    arch.architecture()?, // architecture() returns Result
                    cpu_type,
                    cpu_subtype
                );

                if cpu_type != object::macho::CPU_TYPE_X86_64
                    && cpu_type != object::macho::CPU_TYPE_ARM64
                {
                    debug!(
                        "    Skipping unsupported architecture slice {} in FAT binary: {}",
                        index,
                        file_path_for_log.display()
                    );
                    continue;
                }

                let (offset, size) = arch.file_range();
                let offset = offset as usize;
                let size = size as usize;

                if offset
                    .checked_add(size)
                    .map_or(true, |end| end > buffer.len())
                {
                    warn!(
                        "    Invalid FAT arch slice range (offset={}, size={}) exceeds buffer length ({}) for slice {} in {}",
                        offset, size, buffer.len(), index, file_path_for_log.display()
                    );
                    continue;
                }

                let slice_parse_buffer: &[u8] = &buffer[offset..offset + size];
                let slice_buffer_mut: &mut [u8] = &mut buffer[offset..offset + size];

                // Slice must be MachO32 if parent is MachOFat32
                let macho_file_slice = MachOFile::<MachHeader32<Endianness>, _>::parse(slice_parse_buffer)?;
                debug!("      Processing slice {} as 32-bit.", index);
                let modified_slice = process_macho_commands::<MachHeader32<Endianness>, _>(
                    &macho_file_slice,
                    slice_buffer_mut,
                    replacements,
                    file_path_for_log,
                )?;

                if modified_slice {
                    modified_any = true;
                }
            }
            Ok(modified_any)
        }
        FileKind::MachOFat64 => {
             debug!(
                "  Processing as FAT Mach-O 64-bit: {}",
                file_path_for_log.display()
            );
            let mut modified_any = false;
            let (_fat_header, arches) = FatHeader::parse64(Bytes(buffer.as_slice()))?;

            for (index, arch) in arches.iter().enumerate() {
                 // Use FatArch trait methods
                let endian = arch.endian()?;
                let cpu_type = arch.cputype();
                let cpu_subtype = arch.cpusubtype();
                 debug!(
                    "    Slice {}: Arch={:?}, Type=0x{:x}, Subtype=0x{:x}",
                    index,
                    arch.architecture()?, // architecture() returns Result
                    cpu_type,
                    cpu_subtype
                );

                if cpu_type != object::macho::CPU_TYPE_X86_64
                    && cpu_type != object::macho::CPU_TYPE_ARM64
                {
                    debug!(
                        "    Skipping unsupported architecture slice {} in FAT binary: {}",
                        index,
                        file_path_for_log.display()
                    );
                    continue;
                }

                let (offset, size) = arch.file_range();
                let offset = offset as usize;
                let size = size as usize;

                if offset
                    .checked_add(size)
                    .map_or(true, |end| end > buffer.len())
                {
                    warn!(
                        "    Invalid FAT arch slice range (offset={}, size={}) exceeds buffer length ({}) for slice {} in {}",
                        offset, size, buffer.len(), index, file_path_for_log.display()
                    );
                    continue;
                }

                let slice_parse_buffer: &[u8] = &buffer[offset..offset + size];
                let slice_buffer_mut: &mut [u8] = &mut buffer[offset..offset + size];

                 // Slice must be MachO64 if parent is MachOFat64
                let macho_file_slice = MachOFile::<MachHeader64<Endianness>, _>::parse(slice_parse_buffer)?;
                debug!("      Processing slice {} as 64-bit.", index);
                let modified_slice = process_macho_commands::<MachHeader64<Endianness>, _>(
                    &macho_file_slice,
                    slice_buffer_mut,
                    replacements,
                    file_path_for_log,
                )?;

                if modified_slice {
                    modified_any = true;
                }
            }
            Ok(modified_any)
        }
        _ => {
            debug!(
                "  File is not Mach-O or FAT Mach-O: {}",
                file_path_for_log.display()
            );
            Ok(false)
        }
    }
}

/// Iterates through load commands and patches relevant paths.
/// Now generic over Mach: MachHeader and R: ReadRef
#[cfg(target_os = "macos")]
fn process_macho_commands<'data, Mach, R>(
    macho_file: &MachOFile<'data, Mach, R>, // Use generic Mach and R
    buffer: &mut [u8], // The mutable slice
    replacements: &HashMap<String, String>,
    file_path_for_log: &Path,
) -> Result<bool>
where
    Mach: MachHeader, // Add the MachHeader bound
    R: ReadRef<'data>, // Add the ReadRef bound
{
    let endian = macho_file.endian(); // Get Endianness enum value
    let mut modified = false;

    let mut command_iter = match macho_file.macho_load_commands() { // Use the new macho_load_commands method
        Ok(iter) => iter,
        Err(e) => {
            warn!(
                "  Failed to get load command iterator for {}: {}",
                file_path_for_log.display(),
                e
            );
            return Ok(false); // Cannot proceed without commands
        }
    };

    while let Some(cmd) = command_iter.next()? { // Handle Result from iterator.next()
        let cmd_type = cmd.cmd();
        let cmd_size = cmd.cmdsize() as usize;
        let command_offset_in_slice = 0; // Use 0 as base offset


         // Correctly use cmd.variant() which takes no arguments
        let path_info_opt: Option<(u32, std::result::Result<&'data [u8], object::read::Error>)> = match cmd.variant()? {
            // Only support the available variants
            LoadCommandVariant::Dylib(dylib_command) |
            LoadCommandVariant::IdDylib(dylib_command) => {
                let offset_in_cmd_struct = dylib_command.dylib.name.offset.get(endian);
                // Pass the command_data_slice as context for string resolution
                Some((offset_in_cmd_struct, cmd.string(endian, dylib_command.dylib.name)))
            },
            LoadCommandVariant::Rpath(rpath_command) => {
                let offset_in_cmd_struct = rpath_command.path.offset.get(endian);
                Some((offset_in_cmd_struct, cmd.string(endian, rpath_command.path)))
            },
            _ => None,
        };


        // Correct the Result type alias usage
         if let Some((path_offset_in_cmd_struct, path_bytes_result)) = path_info_opt {
            match path_bytes_result {
                // Check bytes directly for placeholder, then try UTF-8 conversion if needed
                Ok(path_bytes) => {
                    // TODO: Implement placeholder check directly on path_bytes
                    // For now, convert to string and check (less efficient but simpler)
                     match std::str::from_utf8(path_bytes) {
                         Ok(current_path_str) => {
                            if let Some(new_path_str) = find_and_replace_placeholders(current_path_str, replacements)
                            {
                                let allocated_len =
                                    cmd_size.saturating_sub(path_offset_in_cmd_struct as usize);

                                if allocated_len == 0 {
                                    warn!("  Calculated zero allocated length for path in command {:?} for {}", cmd_type, file_path_for_log.display());
                                    continue;
                                }

                                let patch_offset_in_slice =
                                    command_offset_in_slice + path_offset_in_cmd_struct as usize;

                                debug!("  Found placeholder in '{}'. Attempting patch at offset {} with allocated length {}", current_path_str, patch_offset_in_slice, allocated_len);

                                patch_path_in_buffer(
                                    buffer,
                                    patch_offset_in_slice,
                                    allocated_len,
                                    &new_path_str,
                                    file_path_for_log,
                                )?;
                                modified = true;
                            }
                         }
                          Err(_) => {
                              warn!("  Path bytes are not valid UTF-8 for command {:?} in {}. Skipping.", cmd_type, file_path_for_log.display());
                          }
                     }
                }
                 Err(e) => {
                    warn!(
                        "  Error resolving path string for command {:?} in {}: {}. Skipping.",
                        cmd_type,
                        file_path_for_log.display(),
                        e
                    );
                }
            }
        }
    }

    Ok(modified)
}

/// Helper to replace placeholders in a string.
fn find_and_replace_placeholders(
    current_path: &str,
    replacements: &HashMap<String, String>,
) -> Option<String> {
    let mut new_path = current_path.to_string();
    let mut path_modified = false;
    for (placeholder, replacement) in replacements {
        if new_path.contains(placeholder) {
            new_path = new_path.replace(placeholder, replacement);
            path_modified = true;
            debug!("   Replaced '{}' with '{}'", placeholder, replacement);
        }
    }
    if path_modified {
        Some(new_path)
    } else {
        None
    }
}

/// Writes the new path into the buffer at the specified offset.
#[cfg(target_os = "macos")]
fn patch_path_in_buffer(
    buffer_slice: &mut [u8],
    offset_in_slice: usize,
    allocated_len: usize,
    new_path_str: &str,
    file_path_for_log: &Path,
) -> Result<()> {
    let new_path_bytes = new_path_str.as_bytes();

    if new_path_bytes.len() >= allocated_len {
        error!(
            "New path '{}' ({} bytes including null) is too long for allocated space ({} bytes) in {}",
            new_path_str, new_path_bytes.len() + 1, allocated_len, file_path_for_log.display()
        );
        return Err(SapphireError::PathTooLongError(format!(
            "Relocation failed for {}: new path '{}' too long for binary structure (max {} bytes)",
            file_path_for_log.display(),
            new_path_str,
            allocated_len - 1
        )));
    }

    let mut padded_bytes = vec![0u8; allocated_len];
    padded_bytes[..new_path_bytes.len()].copy_from_slice(new_path_bytes);

    if offset_in_slice.checked_add(allocated_len).map_or(true, |end| end > buffer_slice.len()) {
        error!(
            "Internal relocation error: Calculated patch offset/length ({}+{}) exceeds buffer slice size ({}) for {}",
            offset_in_slice, allocated_len, buffer_slice.len(), file_path_for_log.display()
        );
        return Err(SapphireError::MachOModificationError(format!(
            "Internal relocation error: invalid patch offset/length for {}",
            file_path_for_log.display()
        )));
    }

    buffer_slice[offset_in_slice..offset_in_slice + allocated_len].copy_from_slice(&padded_bytes);
    info!(
        "    Patched Mach-O path at offset {} (relative to slice) in {}",
        offset_in_slice,
        file_path_for_log.display()
    );
    Ok(())
}

/// Writes the patched buffer to the original path atomically.
#[cfg(target_os = "macos")]
fn write_patched_buffer(original_path: &Path, buffer: &[u8]) -> Result<()> {
    let dir = original_path.parent().ok_or_else(|| {
        SapphireError::Generic(format!(
            "Cannot get parent directory for {}",
            original_path.display()
        ))
    })?;
    fs::create_dir_all(dir).map_err(|e| SapphireError::Io(e))?;

    let mut temp_file = NamedTempFile::new_in(dir)?;
    debug!(
        "    Writing patched buffer to temporary file: {:?}",
        temp_file.path()
    );
    temp_file.write_all(buffer)?;
    temp_file.flush()?;
    temp_file.as_file().sync_all()?;

    temp_file.persist(original_path).map_err(|e| {
        error!(
            "    Failed to persist/rename temporary file over {}: {}",
            original_path.display(),
            e.error
        );
        SapphireError::Io(e.error)
    })?;
    debug!(
        "    Atomically replaced {} with patched version",
        original_path.display()
    );
    Ok(())
}

/// Re-signs the binary using `codesign` on Apple Silicon platforms.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn resign_binary(path: &Path) -> Result<()> {
    info!("    Re-signing patched binary: {}", path.display());
    let status = StdCommand::new("codesign")
        .args([
            "-s",
            "-",
            "--force",
            "--preserve-metadata=identifier,entitlements",
        ])
        .arg(path)
        .status()
        .map_err(|e| {
            error!(
                "    Failed to execute codesign command for {}: {}",
                path.display(),
                e
            );
            SapphireError::Io(e)
        })?;

    if status.success() {
        debug!("    Successfully re-signed {}", path.display());
        Ok(())
    } else {
        error!(
            "    codesign command failed for {} with status: {}",
            path.display(),
            status
        );
        Err(SapphireError::CodesignError(format!(
            "Failed to re-sign patched binary {}, it may not be executable. Exit status: {}",
            path.display(),
            status
        )))
    }
}

#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
fn resign_binary(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn resign_binary(_path: &Path) -> Result<()> {
    Ok(())
}