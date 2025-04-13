// src/build/link.rs
// Contains logic for linking binaries from the Cellar to bin directory

use crate::Result;
use crate::model::formula::Formula;
use std::path::{Path, PathBuf};
use std::fs;
use std::os::unix::fs as unix_fs;
use serde_json;
use crate::utils::error::SapphireError;
use log::{debug, error, info, warn}; // Use log crate
use crate::build::get_homebrew_prefix; // Added import

/// Link all artifacts (opt, binaries, libraries, headers, etc.) from a formula's installation directory
pub fn link_formula_artifacts(formula: &Formula, formula_dir: &Path) -> Result<()> {
    info!("==> Linking artifacts for {}", formula.name());

    let mut symlinks: Vec<String> = Vec::new();
    let prefix_dir = get_homebrew_prefix(); // Use imported function directly

    // --- Detect potential intermediate directory ---
    let mut formula_content_root = formula_dir.to_path_buf();
    let mut potential_subdirs = Vec::new();
    match fs::read_dir(formula_dir) { // Read original formula_dir here
        Ok(entries) => {
            for entry_res in entries {
                if let Ok(entry) = entry_res {
                    let path = entry.path();
                    let file_name = entry.file_name();
                    if file_name != "INSTALL_MANIFEST.json" && file_name != "INSTALL_RECEIPT.json" {
                        if path.is_dir() {
                            potential_subdirs.push(path);
                        } else {
                             debug!("Found file '{}' at top level of keg, assuming no intermediate dir.", file_name.to_string_lossy());
                             potential_subdirs.clear();
                             break;
                        }
                    }
                } else {
                    // Log the error if reading an entry fails
                    warn!("Failed to read directory entry in {}: {}", formula_dir.display(), entry_res.err().unwrap());
                }
            }
        }
        Err(e) => {
            warn!("Could not read keg directory {} to check for intermediate dir: {}", formula_dir.display(), e);
        }
    }

    if potential_subdirs.len() == 1 {
        let intermediate_dir = &potential_subdirs[0];
        info!("Detected intermediate content directory: {}", intermediate_dir.display());
        formula_content_root = intermediate_dir.clone();
    } else if potential_subdirs.len() > 1 {
        warn!("Multiple directories found directly under keg {}. Assuming content root is the main keg directory.", formula_dir.display());
    }
     debug!("Using content root for linking: {}", formula_content_root.display());


    // --- 1. Link the main opt directory ---
    let opt_path = prefix_dir.join("opt").join(formula.name());
    let target_cellar_dir = formula_dir;

    debug!("Attempting to link opt directory: {} -> {}", opt_path.display(), target_cellar_dir.display());
    if opt_path.exists() || opt_path.symlink_metadata().is_ok() {
        debug!("Removing existing item at opt path: {}", opt_path.display());
        let is_real_dir = opt_path.is_dir() && !opt_path.symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if is_real_dir {
             if let Err(e) = fs::remove_dir_all(&opt_path) { warn!("Failed to remove existing directory at opt path {}: {}", opt_path.display(), e); }
        } else {
             if let Err(e) = fs::remove_file(&opt_path) { warn!("Failed to remove existing file/symlink at opt path {}: {}", opt_path.display(), e); }
        }
    }
    match unix_fs::symlink(target_cellar_dir, &opt_path) {
        Ok(_) => {
            info!("  Linked opt path: {} -> {}", opt_path.display(), target_cellar_dir.display());
            symlinks.push(opt_path.to_string_lossy().to_string());
        }
        Err(e) => {
            error!("Failed to create opt symlink {} -> {}: {}", opt_path.display(), target_cellar_dir.display(), e);
            return Err(SapphireError::Io(e));
        }
    }

    // --- 2. Link standard artifact directories (bin, lib, include, share/man) ---
    let standard_artifact_dirs = ["bin", "lib", "include", "share"];
    for dir_name in &standard_artifact_dirs {
        let formula_subdir = formula_content_root.join(dir_name);
        let target_prefix_subdir = prefix_dir.join(dir_name);
        let is_bin_dir = dir_name == &"bin";

        if is_bin_dir {
            debug!("Processing standard artifact directory: '{}' (from {})", dir_name, formula_content_root.display());
        } else {
            debug!("Checking for artifacts in formula subdir: {}", formula_subdir.display());
        }

        if formula_subdir.is_dir() {
             if !target_prefix_subdir.exists() {
                 debug!("Creating target prefix directory: {}", target_prefix_subdir.display());
                 if let Err(e) = fs::create_dir_all(&target_prefix_subdir) { error!("Failed to create target directory {}: {}", target_prefix_subdir.display(), e); continue; }
             }

            match fs::read_dir(&formula_subdir) {
                Ok(entries) => {
                    for entry_result in entries {
                        match entry_result {
                            Ok(entry) => {
                                let source_path = entry.path();
                                let file_name = entry.file_name();
                                let target_link = target_prefix_subdir.join(&file_name);

                                if is_bin_dir {
                                    debug!("  Found item in bin: '{}' ({})", file_name.to_string_lossy(), source_path.display());
                                    debug!("    Target link path would be: {}", target_link.display());
                                } else {
                                    debug!("Found potential artifact: {}", source_path.display());
                                }

                                let mut skip_linking = false;
                                if target_link.exists() || target_link.symlink_metadata().is_ok() {
                                    debug!("    Removing existing item at link target: {}", target_link.display());
                                    let is_target_real_dir = target_link.is_dir() && !target_link.symlink_metadata()
                                         .map(|m| m.file_type().is_symlink())
                                         .unwrap_or(false);
                                     if is_target_real_dir {
                                         if let Err(e) = fs::remove_dir_all(&target_link) {
                                            warn!("    Failed to remove existing directory at link target {}: {}", target_link.display(), e);
                                            skip_linking = true;
                                         }
                                    } else {
                                         if let Err(e) = fs::remove_file(&target_link) {
                                             warn!("    Failed to remove existing file/symlink at link target {}: {}", target_link.display(), e);
                                             skip_linking = true;
                                         }
                                    }
                                }

                                if skip_linking {
                                     if is_bin_dir { debug!("    Skipping symlink creation for {} due to removal failure.", file_name.to_string_lossy()); }
                                     continue;
                                }

                                if is_bin_dir { debug!("    Attempting symlink creation: {} -> {}", target_link.display(), source_path.display()); }
                                match unix_fs::symlink(&source_path, &target_link) {
                                    Ok(_) => {
                                        if is_bin_dir { debug!("    Symlink SUCCESS for {}", file_name.to_string_lossy()); }
                                        info!("  Linked {} -> {}", target_link.display(), source_path.display());
                                        symlinks.push(target_link.to_string_lossy().to_string());
                                    }
                                    Err(e) => {
                                        if is_bin_dir { debug!("    Symlink FAILURE for {}: {}", file_name.to_string_lossy(), e); }
                                        error!("Failed to create symlink {} -> {}: {}", target_link.display(), source_path.display(), e);
                                    }
                                }
                            }
                            Err(e) => { warn!("Failed to process directory entry in {}: {}", formula_subdir.display(), e); }
                        }
                    }
                }
                Err(e) => { warn!("Failed to read formula artifact directory {}: {}", formula_subdir.display(), e); }
            }
        } else {
             if is_bin_dir { debug!("Formula bin directory {} does not exist, skipping.", formula_subdir.display()); }
             else { debug!("Formula artifact directory {} does not exist, skipping.", formula_subdir.display()); }
        }
    }

    // --- 3. Link executables from libexec ---
    let libexec_dir = formula_content_root.join("libexec");
    let target_bin_dir = prefix_dir.join("bin");

    if libexec_dir.is_dir() {
        debug!("Checking for executables in libexec: {}", libexec_dir.display());
        if !target_bin_dir.exists() {
            debug!("Creating target bin directory: {}", target_bin_dir.display());
            if let Err(e) = fs::create_dir_all(&target_bin_dir) { error!("Failed to create target bin directory {}: {}. Cannot link libexec.", target_bin_dir.display(), e); }
        }

        if target_bin_dir.exists() {
            match fs::read_dir(&libexec_dir) {
                Ok(entries) => {
                     for entry_result in entries {
                         match entry_result {
                            Ok(entry) => {
                                let source_path = entry.path();
                                if source_path.is_file() {
                                     match is_executable(&source_path) {
                                        Ok(true) => {
                                            let file_name = entry.file_name();
                                            let target_link = target_bin_dir.join(&file_name);
                                            debug!("Found executable in libexec: {}", source_path.display());
                                            debug!("Attempting to link libexec executable: {} -> {}", target_link.display(), source_path.display());

                                             if target_link.exists() || target_link.symlink_metadata().is_ok() {
                                                 debug!("Removing existing item at libexec link target: {}", target_link.display());
                                                 if let Err(e) = fs::remove_file(&target_link) { warn!("Failed to remove existing file/symlink at libexec link target {}: {}", target_link.display(), e); continue; }
                                             }
                                             match unix_fs::symlink(&source_path, &target_link) {
                                                 Ok(_) => { info!("  Linked {} -> {}", target_link.display(), source_path.display()); symlinks.push(target_link.to_string_lossy().to_string()); }
                                                 Err(e) => { error!("Failed to create symlink from libexec {} -> {}: {}", target_link.display(), source_path.display(), e); }
                                             }
                                         }
                                         Ok(false) => { /* Not executable, ignore */ }
                                         Err(e) => { warn!("Could not check executable status for {}: {}", source_path.display(), e); }
                                     }
                                 }
                             }
                             Err(e) => { warn!("Failed to process directory entry in {}: {}", libexec_dir.display(), e); }
                         }
                     }
                }
                 Err(e) => { warn!("Failed to read libexec directory {}: {}", libexec_dir.display(), e); }
            }
        }
    } else {
         debug!("Formula libexec directory {} does not exist, skipping.", libexec_dir.display());
    }


    // --- 4. Write install manifest ---
    let manifest_path = formula_dir.join("INSTALL_MANIFEST.json");
    debug!("Writing install manifest to: {}", manifest_path.display());
    match serde_json::to_string_pretty(&symlinks) {
        Ok(manifest_json) => {
            match fs::write(&manifest_path, manifest_json) {
                Ok(_) => { info!("Wrote install manifest: {}", manifest_path.display()); }
                Err(e) => { error!("Failed to write install manifest {}: {}", manifest_path.display(), e); }
            }
        }
        Err(e) => { error!("Failed to serialize install manifest data: {}", e); }
    }

    info!("Successfully completed linking artifacts for {}", formula.name());
    Ok(())
}

