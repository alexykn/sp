// src/build/link.rs
// Contains logic for linking binaries from the Cellar to bin directory

use crate::utils::error::{Result, SapphireError}; // Added SapphireError
use crate::model::formula::{Formula, FormulaDependencies}; // Added FormulaDependencies
use std::path::{Path, PathBuf};
use std::fs;
use std::os::unix::fs as unix_fs;
use serde_json;
use log::{debug, error, info, warn}; // Use log crate
use crate::build::get_homebrew_prefix; // Added import

/// Link all artifacts (opt, binaries, libraries, headers, etc.) from a formula's installation directory
///
/// # Arguments
///
/// * `formula` - The formula metadata.
/// * `installed_keg_path` - The actual path where the keg was installed (e.g., /opt/homebrew/Cellar/foo/1.2_1).
pub fn link_formula_artifacts(formula: &Formula, installed_keg_path: &Path) -> Result<()> {
    info!("==> Linking artifacts for {} from {}", formula.name(), installed_keg_path.display());

    let mut symlinks_created: Vec<String> = Vec::new();
    let prefix_dir = get_homebrew_prefix(); // Use imported function directly

    // --- Determine the root directory *inside* the keg containing installable content ---
    // This handles cases where extraction might create an intermediate directory (like the asciiquarium 1.1_5 dir).
    let formula_content_root = determine_content_root(installed_keg_path)?;
    info!("Using content root for linking: {}", formula_content_root.display());


    // --- 1. Link the main opt directory ---
    // The opt link *always* points to the top-level keg directory (e.g., .../1.1_5).
    let opt_link_path = prefix_dir.join("opt").join(formula.name());
    let target_keg_dir = installed_keg_path; // Opt link points to the actual keg dir

    debug!("Attempting to link opt directory: {} -> {}", opt_link_path.display(), target_keg_dir.display());
    remove_existing_link_target(&opt_link_path)?; // Helper to remove existing file/dir/link

    match unix_fs::symlink(target_keg_dir, &opt_link_path) {
        Ok(_) => {
            info!("  Linked opt path: {} -> {}", opt_link_path.display(), target_keg_dir.display());
            symlinks_created.push(opt_link_path.to_string_lossy().to_string());
        }
        Err(e) => {
            error!("Failed to create opt symlink {} -> {}: {}", opt_link_path.display(), target_keg_dir.display(), e);
            // Consider this a critical error
            return Err(SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed to create opt symlink for {}: {}", formula.name(), e))));
        }
    }

    // --- 2. Link standard artifact directories (bin, lib, include, share/man) ---
    // These links should point to items *within* the formula_content_root.
    let standard_artifact_dirs = ["bin", "lib", "include", "share"];
    for dir_name in &standard_artifact_dirs {
        let source_subdir = formula_content_root.join(dir_name); // Source is inside content root
        let target_prefix_subdir = prefix_dir.join(dir_name); // Target is in the main prefix

        debug!("Checking for artifacts in source subdir: {}", source_subdir.display());

        if source_subdir.is_dir() {
            // Ensure the target directory in the prefix exists
            if !target_prefix_subdir.exists() {
                debug!("Creating target prefix directory: {}", target_prefix_subdir.display());
                if let Err(e) = fs::create_dir_all(&target_prefix_subdir) {
                    error!("Failed to create target directory {}: {}", target_prefix_subdir.display(), e);
                    continue; // Skip this artifact type if target dir creation fails
                }
            }

            match fs::read_dir(&source_subdir) {
                Ok(entries) => {
                    for entry_result in entries {
                        match entry_result {
                            Ok(entry) => {
                                let source_item_path = entry.path(); // e.g., .../keg/1.1_5/bin/asciiquarium
                                let file_name = entry.file_name();
                                let target_link = target_prefix_subdir.join(&file_name); // e.g., /opt/homebrew/bin/asciiquarium

                                debug!("  Found potential artifact: {}", source_item_path.display());

                                // Attempt to remove anything existing at the target link path
                                if remove_existing_link_target(&target_link).is_ok() {
                                     debug!("    Attempting symlink creation: {} -> {}", target_link.display(), source_item_path.display());
                                     match unix_fs::symlink(&source_item_path, &target_link) {
                                         Ok(_) => {
                                             info!("  Linked {} -> {}", target_link.display(), source_item_path.display());
                                             symlinks_created.push(target_link.to_string_lossy().to_string());
                                         }
                                         Err(e) => {
                                             error!("    Failed to create symlink {} -> {}: {}", target_link.display(), source_item_path.display(), e);
                                             // Log error but continue linking other items
                                         }
                                     }
                                }
                                // Else: remove_existing_link_target logged a warning, skip linking this item
                            }
                            Err(e) => { warn!("  Failed to process directory entry in {}: {}", source_subdir.display(), e); }
                        }
                    }
                }
                Err(e) => { warn!("Failed to read source artifact directory {}: {}", source_subdir.display(), e); }
            }
        } else {
             debug!("Source artifact directory {} does not exist, skipping.", source_subdir.display());
        }
    }

    // --- 3. Link executables from libexec ---
    // These also link from formula_content_root/libexec to prefix/bin.
    let source_libexec_dir = formula_content_root.join("libexec");
    let target_bin_dir = prefix_dir.join("bin"); // Links go into the main bin dir

    if source_libexec_dir.is_dir() {
        debug!("Checking for executables in source libexec: {}", source_libexec_dir.display());
        // Ensure target bin directory exists
        if !target_bin_dir.exists() {
            debug!("Creating target bin directory: {}", target_bin_dir.display());
            if let Err(e) = fs::create_dir_all(&target_bin_dir) {
                error!("Failed to create target bin directory {}: {}. Cannot link libexec.", target_bin_dir.display(), e);
                // If we can't create bin, we can't link libexec stuff, so return early for this section
                // Write manifest with what we have linked so far.
                write_install_manifest(installed_keg_path, &symlinks_created)?;
                return Ok(());
            }
        }

        // Use recursive helper to handle potential subdirectories like libexec/bin
        link_libexec_recursive(&source_libexec_dir, &target_bin_dir, &mut symlinks_created)?;

    } else {
         debug!("Source libexec directory {} does not exist, skipping.", source_libexec_dir.display());
    }


    // --- 4. Write install manifest ---
    write_install_manifest(installed_keg_path, &symlinks_created)?;

    info!("Successfully completed linking artifacts for {}", formula.name());
    Ok(())
}


/// Recursively link executables found in libexec subdirectories.
fn link_libexec_recursive(
    current_libexec_dir: &Path,
    target_bin_dir: &Path,
    symlinks_created: &mut Vec<String>,
) -> Result<()> {
    debug!("Recursively checking libexec dir: {}", current_libexec_dir.display());
    match fs::read_dir(current_libexec_dir) {
        Ok(entries) => {
            for entry_result in entries {
                match entry_result {
                    Ok(entry) => {
                        let source_item = entry.path();
                        if source_item.is_dir() {
                            // Recurse into subdirectories
                            link_libexec_recursive(&source_item, target_bin_dir, symlinks_created)?;
                        } else if source_item.is_file() {
                            // Check if the file is executable
                            match is_executable(&source_item) {
                                Ok(true) => {
                                    let file_name = entry.file_name();
                                    let target_link = target_bin_dir.join(file_name); // Link directly into prefix/bin
                                    debug!("  Found executable in recursive libexec: {}", source_item.display());

                                    if remove_existing_link_target(&target_link).is_ok() {
                                         debug!("    Attempting to link libexec executable: {} -> {}", target_link.display(), source_item.display());
                                         match unix_fs::symlink(&source_item, &target_link) {
                                             Ok(_) => { info!("  Linked {} -> {}", target_link.display(), source_item.display()); symlinks_created.push(target_link.to_string_lossy().to_string()); }
                                             Err(e) => { error!("    Failed to create symlink from libexec {} -> {}: {}", target_link.display(), source_item.display(), e); }
                                         }
                                    }
                                    // Else: remove_existing_link_target logged warning, skip
                                }
                                Ok(false) => { /* Not executable, ignore */ }
                                Err(e) => { warn!("    Could not check executable status for {}: {}", source_item.display(), e); }
                            }
                        }
                    }
                    Err(e) => { warn!("  Failed to process directory entry in {}: {}", current_libexec_dir.display(), e); }
                }
            }
        }
        Err(e) => { warn!("Failed to read libexec subdirectory {}: {}", current_libexec_dir.display(), e); }
    }
    Ok(())
}

/// Determines the root directory containing installable content within a keg.
/// Handles cases where extraction might create an intermediate directory.
fn determine_content_root(installed_keg_path: &Path) -> Result<PathBuf> {
    let mut potential_subdirs = Vec::new();
    let mut top_level_files_found = false;

    // Check if the installed_keg_path itself exists and is a directory
    if !installed_keg_path.is_dir() {
        error!("Keg path {} does not exist or is not a directory!", installed_keg_path.display());
        return Err(SapphireError::NotFound(format!("Keg path not found: {}", installed_keg_path.display())));
    }

    match fs::read_dir(installed_keg_path) {
        Ok(entries) => {
            for entry_res in entries {
                if let Ok(entry) = entry_res {
                    let path = entry.path();
                    let file_name = entry.file_name();
                    // Ignore dotfiles and manifest/receipt files when checking for intermediate dirs
                    if file_name.to_string_lossy().starts_with('.') || file_name == "INSTALL_MANIFEST.json" || file_name == "INSTALL_RECEIPT.json" {
                        continue;
                    }
                    if path.is_dir() {
                        potential_subdirs.push(path);
                    } else if path.is_file() {
                        // If we find regular files at the top level, assume no intermediate dir
                        top_level_files_found = true;
                        debug!("Found file '{}' at top level of keg {}, assuming no intermediate dir.", file_name.to_string_lossy(), installed_keg_path.display());
                        break; // No need to check further
                    }
                } else {
                    // Log error if reading an entry fails, but continue checking others
                    warn!("Failed to read directory entry in {}: {}", installed_keg_path.display(), entry_res.err().unwrap());
                }
            }
        }
        Err(e) => {
            // If we can't read the keg dir, we have to assume it's the root
            warn!("Could not read keg directory {} to check for intermediate dir: {}. Assuming keg path is content root.", installed_keg_path.display(), e);
            return Ok(installed_keg_path.to_path_buf());
        }
    }

    // If only one directory found (and no top-level files), assume it's the content root
    if potential_subdirs.len() == 1 && !top_level_files_found {
        let intermediate_dir = potential_subdirs.remove(0); // Use remove(0) as it's the only element
        debug!("Detected single intermediate content directory: {}", intermediate_dir.display());
        Ok(intermediate_dir)
    } else {
        // If multiple dirs, or top-level files, or zero dirs, assume the keg path itself is the root
        if potential_subdirs.len() > 1 {
            warn!("Multiple potential content directories found under keg {}. Using main keg directory as content root.", installed_keg_path.display());
        } else if top_level_files_found {
             debug!("Top-level files found in keg {}. Using main keg directory as content root.", installed_keg_path.display());
        } else if potential_subdirs.is_empty() { // Explicitly check for empty
            debug!("No subdirectories or files found (excluding ignored ones) in keg {}. Using main keg directory as content root.", installed_keg_path.display());
        }
        Ok(installed_keg_path.to_path_buf())
    }
}

/// Helper to remove an existing file, directory, or symlink at a given path.
/// Returns Ok(()) if removal succeeded or path didn't exist, Err otherwise.
fn remove_existing_link_target(path: &Path) -> Result<()> {
     match path.symlink_metadata() { // Use symlink_metadata to check existence without following link
         Ok(metadata) => {
             debug!("    Removing existing item at link target: {}", path.display());
             let is_dir = metadata.file_type().is_dir();
             let is_symlink = metadata.file_type().is_symlink();
             // Check if it's a directory *and not* a symlink (to avoid removing linked dirs unintentionally)
             let is_real_dir = is_dir && !is_symlink;

             let remove_result = if is_real_dir {
                 fs::remove_dir_all(path)
             } else {
                 // Remove file or symlink
                 fs::remove_file(path)
             };

             if let Err(e) = remove_result {
                warn!("    Failed to remove existing item at link target {}: {}", path.display(), e);
                // Return an error to prevent linking over it
                return Err(SapphireError::Io(e));
             }
             Ok(()) // Removal successful
         }
         Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
             Ok(()) // Path doesn't exist, nothing to remove
         }
         Err(e) => {
             // Other errors getting metadata
             warn!("    Failed to get metadata for existing item {}: {}", path.display(), e);
             Err(SapphireError::Io(e))
         }
     }
}

