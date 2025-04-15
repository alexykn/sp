// sapphire-core/src/build/formula/bottle.rs

// --- Imports ---
use super::macho;
use crate::fetch::{http, oci};
use crate::model::formula::{BottleFileSpec, Formula, FormulaDependencies};
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use log::{debug, error, info, warn};
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
     info!("Attempting to download bottle for {}", formula.name);

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
                    info!("Using valid cached bottle: {}", bottle_cache_path.display());
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
            info!(
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
        info!(
            "Detected OCI blob URL, initiating direct blob download: {}",
            bottle_url_str
        );
        match oci::download_oci_blob(bottle_url_str, &bottle_cache_path, config, client).await {
            Ok(_) => {
                info!(
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
        info!(
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
                info!(
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

    info!(
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
        info!(
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

    println!("==> Installing {} from bottle", formula.name());
    println!(
        "==> Pouring {} into {}",
        bottle_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        install_dir.display()
    );

    crate::build::extract_archive_strip_components(bottle_path, &install_dir, 1)?; // sync
    info!(
        "==> Ensuring write permissions for extracted files in {}",
        install_dir.display()
    );
    ensure_write_permissions(&install_dir)?; // sync
    info!(
        "==> Performing bottle relocation in {}",
        install_dir.display()
    );
    perform_bottle_relocation(formula, &install_dir, config)?; // sync call
    crate::build::write_receipt(formula, &install_dir)?; // sync

    info!(
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

fn perform_bottle_relocation(formula: &Formula, install_dir: &Path, config: &Config) -> Result<()> {
    // (Implementation remains the same)
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

    let formula_opt_path = config.prefix.join("opt").join(formula.name());
    let formula_opt_path_str = formula_opt_path.to_string_lossy().to_string();
    let formula_opt_placeholder = format!(
        "@@HOMEBREW_OPT_{}@@",
        formula.name().to_uppercase().replace('-', "_")
    );
    replacements.insert(formula_opt_placeholder, formula_opt_path_str);

    let perl_path_opt = formula
        .dependencies()
        .unwrap_or_default()
        .iter()
        .find(|d| d.name == "perl")
        .map(|_| config.prefix.join("opt").join("perl").join("bin").join("perl"))
        .or_else(|| {
            if cfg!(target_os = "macos") {
                debug!("Perl not a direct dependency, assuming system perl for @@HOMEBREW_PERL@@");
                Some(PathBuf::from("/usr/bin/perl"))
            } else {
                debug!("Perl not a direct dependency and not macOS, using brewed path as default for @@HOMEBREW_PERL@@");
                Some(config.prefix.join("opt").join("perl").join("bin").join("perl"))
            }
        })
        .filter(|p| p.exists());

    let perl_path = match perl_path_opt {
        Some(p) => p.to_string_lossy().to_string(),
        None => {
            warn!("Could not determine a valid path for @@HOMEBREW_PERL@@ replacement. Placeholder might remain.");
            "@@HOMEBREW_PERL@@".to_string()
        }
    };
    if perl_path != "@@HOMEBREW_PERL@@" {
        replacements.insert("@@HOMEBREW_PERL@@".to_string(), perl_path.clone());
    }

    info!("Starting file scan for text replacement and Mach-O patching in: {}", install_dir.display());
    for (placeholder, replacement) in &replacements {
        debug!("  Will replace '{}' with '{}'", placeholder, replacement);
    }

    let mut text_replaced_count = 0;
    let mut macho_patched_count = 0;
    let mut permission_errors = 0;
    let mut macho_errors = 0;
    let mut files_to_chmod: Vec<PathBuf> = Vec::new();

    for entry_result in WalkDir::new(install_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry_result.path();
        debug!("  Scanning file: {}", path.display());

        let is_potential_executable = path
            .components()
            .any(|comp| comp.as_os_str() == "bin" || comp.as_os_str() == "sbin");

        let metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                warn!("Relocation: Could not get metadata for {}: {}", path.display(), e);
                continue;
            }
        };
        if metadata.permissions().readonly() {
            debug!("  Skipping relocation for readonly file: {}", path.display());
            continue;
        }

        let mut was_modified = false;
        if cfg!(target_os = "macos") {
            match macho::patch_macho_file(path, &replacements) {
                Ok(patched) => {
                    if patched {
                        debug!("Successfully patched Mach-O file: {}", path.display());
                        macho_patched_count += 1;
                        was_modified = true;
                        if is_potential_executable { files_to_chmod.push(path.to_path_buf()); }
                    }
                }
                Err(SapphireError::PathTooLongError(e)) => { error!("Mach-O patching failed for {} (Path Too Long): {}", path.display(), e); macho_errors += 1; continue; }
                Err(SapphireError::CodesignError(e)) => { error!("Mach-O patching failed for {} (Codesign Failed): {}", path.display(), e); macho_errors += 1; continue; }
                Err(e) => { warn!("Mach-O patching check failed for {}: {}. Will attempt text replacement.", path.display(), e); }
            }
        }

        if !was_modified {
            let mut is_likely_text = false;
            match File::open(path) {
                Ok(mut file) => {
                    let mut buffer = [0; 1024];
                    match file.read(&mut buffer) {
                        Ok(n) if n > 0 => { if !buffer[..n].contains(&0) { is_likely_text = true; } }
                        Ok(_) => { is_likely_text = true; }
                        Err(e) => { warn!("Could not read chunk from {}: {}", path.display(), e); }
                    }
                }
                Err(e) => { warn!("Could not open {} to check if text: {}", path.display(), e); continue; }
            }

            if is_likely_text {
                match fs::read_to_string(path) {
                    Ok(content) => {
                        let mut modified_content = content.clone();
                        let mut text_was_modified_this_file = false;
                        for (placeholder, replacement) in &replacements {
                            if content.contains(placeholder) {
                                modified_content = modified_content.replace(placeholder, replacement);
                                debug!("  Replaced text placeholder '{}' in: {}", placeholder, path.display());
                                text_was_modified_this_file = true;
                            }
                        }
                        if text_was_modified_this_file {
                            match write_text_file_atomic(path, &modified_content) {
                                Ok(_) => {
                                    text_replaced_count += 1;
                                    if is_potential_executable { files_to_chmod.push(path.to_path_buf()); }
                                }
                                Err(e) => {
                                     if matches!(e, SapphireError::Io(ref io_err) if io_err.kind() == std::io::ErrorKind::PermissionDenied) {
                                        error!("Failed to write relocated text file {}: Permission denied", path.display());
                                        permission_errors += 1;
                                    } else {
                                        warn!("Failed to write relocated text file {}: {}", path.display(), e);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => { debug!("Skipping text relocation for {} (read failed - likely not UTF-8 text): {}", path.display(), e); }
                }
            } else {
                if is_potential_executable { files_to_chmod.push(path.to_path_buf()); }
                else { debug!("Skipping text relocation for {} (likely binary and not Mach-O patched)", path.display()); }
            }
        }
    }

    if cfg!(unix) {
        info!("Ensuring execute permissions for {} identified files...", files_to_chmod.len());
        for path in &files_to_chmod {
            match fs::metadata(path) {
                Ok(metadata) => {
                    let mut perms = metadata.permissions();
                    let current_mode = perms.mode();
                    let new_mode = current_mode | 0o111;
                    if new_mode != current_mode {
                         debug!("Setting execute permission (+x) for: {}", path.display());
                         perms.set_mode(new_mode);
                        if let Err(e) = fs::set_permissions(path, perms) {
                            warn!("Failed to set execute permission on {}: {}", path.display(), e);
                            permission_errors += 1;
                        }
                    } else {
                         debug!("Execute permission already set for: {}", path.display());
                    }
                }
                 Err(e) => {
                     warn!("Could not get metadata for {}: {}", path.display(), e);
                    permission_errors += 1;
                 }
            }
        }
    }

    info!("Relocation scan complete. Text files modified: {}, Mach-O files patched: {}", text_replaced_count, macho_patched_count);
    if permission_errors > 0 || macho_errors > 0 {
        error!("Relocation encountered errors! Permission errors: {}, Mach-O errors: {}. Installation may be broken.", permission_errors, macho_errors);
        return Err(SapphireError::InstallError(format!("Bottle relocation failed for {} files ({} permission, {} Mach-O). Check logs and permissions of {}.", permission_errors + macho_errors, permission_errors, macho_errors, install_dir.display())));
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