/// Unlink artifacts for a formula based on its manifest file.
pub fn unlink_formula_artifacts(formula: &Formula) -> Result<()> {
    info!("==> Unlinking artifacts for {}", formula.name());
    let formula_dir = crate::build::formula::get_formula_cellar_path(formula);
    let manifest_path = formula_dir.join("INSTALL_MANIFEST.json");

    if manifest_path.exists() {
         debug!("Reading install manifest: {}", manifest_path.display());
         match fs::read_to_string(&manifest_path) {
            Ok(manifest_str) => {
                match serde_json::from_str::<Vec<String>>(&manifest_str) {
                    Ok(symlinks_to_remove) => {
                        let mut unlinked_count = 0;
                        if symlinks_to_remove.is_empty() {
                             warn!("Install manifest is empty. Cannot perform manifest-based unlink.");
                             unlink_formula_binaries(formula)?;
                        } else {
                             for symlink_str in symlinks_to_remove {
                                let symlink_path = PathBuf::from(symlink_str);
                                if symlink_path.symlink_metadata().is_ok() {
                                     match fs::remove_file(&symlink_path) {
                                         Ok(_) => { info!("Removed symlink: {}", symlink_path.display()); unlinked_count += 1; }
                                         Err(e) => { warn!("Failed to remove symlink {}: {}", symlink_path.display(), e); }
                                     }
                                } else {
                                    debug!("Symlink listed in manifest not found or not a symlink: {}", symlink_path.display());
                                }
                            }
                            info!("Successfully unlinked {} artifacts based on manifest.", unlinked_count);
                        }
                    }
                    Err(e) => {
                         error!("Failed to parse formula install manifest {}: {}. Cannot perform manifest-based unlink. Attempting legacy unlink...", manifest_path.display(), e);
                         unlink_formula_binaries(formula)?;
                    }
                }
            }
             Err(e) => {
                error!("Failed to read formula install manifest {}: {}. Cannot perform manifest-based unlink. Attempting legacy unlink...", manifest_path.display(), e);
                unlink_formula_binaries(formula)?;
            }
        }
    } else {
        warn!("Warning: No install manifest found at {}. Cannot perform manifest-based unlink. Attempting legacy unlink...", manifest_path.display());
        unlink_formula_binaries(formula)?;
    }

    Ok(())
}