/// Writes the list of created symlinks to the manifest file.
fn write_install_manifest(installed_keg_path: &Path, symlinks_created: &[String]) -> Result<()> {
    // Manifest should always be in the top-level keg directory.
    let manifest_path = installed_keg_path.join("INSTALL_MANIFEST.json");
    debug!("Writing install manifest to: {}", manifest_path.display());
    match serde_json::to_string_pretty(&symlinks_created) {
        Ok(manifest_json) => {
            match fs::write(&manifest_path, manifest_json) {
                Ok(_) => { info!("Wrote install manifest with {} links: {}", symlinks_created.len(), manifest_path.display()); }
                Err(e) => {
                    error!("Failed to write install manifest {}: {}", manifest_path.display(), e);
                    // Return error if manifest write fails
                    return Err(SapphireError::Io(e));
                }
            }
        }
        Err(e) => {
            error!("Failed to serialize install manifest data: {}", e);
             // Return error if serialization fails
            return Err(SapphireError::Json(e));
        }
    }
    Ok(())
}


// --- Legacy unlink_formula_binaries (kept for fallback, ensure it uses determine_content_root) ---

/// Unlink artifacts for a formula based on its manifest file or fallback to legacy.
pub fn unlink_formula_artifacts(formula: &Formula) -> Result<()> {
    info!("==> Unlinking artifacts for {}", formula.name());
    // *** FIX: Use the FormulaDependencies trait to get the expected path ***
    let expected_keg_path = match formula.install_prefix(&crate::build::get_cellar_path()) {
        Ok(path) => path,
        Err(e) => {
             error!("Cannot determine keg path for {}: {}", formula.name(), e);
             // Attempt unlink using opt path if possible as last resort?
             return Err(SapphireError::Generic(format!("Cannot determine keg path for {}", formula.name())));
        }
    };
    let manifest_path = expected_keg_path.join("INSTALL_MANIFEST.json");

    // First, try to unlink using the manifest
    if manifest_path.is_file() { // Use is_file to be sure
         debug!("Reading install manifest: {}", manifest_path.display());
         match fs::read_to_string(&manifest_path) {
            Ok(manifest_str) => {
                match serde_json::from_str::<Vec<String>>(&manifest_str) {
                    Ok(symlinks_to_remove) => {
                        let mut unlinked_count = 0;
                        let mut removal_errors = 0;
                        if symlinks_to_remove.is_empty() {
                             warn!("Install manifest {} is empty. Cannot perform manifest-based unlink.", manifest_path.display());
                             // Decide if fallback is appropriate when manifest is empty
                             // return unlink_formula_binaries_legacy(formula, &expected_keg_path);
                             info!("No links recorded in manifest, unlink complete.");
                             return Ok(()) // Treat empty manifest as success (nothing to unlink)
                        } else {
                             for symlink_str in symlinks_to_remove {
                                let symlink_path = PathBuf::from(symlink_str);
                                // Use symlink_metadata to check existence without following
                                match symlink_path.symlink_metadata() {
                                    Ok(_) => { // Exists (as file, dir, or link)
                                        match fs::remove_file(&symlink_path) {
                                             Ok(_) => { info!("Removed symlink: {}", symlink_path.display()); unlinked_count += 1; }
                                             Err(e) => { warn!("Failed to remove symlink {}: {}", symlink_path.display(), e); removal_errors += 1; }
                                         }
                                    }
                                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                         debug!("Symlink listed in manifest not found: {}", symlink_path.display());
                                    }
                                    Err(e) => {
                                         warn!("Failed to get metadata for symlink {}: {}", symlink_path.display(), e);
                                         removal_errors += 1;
                                    }
                                }
                            }
                            info!("Successfully unlinked {} artifacts based on manifest.", unlinked_count);
                            if removal_errors > 0 {
                                 warn!("Encountered {} errors while removing links listed in manifest.", removal_errors);
                            }
                            return Ok(()); // Success using manifest
                        }
                    }
                    Err(e) => {
                         error!("Failed to parse formula install manifest {}: {}. Falling back to legacy unlink...", manifest_path.display(), e);
                         return unlink_formula_binaries_legacy(formula, &expected_keg_path);
                    }
                }
            }
             Err(e) => {
                error!("Failed to read formula install manifest {}: {}. Falling back to legacy unlink...", manifest_path.display(), e);
                return unlink_formula_binaries_legacy(formula, &expected_keg_path);
            }
        }
    } else {
        warn!("Warning: No install manifest found at {}. Falling back to legacy unlink...", manifest_path.display());
        return unlink_formula_binaries_legacy(formula, &expected_keg_path);
    }
}


