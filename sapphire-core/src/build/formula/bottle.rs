// ===== sapphire-core/src/build/formula/bottle.rs =====
// Corrected E0308

use super::macho;
use crate::fetch::{http, oci};
use crate::model::formula::{BottleFileSpec, Formula, FormulaDependencies};
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use log::{debug, error, warn};
use reqwest::Client;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read; // Added io import
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
    // (Implementation remains the same as provided previously)
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

pub(crate) fn get_bottle_for_platform(formula: &Formula) -> Result<(String, &BottleFileSpec)> {
    // (Implementation remains the same as provided previously)
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

    if let Some(spec) = stable_spec.files.get(&current_platform) {
        debug!("Found exact bottle match for platform: {}", current_platform);
        return Ok((current_platform.clone(), spec));
    }
    debug!("No exact match found for {}", current_platform);

    const ARM_MACOS_VERSIONS: &[&str] = &["sequoia", "sonoma", "ventura", "monterey", "big_sur"];
    const INTEL_MACOS_VERSIONS: &[&str] = &["sequoia", "sonoma", "ventura", "monterey", "big_sur", "catalina", "mojave"];

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
                 debug!("Checked compatible ARM macOS versions ({:?}), no suitable bottle found.", &ARM_MACOS_VERSIONS[current_os_index..]);
            } else {
                 debug!("Current OS '{}' not found in known ARM macOS version list.", current_os_name);
            }
        } else {
             debug!("Could not extract OS name from ARM platform tag '{}'", current_platform);
        }
    } else if cfg!(target_os = "macos") && !current_platform.starts_with("arm64_") {
         let current_os_name = &current_platform;
         if let Some(current_os_index) = INTEL_MACOS_VERSIONS.iter().position(|&v| v == current_os_name) {
             for i in current_os_index..INTEL_MACOS_VERSIONS.len() {
                 let target_os_name = INTEL_MACOS_VERSIONS[i];
                 if let Some(spec) = stable_spec.files.get(target_os_name) {
                     warn!(
                         "No bottle found for exact platform '{}'. Using compatible older bottle '{}'.",
                         current_platform, target_os_name
                     );
                     return Ok((target_os_name.to_string(), spec));
                 }
             }
             debug!("Checked compatible Intel macOS versions ({:?}), no suitable bottle found.", &INTEL_MACOS_VERSIONS[current_os_index..]);
         } else {
             debug!("Current OS '{}' not found in known Intel macOS version list.", current_os_name);
         }
    }

     if current_platform.starts_with("arm64_") {
        if let Some(spec) = stable_spec.files.get("arm64_big_sur") {
            warn!(
                "No specific OS bottle found for {}. Falling back to 'arm64_big_sur' bottle.",
                current_platform
            );
            return Ok(("arm64_big_sur".to_string(), spec));
        }
         debug!("No 'arm64_big_sur' fallback bottle tag found.");
     } else if cfg!(target_os = "macos") {
         if let Some(spec) = stable_spec.files.get("big_sur") {
             warn!(
                 "No specific OS bottle found for {}. Falling back to 'big_sur' bottle.",
                 current_platform
             );
             return Ok(("big_sur".to_string(), spec));
         }
         debug!("No 'big_sur' fallback bottle tag found.");
     }

    if let Some(spec) = stable_spec.files.get("all") {
        warn!(
            "No platform-specific or OS-specific bottle found for {}. Using 'all' platform bottle.",
            current_platform
        );
        return Ok(("all".to_string(), spec));
    }
    debug!("No 'all' platform bottle found.");

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

pub fn install_bottle(bottle_path: &Path, formula: &Formula, config: &Config) -> Result<PathBuf> {
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

    crate::build::extract::extract_archive(bottle_path, &install_dir, 2)?;

    ensure_llvm_symlinks(&install_dir, formula, config)?;

    debug!(
        "==> Ensuring write permissions for extracted files in {}",
        install_dir.display()
    );
    ensure_write_permissions(&install_dir)?;
    debug!(
        "==> Performing bottle relocation in {}",
        install_dir.display()
    );
    perform_bottle_relocation(formula, &install_dir, config)?;
    crate::build::write_receipt(formula, &install_dir)?;

    debug!(
        "Bottle installation complete for {} at {}",
        formula.name(),
        install_dir.display()
    );
    Ok(install_dir)
}

