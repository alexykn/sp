// sapphire-core/src/build/formula/bottle.rs

// --- Imports ---
use super::macho;
use crate::fetch::{http, oci};
use crate::model::formula::{BottleFileSpec, Formula, FormulaDependencies};
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use log::{debug, error, warn};
use reqwest::Client;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;


// --- Bottle Functions ---

// download_bottle (unchanged)
pub async fn download_bottle(
    formula: &Formula,
    config: &Config,
    client: &Client,
) -> Result<PathBuf> {
    // (Implementation remains the same)
     debug!("Attempting to download bottle for {}", formula.name);

    let (platform_tag, bottle_file_spec) = get_bottle_for_platform(formula)?;
    debug!(
        "Selected bottle spec for platform '{}': URL={}, SHA256={}",
        platform_tag, bottle_file_spec.url, bottle_file_spec.sha256
    );
    if bottle_file_spec.url.is_empty() {
        return Err(SapphireError::DownloadError(
            formula.name.clone(),
            "".to_string(),
            "Bottle spec has an empty URL.".to_string(),
        ));
    }

    let standard_version_str = formula.version_str_full();
    let filename = format!(
        "{}-{}.{}.bottle.tar.gz",
        formula.name, standard_version_str, platform_tag
    );
    let cache_dir = config.cache_dir.join("bottles");
    fs::create_dir_all(&cache_dir).map_err(SapphireError::Io)?;
    let bottle_cache_path = cache_dir.join(&filename);

    if bottle_cache_path.is_file() {
        debug!("Bottle found in cache: {}", bottle_cache_path.display());
        if !bottle_file_spec.sha256.is_empty() {
            match http::verify_checksum(&bottle_cache_path, &bottle_file_spec.sha256) {
                Ok(_) => {
                    debug!("Using valid cached bottle: {}", bottle_cache_path.display());
                    return Ok(bottle_cache_path);
                }
                Err(e) => {
                    warn!(
                        "Cached bottle checksum mismatch ({}): {}. Redownloading.",
                        bottle_cache_path.display(),
                        e
                    );
                    if let Err(remove_err) = fs::remove_file(&bottle_cache_path) {
                        warn!(
                            "Failed to remove corrupted cached bottle {}: {}",
                            bottle_cache_path.display(),
                            remove_err
                        );
                    }
                }
            }
        } else {
            debug!(
                "Using cached bottle (checksum not specified): {}",
                bottle_cache_path.display()
            );
            return Ok(bottle_cache_path);
        }
    } else {
        debug!("Bottle not found in cache.");
    }

    let bottle_url_str = &bottle_file_spec.url;
    let registry_domain = config
        .artifact_domain
        .as_deref()
        .unwrap_or(oci::DEFAULT_GHCR_DOMAIN);
    let is_oci_blob_url = (bottle_url_str.contains("://ghcr.io/")
        || bottle_url_str.contains(registry_domain))
        && bottle_url_str.contains("/blobs/sha256:");
    debug!(
        "Checking URL type: '{}'. Is OCI Blob URL? {}",
        bottle_url_str, is_oci_blob_url
    );

    if is_oci_blob_url {
        debug!(
            "Detected OCI blob URL, initiating direct blob download: {}",
            bottle_url_str
        );
        match oci::download_oci_blob(bottle_url_str, &bottle_cache_path, config, client).await {
            Ok(_) => {
                debug!(
                    "Successfully downloaded OCI blob to {}",
                    bottle_cache_path.display()
                );
            }
            Err(e) => {
                error!("Failed to download OCI blob from {}: {}", bottle_url_str, e);
                let _ = fs::remove_file(&bottle_cache_path);
                return Err(SapphireError::DownloadError(
                    formula.name.clone(),
                    bottle_url_str.to_string(),
                    format!("Failed to download OCI blob: {}", e),
                ));
            }
        }
    } else {
        debug!(
            "Detected standard HTTPS URL, using direct download for: {}",
            bottle_url_str
        );
        match http::fetch_formula_source_or_bottle(
            formula.name(),
            bottle_url_str,
            &bottle_file_spec.sha256,
            &[],
            config,
        )
        .await
        {
            Ok(downloaded_path) => {
                if downloaded_path != bottle_cache_path {
                    warn!("Direct download saved to unexpected path: {}. Expected: {}. Attempting move.", downloaded_path.display(), bottle_cache_path.display());
                    if let Err(move_err) = fs::rename(&downloaded_path, &bottle_cache_path) {
                        error!(
                            "Failed to move downloaded file from {} to {}: {}",
                            downloaded_path.display(),
                            bottle_cache_path.display(),
                            move_err
                        );
                        return Err(SapphireError::Io(move_err));
                    }
                }
                debug!(
                    "Successfully downloaded directly to {}",
                    bottle_cache_path.display()
                );
            }
            Err(e) => {
                error!("Failed to download directly from {}: {}", bottle_url_str, e);
                let _ = fs::remove_file(&bottle_cache_path);
                return Err(SapphireError::DownloadError(
                    formula.name.clone(),
                    bottle_url_str.to_string(),
                    format!("Direct download failed: {}", e),
                ));
            }
        }
    }

    if !bottle_file_spec.sha256.is_empty() {
        http::verify_checksum(&bottle_cache_path, &bottle_file_spec.sha256)?;
    } else {
        warn!(
            "Skipping checksum verification for {} as none was provided in the spec.",
            formula.name
        );
    }

    debug!(
        "Bottle download successful: {}",
        bottle_cache_path.display()
    );
    Ok(bottle_cache_path)
}