/// Unlink binaries for a formula (Legacy - only checks bin and libexec based on expected keg path)
fn unlink_formula_binaries_legacy(formula: &Formula, expected_keg_path: &Path) -> Result<()> {
    warn!("==> Using legacy unlink for {}", formula.name());
    let bin_dir = get_bin_directory();
    if !bin_dir.exists() {
        info!("Target bin directory {} does not exist, nothing to do for legacy unlink.", bin_dir.display());
        return Ok(());
    }

    // Determine content root based on the *expected* keg path for legacy unlink
    // If keg path itself doesn't exist, we can't really do legacy unlink effectively
    if !expected_keg_path.is_dir() {
         warn!("Expected keg path {} does not exist. Cannot perform legacy unlink.", expected_keg_path.display());
         // Still try to remove opt link as a last resort
         let opt_link_path = get_homebrew_prefix().join("opt").join(formula.name());
         if opt_link_path.symlink_metadata().is_ok() {
            warn!("Attempting to remove opt link {} even though keg is missing.", opt_link_path.display());
            if let Err(e) = fs::remove_file(&opt_link_path) {
                warn!("Failed to remove legacy opt symlink {}: {}", opt_link_path.display(), e);
            } else {
                info!("Removed legacy opt symlink: {}", opt_link_path.display());
            }
         }
         return Ok(());
    }

    let formula_content_root = match determine_content_root(expected_keg_path) {
        Ok(root) => root,
        Err(_) => {
            // If determine_content_root fails after confirming keg exists, something is odd.
            warn!("Could not determine content root for legacy unlink of {}. Assuming top level.", expected_keg_path.display());
            expected_keg_path.to_path_buf()
        }
    };
    debug!("Legacy unlink using content root: {}", formula_content_root.display());


    let formula_bin_dir = formula_content_root.join("bin");
    let formula_libexec_dir = formula_content_root.join("libexec");
    let mut unlinked_count = 0;
    let mut unlink_errors = 0;

    // Unlink items from bin
    if formula_bin_dir.is_dir() {
        match unlink_executables_from_dir(&formula_bin_dir, &bin_dir) {
             Ok(count) => unlinked_count += count,
             Err(_) => unlink_errors += 1, // Count errors from helper
        }
    }
    // Unlink items from libexec
    if formula_libexec_dir.is_dir() {
         match unlink_executables_from_dir(&formula_libexec_dir, &bin_dir) {
             Ok(count) => unlinked_count += count,
             Err(_) => unlink_errors += 1,
         }
    }

    // Unlink the opt dir last in legacy mode
    let opt_link_path = get_homebrew_prefix().join("opt").join(formula.name());
    match is_symlink_to(&opt_link_path, expected_keg_path) { // Check if opt points to the *expected* keg
        Ok(true) => {
            if let Err(e) = fs::remove_file(&opt_link_path) {
                warn!("Failed to remove legacy opt symlink {}: {}", opt_link_path.display(), e);
                unlink_errors += 1;
            } else {
                debug!("Removed legacy opt symlink: {}", opt_link_path.display());
                // Don't increment unlinked_count here, as we track executable links primarily
            }
        }
        Ok(false) => debug!("Legacy unlink: Opt link {} exists but doesn't point to expected keg {}.", opt_link_path.display(), expected_keg_path.display()),
        Err(e) => {
            warn!("Failed to check legacy opt symlink {}: {}", opt_link_path.display(), e);
            unlink_errors += 1;
        }
    }


    if unlinked_count == 0 && unlink_errors == 0 && !formula_bin_dir.exists() && !formula_libexec_dir.exists() {
        debug!("Legacy unlink: No bin or libexec directory found in {} and opt link not removed.", formula_content_root.display());
    } else if unlinked_count > 0 || unlink_errors == 0 { // Consider success if links were removed OR no errors occurred
        info!("Successfully unlinked {} binaries for {} (legacy method).", unlinked_count, formula.name());
        if unlink_errors > 0 {
            warn!("Encountered {} errors during legacy unlink.", unlink_errors);
        }
    } else { // unlinked_count == 0 && unlink_errors > 0
        error!("Legacy unlink failed for {}. Encountered {} errors and removed 0 links.", formula.name(), unlink_errors);
        return Err(SapphireError::Generic(format!("Legacy unlink failed for {}", formula.name())));
    }
    Ok(())
}