// --- Legacy unlink_formula_binaries (kept for fallback) ---

/// Unlink binaries for a formula (Legacy - only checks /bin and /libexec)
pub fn unlink_formula_binaries(formula: &Formula) -> Result<()> {
    warn!("==> Using legacy unlink for {}", formula.name);
    let bin_dir = get_bin_directory();
    if !bin_dir.exists() { return Ok(()); }

    let formula_dir = crate::build::formula::get_formula_cellar_path(formula);
    let mut formula_content_root = formula_dir.clone(); // Clone formula_dir here

     // *** Fix E0382: Borrow formula_dir for fs::read_dir calls ***
    let potential_subdir = fs::read_dir(&formula_dir) // Borrow here
        .map_err(SapphireError::Io)? // Handle potential IO error from read_dir
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_dir() && e.file_name() != "INSTALL_MANIFEST.json" && e.file_name() != "INSTALL_RECEIPT.json");

     if let Some(entry) = potential_subdir {
         // *** Fix E0382: Borrow formula_dir for fs::read_dir call ***
         let other_entries_count = fs::read_dir(&formula_dir) // Borrow here
            .map_err(SapphireError::Io)? // Handle potential IO error
            .filter(|e| e.is_ok()).count();
         if other_entries_count <= 3 {
            formula_content_root = entry.path();
            debug!("Legacy unlink using intermediate dir: {}", formula_content_root.display());
         }
     }

    let formula_bin_dir = formula_content_root.join("bin");
    let formula_libexec_dir = formula_content_root.join("libexec");
    let mut unlinked_count = 0;

    if formula_bin_dir.exists() { unlinked_count += unlink_executables_from_dir(&formula_bin_dir, &bin_dir)?; }
    if formula_libexec_dir.exists() { unlinked_count += unlink_executables_from_dir(&formula_libexec_dir, &bin_dir)?; }

    if unlinked_count == 0 && !formula_bin_dir.exists() && !formula_libexec_dir.exists() {
        debug!("Legacy unlink: No bin or libexec directory found in {}", formula_content_root.display());
    } else if unlinked_count > 0 {
        info!("Successfully unlinked {} binaries for {} (legacy method).", unlinked_count, formula.name);
    } else {
        info!("No binaries found to unlink for {} (legacy method).", formula.name);
    }
    Ok(())
}


