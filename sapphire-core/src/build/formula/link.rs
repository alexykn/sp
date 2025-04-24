// ===== sapphire-core/src/build/formula/link.rs =====
use std::fs;
use std::io::Write;
use std::os::unix::fs as unix_fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json;
use tracing::{debug, error};

use crate::model::formula::Formula;
use crate::utils::config::Config; // Import Config
use crate::utils::error::{Result, SapphireError};

const STANDARD_KEG_DIRS: [&str; 6] = ["bin", "lib", "share", "include", "etc", "Frameworks"];

/// Link all artifacts from a formula's installation directory.
// Added Config parameter
pub fn link_formula_artifacts(
    formula: &Formula,
    installed_keg_path: &Path,
    config: &Config, // Added config
) -> Result<()> {
    debug!(
        "Linking artifacts for {} from {}",
        formula.name(),
        installed_keg_path.display()
    );

    let formula_content_root = determine_content_root(installed_keg_path)?;
    let mut symlinks_created = Vec::<String>::new();

    // Use config methods for paths
    let opt_link_path = config.formula_opt_link_path(formula.name());
    let target_keg_dir = &formula_content_root;

    remove_existing_link_target(&opt_link_path)?;
    unix_fs::symlink(target_keg_dir, &opt_link_path).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to create opt symlink for {}: {}", formula.name(), e),
        ))
    })?;
    symlinks_created.push(opt_link_path.to_string_lossy().to_string());
    debug!(
        "  Linked opt path: {} -> {}",
        opt_link_path.display(),
        target_keg_dir.display()
    );

    if let Some((base, _version)) = formula.name().split_once('@') {
        let alias_path = config.opt_dir().join(base); // Use config.opt_dir()
        if !alias_path.exists() {
            match unix_fs::symlink(target_keg_dir, &alias_path) {
                Ok(_) => {
                    debug!(
                        "  Added un‑versioned opt alias: {} -> {}",
                        alias_path.display(),
                        target_keg_dir.display()
                    );
                    symlinks_created.push(alias_path.to_string_lossy().to_string());
                }
                Err(e) => {
                    debug!(
                        "  Could not create opt alias {}: {}",
                        alias_path.display(),
                        e
                    );
                }
            }
        }
    }

    let standard_artifact_dirs = ["lib", "include", "share"];
    for dir_name in &standard_artifact_dirs {
        let source_subdir = formula_content_root.join(dir_name);
        // Use config.prefix() for target base
        let target_prefix_subdir = config.prefix().join(dir_name);

        if source_subdir.is_dir() {
            fs::create_dir_all(&target_prefix_subdir)?;
            for entry in fs::read_dir(&source_subdir)? {
                let entry = entry?;
                let source_item_path = entry.path();
                let file_name = entry.file_name();
                if file_name.to_string_lossy().starts_with('.') {
                    continue;
                }

                let target_link = target_prefix_subdir.join(&file_name);
                remove_existing_link_target(&target_link)?;
                unix_fs::symlink(&source_item_path, &target_link).ok(); // ignore errors for individual links?
                symlinks_created.push(target_link.to_string_lossy().to_string());
                debug!(
                    "  Linked {} -> {}",
                    target_link.display(),
                    source_item_path.display()
                );
            }
        }
    }

    // Use config.bin_dir() for target bin
    let target_bin_dir = config.bin_dir();
    fs::create_dir_all(&target_bin_dir).ok();

    let source_bin_dir = formula_content_root.join("bin");
    if source_bin_dir.is_dir() {
        create_wrappers_in_dir(
            &source_bin_dir,
            &target_bin_dir,
            &formula_content_root,
            &mut symlinks_created,
        )?;
    }
    let source_libexec_dir = formula_content_root.join("libexec");
    if source_libexec_dir.is_dir() {
        create_wrappers_in_dir(
            &source_libexec_dir,
            &target_bin_dir,
            &formula_content_root,
            &mut symlinks_created,
        )?;
    }

    write_install_manifest(installed_keg_path, &symlinks_created)?;

    debug!(
        "Successfully completed linking artifacts for {}",
        formula.name()
    );
    Ok(())
}