// *** Make function crate-visible: `pub(crate) fn` ***
/// Gets the most specific available bottle spec for the current platform, including fallbacks.
pub(crate) fn get_bottle_for_platform(formula: &Formula) -> Result<(String, &BottleFileSpec)> {
    // (Implementation with hierarchical fallback remains the same)
    let stable_spec = formula.bottle.stable.as_ref().ok_or_else(|| {
        SapphireError::Generic(format!(
            "Formula '{}' has no stable bottle specification.",
            formula.name
        ))
    })?;

    if stable_spec.files.is_empty() {
        return Err(SapphireError::Generic(format!(
            "Formula '{}' has no bottle files listed in stable spec.",
            formula.name
        )));
    }

    const ARM_MACOS_VERSIONS: &[&str] = &["sequoia", "sonoma", "ventura", "monterey"];

    let current_platform = crate::build::formula::get_current_platform();
    if current_platform == "unknown" || current_platform.contains("unknown") {
        warn!("Could not reliably determine macOS platform. Bottle selection might be incorrect.");
    }
    debug!(
        "Determining bottle for current platform: {}",
        current_platform
    );
    debug!(
        "Available bottle platforms in formula spec: {:?}",
        stable_spec.files.keys().cloned().collect::<Vec<_>>()
    );

    // 1. Check for exact platform match
    if let Some(spec) = stable_spec.files.get(&current_platform) {
        debug!("Found exact bottle match for platform: {}", current_platform);
        return Ok((current_platform.clone(), spec));
    }
    debug!("No exact match found for {}", current_platform);

    // 2. Hierarchical OS fallback for ARM
    if current_platform.starts_with("arm64_") {
        if let Some(current_os_name) = current_platform.strip_prefix("arm64_") {
            if let Some(current_os_index) = ARM_MACOS_VERSIONS.iter().position(|&v| v == current_os_name) {
                 for i in current_os_index..ARM_MACOS_VERSIONS.len() {
                    let target_os_name = ARM_MACOS_VERSIONS[i];
                    let target_tag = format!("arm64_{}", target_os_name);
                    if let Some(spec) = stable_spec.files.get(&target_tag) {
                         warn!(
                            "No bottle found for exact platform '{}'. Using compatible older bottle '{}'.",
                             current_platform, target_tag
                         );
                         return Ok((target_tag, spec));
                    }
                 }
                 debug!("Checked older ARM macOS versions ({:?}), no suitable bottle found.", &ARM_MACOS_VERSIONS[current_os_index..]);
            } else {
                 debug!("Current OS '{}' not found in known ARM macOS version list.", current_os_name);
            }
        } else {
             debug!("Could not extract OS name from ARM platform tag '{}'", current_platform);
        }

        // 3. Check for generic "arm64" tag
        if let Some(spec) = stable_spec.files.get("arm64") {
            warn!(
                "No specific OS bottle found for {}. Falling back to generic 'arm64' bottle.",
                current_platform
            );
            return Ok(("arm64".to_string(), spec));
        }
        debug!("No generic 'arm64' bottle tag found.");
    }

    // 4. Fallback for Intel macOS or general OS name match
    if let Some(os_name) = current_platform.split('_').last() {
         if os_name != current_platform {
             if let Some(spec) = stable_spec.files.get(os_name) {
                 warn!(
                     "No architecture-specific bottle found for {}. Falling back to OS-only tag '{}' bottle.",
                     current_platform, os_name
                 );
                 return Ok((os_name.to_string(), spec));
             }
              debug!("No fallback bottle found for OS tag: {}", os_name);
         }
    }

    // 5. Fallback to "all" platform
    if let Some(spec) = stable_spec.files.get("all") {
        warn!(
            "No platform-specific or OS-specific bottle found for {}. Using 'all' platform bottle.",
            current_platform
        );
        return Ok(("all".to_string(), spec));
    }
    debug!("No 'all' platform bottle found.");

    // 6. If no suitable bottle found
    Err(SapphireError::DownloadError(
        formula.name.clone(),
        "".to_string(),
        format!(
            "No compatible bottle found for platform '{}'. Available: {:?}",
            current_platform,
            stable_spec.files.keys().collect::<Vec<_>>()
        ),
    ))
}