// Helper for legacy unlink to process one source directory and its subdirectories
fn unlink_executables_from_dir(source_exec_dir: &Path, target_link_dir: &Path) -> Result<usize> {
    let mut unlinked_count = 0;
    if !source_exec_dir.is_dir() { return Ok(0); } // Source dir doesn't exist

    match fs::read_dir(source_exec_dir) {
        Ok(entries) => {
            for entry_result in entries {
                match entry_result {
                    Ok(entry) => {
                        let source_path = entry.path();
                        // Check subdirs recursively for executables in legacy mode too
                        if source_path.is_dir() {
                            // Ignore errors from recursive calls? Or propagate?
                            // Let's propagate for now.
                            unlinked_count += unlink_executables_from_dir(&source_path, target_link_dir)?;
                        }
                        else if source_path.is_file() {
                             match is_executable(&source_path) {
                                Ok(true) => {
                                    let file_name = entry.file_name();
                                    let target_link = target_link_dir.join(file_name);
                                    // Check if the target exists and is a symlink pointing to the source
                                    match is_symlink_to(&target_link, &source_path) {
                                        Ok(true) => {
                                             match fs::remove_file(&target_link) {
                                                Ok(_) => { debug!("  Legacy unlinked {} -> {}", target_link.display(), source_path.display()); unlinked_count += 1; }
                                                 Err(e) => { warn!("Failed to remove legacy symlink {}: {}", target_link.display(), e); } // Don't return error, just warn
                                             }
                                        }
                                        Ok(false) => { /* Target exists but doesn't point here, or isn't a link */ }
                                        Err(e) => { warn!("Failed to check legacy symlink {}: {}", target_link.display(), e); } // Error checking link
                                    }
                                }
                                Ok(false) => { /* Ignore non-executable files */ }
                                Err(e) => { warn!("Could not check executable status for {}: {}", source_path.display(), e); }
                            }
                        }
                    }
                     Err(e) => { warn!("Failed to process directory entry in {}: {}", source_exec_dir.display(), e); }
                }
            }
        }
        Err(e) => {
            warn!("Failed to read source directory {} during legacy unlink: {}", source_exec_dir.display(), e);
            // Return error if we can't read the source directory
            return Err(SapphireError::Io(e));
        }
    }
    Ok(unlinked_count)
}