// remove_existing_link_target, write_install_manifest remain mostly unchanged internally) ...
fn create_wrappers_in_dir(
    source_dir: &Path,
    target_bin_dir: &Path,
    formula_content_root: &Path,
    wrappers_created: &mut Vec<String>,
) -> Result<()> {
    debug!(
        "Scanning for executables in {} to create wrappers in {}",
        source_dir.display(),
        target_bin_dir.display()
    );
    match fs::read_dir(source_dir) {
        Ok(entries) => {
            for entry_result in entries {
                match entry_result {
                    Ok(entry) => {
                        let source_item_path = entry.path();
                        let file_name = entry.file_name();

                        if file_name.to_string_lossy().starts_with('.') {
                            continue;
                        }

                        if source_item_path.is_dir() {
                            create_wrappers_in_dir(
                                &source_item_path,
                                target_bin_dir,
                                formula_content_root,
                                wrappers_created,
                            )?;
                        } else if source_item_path.is_file() {
                            match is_executable(&source_item_path) {
                                Ok(true) => {
                                    let wrapper_path = target_bin_dir.join(&file_name);
                                    debug!("  Found executable: {}", source_item_path.display());
                                    if remove_existing_link_target(&wrapper_path).is_ok() {
                                        debug!(
                                            "    Creating wrapper script: {} -> {}",
                                            wrapper_path.display(),
                                            source_item_path.display()
                                        );
                                        match create_wrapper_script(
                                            &source_item_path,
                                            &wrapper_path,
                                            formula_content_root,
                                        ) {
                                            Ok(_) => {
                                                debug!(
                                                    "  Created wrapper {} -> {}",
                                                    wrapper_path.display(),
                                                    source_item_path.display()
                                                );
                                                wrappers_created.push(
                                                    wrapper_path.to_string_lossy().to_string(),
                                                );
                                            }
                                            Err(e) => {
                                                error!("    Failed to create wrapper script {} -> {}: {}", wrapper_path.display(), source_item_path.display(), e);
                                            }
                                        }
                                    }
                                }
                                Ok(false) => { /* Not executable, ignore */ }
                                Err(e) => {
                                    debug!(
                                        "    Could not check executable status for {}: {}",
                                        source_item_path.display(),
                                        e
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        debug!(
                            "  Failed to process directory entry in {}: {}",
                            source_dir.display(),
                            e
                        );
                    }
                }
            }
        }
        Err(e) => {
            debug!(
                "Failed to read source directory {}: {}",
                source_dir.display(),
                e
            );
        }
    }
    Ok(())
}
fn create_wrapper_script(
    target_executable: &Path,
    wrapper_path: &Path,
    formula_content_root: &Path,
) -> Result<()> {
    let libexec_path = formula_content_root.join("libexec");
    let perl_lib_path = libexec_path.join("lib").join("perl5");
    let python_lib_path = libexec_path.join("vendor"); // Assuming simple vendor dir

    let mut script_content = String::new();
    script_content.push_str("#!/bin/bash\n");
    script_content.push_str("# Wrapper script generated by Sapphire\n");
    script_content.push_str("set -e\n\n");

    if perl_lib_path.exists() && perl_lib_path.is_dir() {
        script_content.push_str(&format!(
            "export PERL5LIB=\"{}:$PERL5LIB\"\n",
            perl_lib_path.display()
        ));
        debug!(
            "  (Wrapper will set PERL5LIB to {})",
            perl_lib_path.display()
        );
    }
    if python_lib_path.exists() && python_lib_path.is_dir() {
        script_content.push_str(&format!(
            "export PYTHONPATH=\"{}:$PYTHONPATH\"\n",
            python_lib_path.display()
        ));
        debug!(
            "  (Wrapper will set PYTHONPATH to {})",
            python_lib_path.display()
        );
    }

    script_content.push_str(&format!(
        "\nexec \"{}\" \"$@\"\n",
        target_executable.display()
    ));

    let mut file = fs::File::create(wrapper_path).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed create wrapper {}: {}", wrapper_path.display(), e),
        ))
    })?;
    file.write_all(script_content.as_bytes()).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed write wrapper {}: {}", wrapper_path.display(), e),
        ))
    })?;

    #[cfg(unix)]
    {
        let metadata = file.metadata()?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(wrapper_path, permissions).map_err(|e| {
            SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed set wrapper executable {}: {}",
                    wrapper_path.display(),
                    e
                ),
            ))
        })?;
    }

    Ok(())
}