fn ensure_write_permissions(path: &Path) -> Result<()> {
    // (Implementation remains the same as provided previously)
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
    formula: &Formula,
    install_dir: &Path,
    config: &Config,
) -> Result<()> {
    // (Implementation remains the same as provided previously)
    let mut repl: HashMap<String, String> = HashMap::new();
    repl.insert("@@HOMEBREW_CELLAR@@".into(), config.cellar.to_string_lossy().into());
    repl.insert("@@HOMEBREW_PREFIX@@".into(), config.prefix.to_string_lossy().into());
    repl.insert(
        "@@HOMEBREW_REPOSITORY@@".into(),
        config.prefix.join("Library/Taps/homebrew/homebrew-core").to_string_lossy().into()
    );
    repl.insert(
        "@@HOMEBREW_LIBRARY@@".into(),
        config.prefix.join("Library").to_string_lossy().into()
    );

    let opt_placeholder = format!(
        "@@HOMEBREW_OPT_{}@@",
        formula.name().to_uppercase().replace('-', "_")
    );
    repl.insert(
        opt_placeholder,
        config.prefix.join("opt").join(formula.name()).to_string_lossy().into()
    );

    if let Some(p) = find_brewed_perl(&config.prefix)
        .or_else(|| if cfg!(target_os = "macos") { Some(PathBuf::from("/usr/bin/perl")) } else { None })
    {
        repl.insert("@@HOMEBREW_PERL@@".into(), p.to_string_lossy().into());
    }

    let llvm_name = formula
        .dependencies()
        .unwrap_or_default()
        .iter()
        .find(|d| d.name.starts_with("llvm"))
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "llvm".into());
    let llvm_lib = config.prefix.join("opt").join(llvm_name).join("lib");
    if llvm_lib.is_dir() {
        repl.insert("@loader_path/../lib".into(), llvm_lib.to_string_lossy().into());
    }

    log::debug!("Relocation table:");
    for (k, v) in &repl {
        log::debug!("  {}  →  {}", k, v);
    }

    original_relocation_scan_and_patch(formula, install_dir, config, repl)
}