/// Get the standard Homebrew bin directory (internal helper)
pub(crate) fn get_bin_directory() -> PathBuf {
    get_homebrew_prefix().join("bin")
}

/// Check if a file is executable (internal helper)
fn is_executable(path: &Path) -> Result<bool> {
    // Use try_exists to avoid errors if path is a broken symlink during check
    if !path.try_exists().unwrap_or(false) || !path.is_file() { return Ok(false); }
    if cfg!(unix) {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(path) {
            Ok(metadata) => { Ok(metadata.permissions().mode() & 0o111 != 0) }
            Err(e) => { Err(SapphireError::Io(e)) } // Propagate IO errors reading metadata
        }
    } else {
        // Basic check for non-unix (consider executable extensions?)
        Ok(true) // Assume executable if it exists on non-unix for now
    }
}

/// Check if a symlink points to a specific target (internal helper)
fn is_symlink_to(link: &Path, target: &Path) -> Result<bool> {
    match link.symlink_metadata() {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink() { return Ok(false); } // Not a symlink
            match fs::read_link(link) {
                Ok(link_target_path) => {
                    // Canonicalize both paths for robust comparison
                     match (target.canonicalize(), link.parent()) {
                        (Ok(canonical_target), Some(link_parent)) => {
                            // Resolve link target relative to the link's parent directory if it's relative
                             let resolved_link_target = if link_target_path.is_absolute() {
                                 link_target_path
                             } else {
                                 link_parent.join(&link_target_path)
                             };
                             // Canonicalize the resolved link target
                             match resolved_link_target.canonicalize() {
                                 Ok(canonical_link_target) => Ok(canonical_link_target == canonical_target),
                                 Err(e) => {
                                     // Canonicalization fails if target doesn't exist. Compare raw resolved path.
                                     debug!("Could not canonicalize link target path {} (from link {}): {}. Comparing raw paths.", resolved_link_target.display(), link.display(), e);
                                     Ok(resolved_link_target == canonical_target) // Compare potentially non-existent paths
                                 }
                             }
                        }
                        (Err(e), _) => {
                             debug!("Could not canonicalize expected target path {}: {}", target.display(), e);
                             Ok(false) // Cannot compare if target canonicalization fails
                        }
                        (_, None) => {
                             warn!("Could not get parent directory for link {}", link.display());
                             Ok(false) // Cannot resolve relative links without parent
                        }
                     }
                }
                Err(e) => {
                    // Error reading the link itself
                    warn!("Failed to read link target for {}: {}", link.display(), e);
                    Err(SapphireError::Io(e)) // Propagate error reading link
                },
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(false) // Link doesn't exist
        }
        Err(e) => {
            // Other errors getting metadata for the link
            warn!("Failed to get symlink metadata for {}: {}", link.display(), e);
            Err(SapphireError::Io(e)) // Propagate other metadata errors
        },
    }
}