// Helper for legacy unlink to process one source directory
fn unlink_executables_from_dir(source_exec_dir: &Path, target_link_dir: &Path) -> Result<usize> {
    let mut unlinked_count = 0;
    match fs::read_dir(source_exec_dir) {
        Ok(entries) => {
            for entry_result in entries {
                match entry_result {
                    Ok(entry) => {
                        let source_path = entry.path();
                        match is_executable(&source_path) {
                            Ok(true) if source_path.is_file() => {
                                let file_name = entry.file_name();
                                let target_link = target_link_dir.join(file_name);
                                if target_link.exists() {
                                    match is_symlink_to(&target_link, &source_path) {
                                        Ok(true) => {
                                             match fs::remove_file(&target_link) {
                                                Ok(_) => { debug!("  Legacy unlinked {} -> {}", target_link.display(), source_path.display()); unlinked_count += 1; }
                                                 Err(e) => { warn!("Failed to remove legacy symlink {}: {}", target_link.display(), e); }
                                             }
                                        }
                                        Ok(false) => { debug!("Legacy unlink: {} exists but does not point to {}", target_link.display(), source_path.display()); }
                                        Err(e) => { warn!("Failed to check legacy symlink {}: {}", target_link.display(), e); }
                                    }
                                }
                            }
                            Ok(false) | Ok(_) => { /* Ignore */ }
                            Err(e) => { warn!("Could not check executable status for {}: {}", source_path.display(), e); }
                        }
                    }
                     Err(e) => { warn!("Failed to process directory entry in {}: {}", source_exec_dir.display(), e); }
                }
            }
        }
        Err(e) => { warn!("Failed to read source directory {} during legacy unlink: {}", source_exec_dir.display(), e); }
    }
    Ok(unlinked_count)
}


/// Get the standard Homebrew bin directory (internal helper)
pub(crate) fn get_bin_directory() -> PathBuf {
    get_homebrew_prefix().join("bin")
}

/// Check if a file is executable (internal helper)
fn is_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    if !path.is_file() { return Ok(false); }
    match fs::metadata(path) {
        Ok(metadata) => { Ok(metadata.permissions().mode() & 0o111 != 0) }
        Err(e) => { Err(SapphireError::Io(e)) }
    }
}

/// Check if a symlink points to a specific target (internal helper)
fn is_symlink_to(link: &Path, target: &Path) -> Result<bool> {
    match link.symlink_metadata() {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink() { return Ok(false); }
            match fs::read_link(link) {
                Ok(link_target_path) => {
                     match target.canonicalize() {
                        Ok(canonical_target) => {
                             let canonical_link_target = if link_target_path.is_absolute() { link_target_path.canonicalize() }
                             else { link.parent().unwrap_or_else(|| Path::new(".")).join(&link_target_path).canonicalize() };
                             match canonical_link_target {
                                Ok(clt) => Ok(clt == canonical_target),
                                Err(e) => { debug!("Could not canonicalize link target path {}: {}", link_target_path.display(), e); Ok(false) }
                             }
                        }
                        Err(e) => { debug!("Could not canonicalize expected target path {}: {}", target.display(), e); Ok(false) }
                     }
                }
                Err(e) => { warn!("Failed to read link target for {}: {}", link.display(), e); Ok(false) },
            }
        }
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound { warn!("Failed to get symlink metadata for {}: {}", link.display(), e); }
            Ok(false)
        },
    }
}