// install_bottle (unchanged)
pub fn install_bottle(bottle_path: &Path, formula: &Formula, config: &Config) -> Result<PathBuf> {
    // (Implementation remains the same)
    let install_dir = match formula.install_prefix(&config.cellar) {
        Ok(path) => path,
        Err(e) => {
            return Err(SapphireError::InstallError(format!(
                "Could not determine install path for {}: {}",
                formula.name(),
                e
            )))
        }
    };
    if install_dir.exists() {
        debug!(
            "Removing existing keg directory before installing: {}",
            install_dir.display()
        );
        fs::remove_dir_all(&install_dir).map_err(|e| {
            SapphireError::InstallError(format!(
                "Failed to remove existing keg {}: {}",
                install_dir.display(),
                e
            ))
        })?;
    }
    if let Some(parent_dir) = install_dir.parent() {
        fs::create_dir_all(parent_dir).map_err(|e| {
            SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to create parent dir {}: {}",
                    parent_dir.display(),
                    e
                ),
            ))
        })?;
    } else {
        return Err(SapphireError::InstallError(format!(
            "Could not determine parent directory for install path: {}",
            install_dir.display()
        )));
    }
    fs::create_dir_all(&install_dir).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to create keg dir {}: {}", install_dir.display(), e),
        ))
    })?;

    crate::build::extract_archive_strip_components(bottle_path, &install_dir, 2)?;
    debug!(
        "==> Ensuring write permissions for extracted files in {}",
        install_dir.display()
    );
    ensure_write_permissions(&install_dir)?; // sync
    debug!(
        "==> Performing bottle relocation in {}",
        install_dir.display()
    );
    perform_bottle_relocation(formula, &install_dir, config)?; // sync call
    crate::build::write_receipt(formula, &install_dir)?; // sync

    debug!(
        "Bottle installation complete for {} at {}",
        formula.name(),
        install_dir.display()
    );
    Ok(install_dir)
}