fn determine_content_root(installed_keg_path: &Path) -> Result<PathBuf> {
    let mut potential_subdirs = Vec::new();
    let mut top_level_files_found = false;
    if !installed_keg_path.is_dir() {
        error!(
            "Keg path {} does not exist or is not a directory!",
            installed_keg_path.display()
        );
        return Err(SapphireError::NotFound(format!(
            "Keg path not found: {}",
            installed_keg_path.display()
        )));
    }
    match fs::read_dir(installed_keg_path) {
        Ok(entries) => {
            for entry_res in entries {
                if let Ok(entry) = entry_res {
                    let path = entry.path();
                    let file_name = entry.file_name();
                    // --- Use OsStr comparison ---
                    let file_name_osstr = file_name.as_os_str();
                    if file_name_osstr.to_string_lossy().starts_with('.')
                        || file_name_osstr == "INSTALL_MANIFEST.json"
                        || file_name_osstr == "INSTALL_RECEIPT.json"
                    {
                        continue;
                    }
                    if path.is_dir() {
                        // Store both path and name for check later
                        potential_subdirs.push((path, file_name.to_string_lossy().to_string()));
                    } else if path.is_file() {
                        top_level_files_found = true;
                        debug!(
                            "Found file '{}' at top level of keg {}, assuming no intermediate dir.",
                            file_name.to_string_lossy(), // Use lossy for display
                            installed_keg_path.display()
                        );
                        break; // Stop scanning if top-level files found
                    }
                } else {
                    debug!(
                        "Failed to read directory entry in {}: {}",
                        installed_keg_path.display(),
                        entry_res.err().unwrap() // Safe unwrap as we are in Err path
                    );
                }
            }
        }
        Err(e) => {
            debug!("Could not read keg directory {} to check for intermediate dir: {}. Assuming keg path is content root.", installed_keg_path.display(), e);
            return Ok(installed_keg_path.to_path_buf());
        }
    }

    // --- MODIFIED LOGIC ---
    if potential_subdirs.len() == 1 && !top_level_files_found {
        // Get the single subdirectory path and name
        let (intermediate_dir_path, intermediate_dir_name) = potential_subdirs.remove(0); // Use remove

        // Check if the single directory name is one of the standard install dirs
        if STANDARD_KEG_DIRS.contains(&intermediate_dir_name.as_str()) {
            debug!(
                "Single directory found ('{}') is a standard directory. Using main keg directory {} as content root.",
                intermediate_dir_name,
                installed_keg_path.display()
            );
            Ok(installed_keg_path.to_path_buf()) // Use main keg path
        } else {
            // Single dir is NOT a standard name, assume it's an intermediate content root
            debug!(
                "Detected single non-standard intermediate content directory: {}",
                intermediate_dir_path.display()
            );
            Ok(intermediate_dir_path) // Use the intermediate dir
        }
    // --- END MODIFIED LOGIC ---
    } else {
        // Handle multiple subdirs or top-level files found case (no change needed here)
        if potential_subdirs.len() > 1 {
            debug!("Multiple potential content directories found under keg {}. Using main keg directory as content root.", installed_keg_path.display());
        } else if top_level_files_found {
            debug!(
                "Top-level files found in keg {}. Using main keg directory as content root.",
                installed_keg_path.display()
            );
        } else if potential_subdirs.is_empty() {
            // Changed from else if to else
            debug!("No subdirectories or files found (excluding ignored ones) in keg {}. Using main keg directory as content root.", installed_keg_path.display());
        }
        Ok(installed_keg_path.to_path_buf()) // Use main keg path in these cases too
    }
}