fn original_relocation_scan_and_patch(
    _formula: &Formula,
    install_dir: &Path,
    _config: &Config,
    replacements: HashMap<String, String>,
) -> Result<()> {
    // (Implementation remains the same as provided previously, with E0308 fix)
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

        if cfg!(target_os = "macos") {
            match macho::patch_macho_file(path, &replacements) {
                Ok(patched) if patched => {
                    debug!("Successfully patched Mach-O file: {}", path.display());
                    macho_patched_count += 1;
                    was_modified = true;
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
                // *** FIX for E0308: Use @ pattern to bind the whole error ***
                Err(e @ SapphireError::MachOError(_)) | Err(e @ SapphireError::Object(_)) => {
                     warn!("Mach-O processing failed for {}: {}. Skipping text replacement.", path.display(), e);
                     macho_errors += 1;
                     continue;
                 }
                Err(e) => {
                    warn!("Mach-O check failed for {} (non-fatal for text replacement): {}. Falling back to text replacer.", path.display(), e);
                }
            }
        }

        if !was_modified {
            let mut is_text = false;
            if let Ok(mut f) = File::open(path) {
                let mut buf = [0; 1024];
                if let Ok(n) = f.read(&mut buf) {
                    if n > 0 && !buf[..n].contains(&0) {
                        is_text = true;
                    }
                }
            } else {
                 warn!("Could not open file {} for text check.", path.display());
            }


            if is_text {
                match fs::read_to_string(path) {
                    Ok(content) => {
                         let mut new_content = content.clone();
                         let mut replacements_made = false;
                         for (placeholder, replacement) in &replacements {
                             if new_content.contains(placeholder) {
                                 new_content = new_content.replace(placeholder, replacement);
                                 replacements_made = true;
                                 debug!("  Text Replaced '{}' in {}", placeholder, path.display());
                             }
                         }

                         if replacements_made {
                             match write_text_file_atomic(path, &new_content) {
                                 Ok(_) => {
                                     text_replaced_count += 1;
                                     was_modified = true;
                                      debug!("Successfully wrote replaced text to {}", path.display());
                                 }
                                 Err(e) => {
                                     error!("Failed to write replaced text to {}: {}", path.display(), e);
                                     continue;
                                 }
                             }
                         }
                     }
                    Err(e) => {
                         debug!("File {} is not valid UTF-8 (likely binary), skipping text replacement. Error: {}", path.display(), e);
                     }
                 }
            } else {
                debug!("Skipping text replacement for non-text file: {}", path.display());
            }
        }

        if was_modified || is_potential_executable {
             files_to_chmod.push(path.to_path_buf());
        }
    }

    #[cfg(unix)]
    {
        debug!("Ensuring execute permissions for {} files...", files_to_chmod.len());
        let unique_files_to_chmod: std::collections::HashSet<_> = files_to_chmod.into_iter().collect();

        for p in &unique_files_to_chmod {
            match fs::metadata(p) {
                Ok(m) => {
                    let mut perms = m.permissions();
                    let current_mode = perms.mode();
                    let new_mode = current_mode | 0o111;
                    if new_mode != current_mode {
                        perms.set_mode(new_mode);
                        if let Err(e) = fs::set_permissions(p, perms) {
                            warn!("Failed to set +x on {}: {}", p.display(), e);
                            permission_errors += 1;
                        } else {
                             debug!("Set +x on {}", p.display());
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
        "Relocation complete. Text files replaced: {}, Mach-O files patched: {}",
        text_replaced_count, macho_patched_count
    );
    if permission_errors > 0 || macho_errors > 0 {
        error!(
            "Encountered {} chmod errors and {} Mach-O errors during relocation. Installation may be broken.",
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
    // (Implementation remains the same as provided previously)
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

      let original_perms = fs::metadata(original_path).map(|m| m.permissions()).ok();


     temp_file.persist(original_path).map_err(|e| {
         error!(
             "    Failed to persist/rename temporary text file over {}: {}",
             original_path.display(),
             e.error
         );
         SapphireError::Io(e.error)
     })?;

      if let Some(perms) = original_perms {
         if let Err(e) = fs::set_permissions(original_path, perms) {
             warn!("Failed to restore permissions on {}: {}", original_path.display(), e);
         }
      }


     debug!(
         "    Atomically replaced {} with relocated text version",
         original_path.display()
     );
     Ok(())
}

// find_brewed_perl (unchanged)
fn find_brewed_perl(prefix: &Path) -> Option<PathBuf> {
    // (Implementation remains the same as provided previously)
    let opt_dir = prefix.join("opt");
    let mut best: Option<(semver::Version, PathBuf)> = None;

    for entry in std::fs::read_dir(opt_dir).ok()?.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();

        if let Some(rest) = s.strip_prefix("perl@") {
            let version_str = rest.split('.').take(2).collect::<Vec<_>>().join(".");
            let version_str_padded = format!("{}.0", version_str);

            if let Ok(v) = semver::Version::parse(&version_str_padded) {
                 let cand = entry.path().join("bin/perl");
                 if cand.is_file() && best.as_ref().map_or(true, |(b, _)| v > *b) {
                     best = Some((v, cand));
                 }
            } else {
                 warn!("Could not parse perl version from: {}", rest);
            }
        } else if s == "perl" {
            let cand = entry.path().join("bin/perl");
            if cand.is_file() && best.is_none() {
                best = Some((semver::Version::new(5, 0, 0), cand));
            }
        }
    }
    best.map(|(_, p)| p)
}

// ensure_llvm_symlinks (unchanged)
fn ensure_llvm_symlinks(install_dir: &Path, formula: &Formula, config: &Config) -> Result<()> {
    // (Implementation remains the same as provided previously)
    let bin_dir = install_dir.join("bin");
    let lib_dir = install_dir.join("lib");
    if !bin_dir.exists() || !lib_dir.exists() {
        debug!("Skipping LLVM symlink creation as bin or lib dir is missing in {}", install_dir.display());
        return Ok(());
    }

    let llvm_name = formula
        .dependencies()
        .unwrap_or_default()
        .iter()
        .find(|d| d.name.starts_with("llvm"))
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "llvm".into());
    let llvm_lib_path = config
        .prefix
        .join("opt")
        .join(llvm_name)
        .join("lib")
        .join("libLLVM.dylib");

    if !llvm_lib_path.exists() {
        warn!("LLVM library not found at {}. Skipping symlink creation.", llvm_lib_path.display());
        return Ok(());
    }

    let symlink_path = lib_dir.join("libLLVM.dylib");
    if symlink_path.exists() || symlink_path.symlink_metadata().is_ok() {
        debug!("Symlink or file already exists at {}.", symlink_path.display());
        return Ok(());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        match symlink(&llvm_lib_path, &symlink_path) {
            Ok(_) => debug!("Created symlink {} -> {}", symlink_path.display(), llvm_lib_path.display()),
            Err(e) => {
                warn!("Failed to create LLVM symlink {} -> {}: {}", symlink_path.display(), llvm_lib_path.display(), e);
            }
        }
    }
    #[cfg(not(unix))]
    {
        debug!("LLVM Symlink creation not supported on this platform.");
    }
    Ok(())
}