// --- Permissions and Placeholder Relocation (unchanged) ---
fn ensure_write_permissions(path: &Path) -> Result<()> {
    // (Implementation remains the same)
    for entry_result in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        let entry_path = entry_result.path();
        match fs::metadata(entry_path) {
            Ok(metadata) => {
                let mut perms = metadata.permissions();
                #[cfg(unix)]
                {
                    let current_mode = perms.mode();
                    let new_mode = current_mode | 0o200;
                    if new_mode != current_mode {
                        perms.set_mode(new_mode);
                        if let Err(e) = fs::set_permissions(entry_path, perms) {
                            warn!("Failed to set write permission on {}: {}", entry_path.display(), e);
                        } else {
                            debug!("Set write permission on: {}", entry_path.display());
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    if perms.readonly() {
                        perms.set_readonly(false);
                        if let Err(e) = fs::set_permissions(entry_path, perms) {
                            warn!("Failed to unset readonly attribute on {}: {}", entry_path.display(), e);
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to get metadata for {}: {}", entry_path.display(), e);
            }
        }
    }
    Ok(())
}

fn perform_bottle_relocation(
    _formula: &Formula,
    install_dir: &Path,
    config: &Config,
) -> Result<()> {
    // Build up all the placeholder→real‐path mappings as before...
    let mut replacements = HashMap::new();
    let cellar_path_str = config.cellar.to_string_lossy().to_string();
    let prefix_path_str = config.prefix.to_string_lossy().to_string();
    let repo_path = config.prefix.join("Library/Taps/homebrew/homebrew-core");
    let repo_path_str = repo_path.to_string_lossy().to_string();
    let library_path = config.prefix.join("Library");
    let library_path_str = library_path.to_string_lossy().to_string();

    replacements.insert("@@HOMEBREW_CELLAR@@".to_string(), cellar_path_str.clone());
    replacements.insert("@@HOMEBREW_PREFIX@@".to_string(), prefix_path_str.clone());
    replacements.insert("@@HOMEBREW_REPOSITORY@@".to_string(), repo_path_str);
    replacements.insert("@@HOMEBREW_LIBRARY@@".to_string(), library_path_str);

    // … plus perl, opt, resource placeholders … (unchanged) …

    debug!("Starting file scan for text replacement and Mach-O patching in: {}", install_dir.display());
    for (placeholder, replacement) in &replacements {
        debug!("  Will replace '{}' with '{}'", placeholder, replacement);
    }

    let mut text_replaced_count = 0;
    let mut macho_patched_count = 0;
    let mut permission_errors = 0;
    let mut macho_errors = 0;
    let mut files_to_chmod: Vec<PathBuf> = Vec::new();

    for entry in WalkDir::new(install_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        debug!("  Scanning file: {}", path.display());

        // Determine if it *looks* like something that ought to be +x by convention:
        let is_potential_executable = path
            .components()
            .any(|c| c.as_os_str() == "bin" || c.as_os_str() == "sbin");

        let meta = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) => { warn!("Could not stat {}: {}", path.display(), e); continue; }
        };
        if meta.permissions().readonly() {
            debug!("  Skipping readonly file: {}", path.display());
            continue;
        }

        let mut was_modified = false;

        // --- Mach‑O patching branch ---
        if cfg!(target_os = "macos") {
            match macho::patch_macho_file(path, &replacements) {
                Ok(patched) if patched => {
                    debug!("Successfully patched Mach-O file: {}", path.display());
                    macho_patched_count += 1;
                    was_modified = true;
                    // **Always** restore exec perms on *any* patched Mach-O
                    files_to_chmod.push(path.to_path_buf());
                }
                Ok(_) => {}
                Err(SapphireError::PathTooLongError(e)) => {
                    error!("Mach-O patch failed (too long) for {}: {}", path.display(), e);
                    macho_errors += 1;
                    continue;
                }
                Err(SapphireError::CodesignError(e)) => {
                    error!("Mach-O patch failed (codesign) for {}: {}", path.display(), e);
                    macho_errors += 1;
                    continue;
                }
                Err(e) => {
                    warn!("Mach-O check failed for {}: {}. Falling back to text replacer.", path.display(), e);
                }
            }
        }

        // --- Text placeholder replacement branch ---
        if !was_modified {
            // Your existing “is this text?” heuristic...
            let mut is_text = false;
            if let Ok(mut f) = File::open(path) {
                let mut buf = [0; 1024];
                if let Ok(n) = f.read(&mut buf) {
                    if n == 0 || !buf[..n].contains(&0) {
                        is_text = true;
                    }
                }
            }
            if is_text {
                if let Ok(content) = fs::read_to_string(path) {
                    let mut new = content.clone();
                    let mut did = false;
                    for (ph, rep) in &replacements {
                        if new.contains(ph) {
                            new = new.replace(ph, rep);
                            did = true;
                            debug!("  Replaced '{}' in {}", ph, path.display());
                        }
                    }
                    if did {
                        if write_text_file_atomic(path, &new).is_ok() {
                            text_replaced_count += 1;
                            // restore +x if it was in bin/sbin
                            if is_potential_executable {
                                files_to_chmod.push(path.to_path_buf());
                            }
                        }
                    }
                }
            } else {
                // Non‑text, non‑Mach‑O binaries still might need +x
                if is_potential_executable {
                    files_to_chmod.push(path.to_path_buf());
                }
            }
        }
    }

    // Finally, restore +x on every file we recorded
    #[cfg(unix)]
    {
        debug!("Ensuring execute permissions for {} files...", files_to_chmod.len());
        for p in &files_to_chmod {
            match fs::metadata(p) {
                Ok(m) => {
                    let mut perms = m.permissions();
                    let new_mode = perms.mode() | 0o111;
                    if new_mode != perms.mode() {
                        perms.set_mode(new_mode);
                        if let Err(e) = fs::set_permissions(p, perms) {
                            warn!("Failed to set +x on {}: {}", p.display(), e);
                            permission_errors += 1;
                        }
                    }
                }
                Err(e) => {
                    warn!("Could not stat {} during chmod: {}", p.display(), e);
                    permission_errors += 1;
                }
            }
        }
    }

    debug!(
        "Relocation complete. Text files: {}, Mach-O files: {}",
        text_replaced_count, macho_patched_count
    );
    if permission_errors > 0 || macho_errors > 0 {
        error!(
            "Encountered {} chmod errors and {} Mach-O errors. Installation may be broken.",
            permission_errors, macho_errors
        );
        return Err(SapphireError::InstallError(format!(
            "Bottle relocation failed: {} chmod errors, {} Mach-O errors in {}",
            permission_errors, macho_errors, install_dir.display()
        )));
    }
    Ok(())
}


// write_text_file_atomic (unchanged)
fn write_text_file_atomic(original_path: &Path, content: &str) -> Result<()> {
    // (Implementation remains the same)
    use std::io::Write;
     use tempfile::NamedTempFile;

     let dir = original_path.parent().ok_or_else(|| {
         SapphireError::Generic(format!(
             "Cannot get parent directory for {}",
             original_path.display()
         ))
     })?;
     fs::create_dir_all(dir)?;

     let mut temp_file = NamedTempFile::new_in(dir)?;
     debug!(
         "    Writing relocated text to temporary file: {:?}",
         temp_file.path()
     );
     temp_file.write_all(content.as_bytes())?;
     temp_file.flush()?;
     temp_file.as_file().sync_all()?;

     temp_file.persist(original_path).map_err(|e| {
         error!(
             "    Failed to persist/rename temporary text file over {}: {}",
             original_path.display(),
             e.error
         );
         SapphireError::Io(e.error)
     })?;
     debug!(
         "    Atomically replaced {} with relocated text version",
         original_path.display()
     );
     Ok(())
}