fn remove_existing_link_target(path: &Path) -> Result<()> {
    match path.symlink_metadata() {
        Ok(metadata) => {
            debug!(
                "    Removing existing item at link target: {}",
                path.display()
            );
            let is_dir = metadata.file_type().is_dir();
            let is_symlink = metadata.file_type().is_symlink();
            let is_real_dir = is_dir && !is_symlink;
            let remove_result = if is_real_dir {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
            if let Err(e) = remove_result {
                debug!(
                    "    Failed to remove existing item at link target {}: {}",
                    path.display(),
                    e
                );
                return Err(SapphireError::Io(e));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            debug!(
                "    Failed to get metadata for existing item {}: {}",
                path.display(),
                e
            );
            Err(SapphireError::Io(e))
        }
    }
}

fn write_install_manifest(installed_keg_path: &Path, symlinks_created: &[String]) -> Result<()> {
    let manifest_path = installed_keg_path.join("INSTALL_MANIFEST.json");
    debug!("Writing install manifest to: {}", manifest_path.display());
    match serde_json::to_string_pretty(&symlinks_created) {
        Ok(manifest_json) => match fs::write(&manifest_path, manifest_json) {
            Ok(_) => {
                debug!(
                    "Wrote install manifest with {} links: {}",
                    symlinks_created.len(),
                    manifest_path.display()
                );
            }
            Err(e) => {
                error!(
                    "Failed to write install manifest {}: {}",
                    manifest_path.display(),
                    e
                );
                return Err(SapphireError::Io(e));
            }
        },
        Err(e) => {
            error!("Failed to serialize install manifest data: {}", e);
            return Err(SapphireError::Json(e));
        }
    }
    Ok(())
}

/// Unlink artifacts based on the manifest file.
// Added Config parameter
pub fn unlink_formula_artifacts(formula: &Formula, config: &Config) -> Result<()> {
    debug!("Unlinking artifacts for {}", formula.name());
    // Use config method to get expected keg path
    let expected_keg_path = config.formula_keg_path(formula.name(), &formula.version_str_full());
    let manifest_path = expected_keg_path.join("INSTALL_MANIFEST.json");

    if manifest_path.is_file() {
        debug!("Reading install manifest: {}", manifest_path.display());
        match fs::read_to_string(&manifest_path) {
            Ok(manifest_str) => {
                match serde_json::from_str::<Vec<String>>(&manifest_str) {
                    Ok(links_to_remove) => {
                        let mut unlinked_count = 0;
                        let mut removal_errors = 0;
                        if links_to_remove.is_empty() {
                            debug!("Install manifest {} is empty. Cannot perform manifest-based unlink.", manifest_path.display());
                            debug!("No links recorded in manifest, unlink complete.");
                            Ok(())
                        } else {
                            // Use Config to get base paths for checking
                            let opt_base = config.opt_dir();
                            let bin_base = config.bin_dir();
                            let lib_base = config.prefix().join("lib");
                            let include_base = config.prefix().join("include");
                            let share_base = config.prefix().join("share");

                            for link_str in links_to_remove {
                                let link_path = PathBuf::from(link_str);
                                match link_path.symlink_metadata() {
                                    Ok(_) => {
                                        // Check if it's under a managed directory
                                        if link_path.starts_with(&opt_base)
                                            || link_path.starts_with(&bin_base)
                                            || link_path.starts_with(&lib_base)
                                            || link_path.starts_with(&include_base)
                                            || link_path.starts_with(&share_base)
                                        {
                                            match fs::remove_file(&link_path) {
                                                Ok(_) => {
                                                    debug!(
                                                        "Removed link/wrapper: {}",
                                                        link_path.display()
                                                    );
                                                    unlinked_count += 1;
                                                }
                                                Err(e) => {
                                                    debug!(
                                                        "Failed to remove link/wrapper {}: {}",
                                                        link_path.display(),
                                                        e
                                                    );
                                                    removal_errors += 1;
                                                }
                                            }
                                        } else {
                                            debug!(
                                                "Manifest contains unexpected link path: {}",
                                                link_path.display()
                                            );
                                        }
                                    }
                                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                        debug!(
                                            "Link listed in manifest not found: {}",
                                            link_path.display()
                                        );
                                    }
                                    Err(e) => {
                                        debug!(
                                            "Failed to get metadata for link {}: {}",
                                            link_path.display(),
                                            e
                                        );
                                        removal_errors += 1;
                                    }
                                }
                            }
                            debug!(
                                "Successfully unlinked {} artifacts based on manifest.",
                                unlinked_count
                            );
                            if removal_errors > 0 {
                                debug!("Encountered {} errors while removing links listed in manifest.", removal_errors);
                            }
                            Ok(())
                        }
                    }
                    Err(e) => {
                        error!("Failed to parse formula install manifest {}: {}. Falling back to legacy unlink...", manifest_path.display(), e);
                        unlink_formula_binaries_legacy(formula, &expected_keg_path, config)
                        // Pass config
                    }
                }
            }
            Err(e) => {
                error!("Failed to read formula install manifest {}: {}. Falling back to legacy unlink...", manifest_path.display(), e);
                unlink_formula_binaries_legacy(formula, &expected_keg_path, config)
                // Pass config
            }
        }
    } else {
        debug!(
            "Warning: No install manifest found at {}. Falling back to legacy unlink...",
            manifest_path.display()
        );
        unlink_formula_binaries_legacy(formula, &expected_keg_path, config)
        // Pass config
    }
}

// Legacy unlink needs Config now
fn unlink_formula_binaries_legacy(
    formula: &Formula,
    expected_keg_path: &Path,
    config: &Config,
) -> Result<()> {
    debug!("Using legacy unlink for {}", formula.name());
    // Use config method
    let bin_dir = config.bin_dir();
    if !bin_dir.exists() {
        debug!(
            "Target bin directory {} does not exist, nothing to do for legacy unlink.",
            bin_dir.display()
        );
        return Ok(());
    }
    if !expected_keg_path.is_dir() {
        debug!(
            "Expected keg path {} does not exist. Cannot perform legacy unlink.",
            expected_keg_path.display()
        );
        // Use config method
        let opt_link_path = config.formula_opt_link_path(formula.name());
        if opt_link_path.symlink_metadata().is_ok() {
            debug!(
                "Attempting to remove opt link {} even though keg is missing.",
                opt_link_path.display()
            );
            if let Err(e) = fs::remove_file(&opt_link_path) {
                debug!(
                    "Failed to remove legacy opt symlink {}: {}",
                    opt_link_path.display(),
                    e
                );
            } else {
                debug!("Removed legacy opt symlink: {}", opt_link_path.display());
            }
        }
        return Ok(());
    }
    let formula_content_root = match determine_content_root(expected_keg_path) {
        Ok(root) => root,
        Err(_) => {
            debug!(
                "Could not determine content root for legacy unlink of {}. Assuming top level.",
                expected_keg_path.display()
            );
            expected_keg_path.to_path_buf()
        }
    };
    debug!(
        "Legacy unlink using content root: {}",
        formula_content_root.display()
    );
    let formula_bin_dir = formula_content_root.join("bin");
    let formula_libexec_dir = formula_content_root.join("libexec");
    let mut unlinked_count = 0;
    let mut unlink_errors = 0;
    if formula_bin_dir.is_dir() {
        match unlink_executables_from_dir(&formula_bin_dir, &bin_dir) {
            Ok(count) => unlinked_count += count,
            Err(_) => unlink_errors += 1,
        }
    }
    if formula_libexec_dir.is_dir() {
        match unlink_executables_from_dir(&formula_libexec_dir, &bin_dir) {
            Ok(count) => unlinked_count += count,
            Err(_) => unlink_errors += 1,
        }
    }
    // Use config method
    let opt_link_path = config.formula_opt_link_path(formula.name());
    match is_symlink_to(&opt_link_path, expected_keg_path) {
        Ok(true) => {
            if let Err(e) = fs::remove_file(&opt_link_path) {
                debug!(
                    "Failed to remove legacy opt symlink {}: {}",
                    opt_link_path.display(),
                    e
                );
                unlink_errors += 1;
            } else {
                debug!("Removed legacy opt symlink: {}", opt_link_path.display());
            }
        }
        Ok(false) => debug!(
            "Legacy unlink: Opt link {} exists but doesn't point to expected keg {}.",
            opt_link_path.display(),
            expected_keg_path.display()
        ),
        Err(e) => {
            debug!(
                "Failed to check legacy opt symlink {}: {}",
                opt_link_path.display(),
                e
            );
            unlink_errors += 1;
        }
    }
    if unlinked_count == 0
        && unlink_errors == 0
        && !formula_bin_dir.exists()
        && !formula_libexec_dir.exists()
    {
        debug!(
            "Legacy unlink: No bin or libexec directory found in {} and opt link not removed.",
            formula_content_root.display()
        );
    } else if unlinked_count > 0 || unlink_errors == 0 {
        debug!(
            "Successfully unlinked {} binaries for {} (legacy method).",
            unlinked_count,
            formula.name()
        );
        if unlink_errors > 0 {
            debug!("Encountered {} errors during legacy unlink.", unlink_errors);
        }
    } else {
        error!(
            "Legacy unlink failed for {}. Encountered {} errors and removed 0 links.",
            formula.name(),
            unlink_errors
        );
        return Err(SapphireError::Generic(format!(
            "Legacy unlink failed for {}",
            formula.name()
        )));
    }
    Ok(())
}

// ... (unlink_executables_from_dir, is_executable, is_symlink_to remain unchanged) ...
fn unlink_executables_from_dir(source_exec_dir: &Path, target_link_dir: &Path) -> Result<usize> {
    let mut unlinked_count = 0;
    if !source_exec_dir.is_dir() {
        return Ok(0);
    }
    match fs::read_dir(source_exec_dir) {
        Ok(entries) => {
            for entry_result in entries {
                match entry_result {
                    Ok(entry) => {
                        let source_path = entry.path();
                        if source_path.is_dir() {
                            unlinked_count +=
                                unlink_executables_from_dir(&source_path, target_link_dir)?;
                        } else if source_path.is_file() {
                            match is_executable(&source_path) {
                                Ok(true) => {
                                    let file_name = entry.file_name();
                                    let target_link = target_link_dir.join(file_name);
                                    match is_symlink_to(&target_link, &source_path) {
                                        Ok(true) => match fs::remove_file(&target_link) {
                                            Ok(_) => {
                                                debug!(
                                                    "  Legacy unlinked {} -> {}",
                                                    target_link.display(),
                                                    source_path.display()
                                                );
                                                unlinked_count += 1;
                                            }
                                            Err(e) => {
                                                debug!(
                                                    "Failed to remove legacy symlink {}: {}",
                                                    target_link.display(),
                                                    e
                                                );
                                            }
                                        },
                                        Ok(false) => {}
                                        Err(e) => {
                                            debug!(
                                                "Failed to check legacy symlink {}: {}",
                                                target_link.display(),
                                                e
                                            );
                                        }
                                    }
                                }
                                Ok(false) => {}
                                Err(e) => {
                                    debug!(
                                        "Could not check executable status for {}: {}",
                                        source_path.display(),
                                        e
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        debug!(
                            "Failed to process directory entry in {}: {}",
                            source_exec_dir.display(),
                            e
                        );
                    }
                }
            }
        }
        Err(e) => {
            debug!(
                "Failed to read source directory {} during legacy unlink: {}",
                source_exec_dir.display(),
                e
            );
            return Err(SapphireError::Io(e));
        }
    }
    Ok(unlinked_count)
}
fn is_executable(path: &Path) -> Result<bool> {
    if !path.try_exists().unwrap_or(false) || !path.is_file() {
        return Ok(false);
    }
    if cfg!(unix) {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(path) {
            Ok(metadata) => Ok(metadata.permissions().mode() & 0o111 != 0),
            Err(e) => Err(SapphireError::Io(e)),
        }
    } else {
        Ok(true)
    }
}
fn is_symlink_to(link: &Path, target: &Path) -> Result<bool> {
    match link.symlink_metadata() {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink() {
                return Ok(false);
            }
            match fs::read_link(link) {
                Ok(link_target_path) => match (target.canonicalize(), link.parent()) {
                    (Ok(canonical_target), Some(link_parent)) => {
                        let resolved_link_target = if link_target_path.is_absolute() {
                            link_target_path
                        } else {
                            link_parent.join(&link_target_path)
                        };
                        match resolved_link_target.canonicalize() {
                            Ok(canonical_link_target) => {
                                Ok(canonical_link_target == canonical_target)
                            }
                            Err(e) => {
                                debug!("Could not canonicalize link target path {} (from link {}): {}. Comparing raw paths.", resolved_link_target.display(), link.display(), e);
                                Ok(resolved_link_target == canonical_target)
                            }
                        }
                    }
                    (Err(e), _) => {
                        debug!(
                            "Could not canonicalize expected target path {}: {}",
                            target.display(),
                            e
                        );
                        Ok(false)
                    }
                    (_, None) => {
                        debug!("Could not get parent directory for link {}", link.display());
                        Ok(false)
                    }
                },
                Err(e) => {
                    debug!("Failed to read link target for {}: {}", link.display(), e);
                    Err(SapphireError::Io(e))
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => {
            debug!(
                "Failed to get symlink metadata for {}: {}",
                link.display(),
                e
            );
            Err(SapphireError::Io(e))
        }
    }
}
