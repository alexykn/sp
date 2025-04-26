use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use reqwest::Client;
use semver;
use tempfile::NamedTempFile;
use tracing::{debug, error, warn};
use walkdir::WalkDir;

use super::macho;
use crate::build::formula::get_current_platform;
use crate::fetch::{self, oci};
use crate::model::formula::{BottleFileSpec, Formula, FormulaDependencies};
use crate::utils::config::Config;
use crate::utils::error::{Result, SpmError};

pub async fn download_bottle(
    formula: &Formula,
    config: &Config,
    client: &Client,
) -> Result<PathBuf> {
    debug!("Attempting to download bottle for {}", formula.name);
    let (platform_tag, bottle_file_spec) = get_bottle_for_platform(formula)?;
    debug!(
        "Selected bottle spec for platform '{}': URL={}, SHA256={}",
        platform_tag, bottle_file_spec.url, bottle_file_spec.sha256
    );
    if bottle_file_spec.url.is_empty() {
        return Err(SpmError::DownloadError(
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
    fs::create_dir_all(&cache_dir).map_err(|e| SpmError::Io(std::sync::Arc::new(e)))?;
    let bottle_cache_path = cache_dir.join(&filename);
    if bottle_cache_path.is_file() {
        debug!("Bottle found in cache: {}", bottle_cache_path.display());
        if !bottle_file_spec.sha256.is_empty() {
            match fetch::verify_checksum(&bottle_cache_path, &bottle_file_spec.sha256) {
                Ok(_) => {
                    debug!("Using valid cached bottle: {}", bottle_cache_path.display());
                    return Ok(bottle_cache_path);
                }
                Err(e) => {
                    debug!(
                        "Cached bottle checksum mismatch ({}): {}. Redownloading.",
                        bottle_cache_path.display(),
                        e
                    );
                    let _ = fs::remove_file(&bottle_cache_path);
                }
            }
        } else {
            warn!(
                "Using cached bottle without checksum verification (checksum not specified): {}",
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
        let expected_digest = bottle_url_str.split("/blobs/sha256:").nth(1).unwrap_or("");
        if expected_digest.is_empty() {
            warn!(
                "Could not extract expected SHA256 digest from OCI URL: {}",
                bottle_url_str
            );
        }
        debug!(
            "Detected OCI blob URL, initiating direct blob download: {} (Digest: {})",
            bottle_url_str, expected_digest
        );
        match oci::download_oci_blob(
            bottle_url_str,
            &bottle_cache_path,
            config,
            client,
            expected_digest,
        )
        .await
        {
            Ok(_) => {
                debug!(
                    "Successfully downloaded OCI blob to {}",
                    bottle_cache_path.display()
                );
            }
            Err(e) => {
                error!("Failed to download OCI blob from {}: {}", bottle_url_str, e);
                let _ = fs::remove_file(&bottle_cache_path);
                return Err(SpmError::DownloadError(
                    formula.name.clone(),
                    bottle_url_str.to_string(),
                    format!("Failed to download OCI blob: {e}"),
                ));
            }
        }
    } else {
        debug!(
            "Detected standard HTTPS URL, using direct download for: {}",
            bottle_url_str
        );
        match crate::fetch::http::fetch_formula_source_or_bottle(
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
                    debug!("fetch_formula_source_or_bottle returned path {}. Expected: {}. Assuming correct.", downloaded_path.display(), bottle_cache_path.display());
                    if !bottle_cache_path.exists() {
                        error!(
                            "Downloaded path {} exists, but expected final cache path {} does not!",
                            downloaded_path.display(),
                            bottle_cache_path.display()
                        );
                        return Err(SpmError::Generic(format!(
                            "Download path mismatch and final file missing: {}",
                            bottle_cache_path.display()
                        )));
                    }
                }
                debug!(
                    "Successfully downloaded directly to {}",
                    bottle_cache_path.display()
                );
            }
            Err(e) => {
                error!("Failed to download directly from {}: {}", bottle_url_str, e);
                return Err(e);
            }
        }
    }
    if !bottle_cache_path.exists() {
        error!(
            "Bottle download process completed, but the final file {} does not exist.",
            bottle_cache_path.display()
        );
        return Err(SpmError::Generic(format!(
            "Bottle file missing after download attempt: {}",
            bottle_cache_path.display()
        )));
    }
    debug!(
        "Bottle download successful: {}",
        bottle_cache_path.display()
    );
    Ok(bottle_cache_path)
}

pub(crate) fn get_bottle_for_platform(formula: &Formula) -> Result<(String, &BottleFileSpec)> {
    let stable_spec = formula.bottle.stable.as_ref().ok_or_else(|| {
        SpmError::Generic(format!(
            "Formula '{}' has no stable bottle specification.",
            formula.name
        ))
    })?;
    if stable_spec.files.is_empty() {
        return Err(SpmError::Generic(format!(
            "Formula '{}' has no bottle files listed in stable spec.",
            formula.name
        )));
    }
    let current_platform = get_current_platform();
    if current_platform == "unknown" || current_platform.contains("unknown") {
        debug!("Could not reliably determine current platform ('{}'). Bottle selection might be incorrect.", current_platform);
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
        debug!(
            "Found exact bottle match for platform: {}",
            current_platform
        );
        return Ok((current_platform.clone(), spec));
    }
    debug!("No exact match found for {}", current_platform);
    const ARM_MACOS_VERSIONS: &[&str] = &["sequoia", "sonoma", "ventura", "monterey", "big_sur"];
    const INTEL_MACOS_VERSIONS: &[&str] = &[
        "sequoia", "sonoma", "ventura", "monterey", "big_sur", "catalina", "mojave",
    ];
    if cfg!(target_os = "macos") {
        if let Some(current_os_name) = current_platform
            .strip_prefix("arm64_")
            .or(Some(current_platform.as_str()))
        {
            let version_list = if current_platform.starts_with("arm64_") {
                ARM_MACOS_VERSIONS
            } else {
                INTEL_MACOS_VERSIONS
            };
            if let Some(current_os_index) = version_list.iter().position(|&v| v == current_os_name)
            {
                for target_os_name in version_list.iter().skip(current_os_index) {
                    let target_tag = if current_platform.starts_with("arm64_") {
                        format!("arm64_{target_os_name}")
                    } else {
                        target_os_name.to_string()
                    };
                    if let Some(spec) = stable_spec.files.get(&target_tag) {
                        debug!("No bottle found for exact platform '{}'. Using compatible older bottle '{}'.", current_platform, target_tag);
                        return Ok((target_tag, spec));
                    }
                }
                debug!(
                    "Checked compatible older macOS versions ({:?}), no suitable bottle found.",
                    &version_list[current_os_index..]
                );
            } else {
                debug!(
                    "Current OS '{}' not found in known macOS version list.",
                    current_os_name
                );
            }
        } else {
            debug!(
                "Could not extract OS name from platform tag '{}'",
                current_platform
            );
        }
    }
    if current_platform.starts_with("arm64_") {
        if let Some(spec) = stable_spec.files.get("arm64_big_sur") {
            debug!(
                "No specific OS bottle found for {}. Falling back to 'arm64_big_sur' bottle.",
                current_platform
            );
            return Ok(("arm64_big_sur".to_string(), spec));
        }
        debug!("No 'arm64_big_sur' fallback bottle tag found.");
    } else if cfg!(target_os = "macos") {
        if let Some(spec) = stable_spec.files.get("big_sur") {
            debug!(
                "No specific OS bottle found for {}. Falling back to 'big_sur' bottle.",
                current_platform
            );
            return Ok(("big_sur".to_string(), spec));
        }
        debug!("No 'big_sur' fallback bottle tag found.");
    }
    if let Some(spec) = stable_spec.files.get("all") {
        debug!(
            "No platform-specific or OS-specific bottle found for {}. Using 'all' platform bottle.",
            current_platform
        );
        return Ok(("all".to_string(), spec));
    }
    debug!("No 'all' platform bottle found.");
    Err(SpmError::DownloadError(
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
    let install_dir = formula.install_prefix(&config.cellar)?;
    if install_dir.exists() {
        debug!(
            "Removing existing keg directory before installing: {}",
            install_dir.display()
        );
        fs::remove_dir_all(&install_dir).map_err(|e| {
            SpmError::InstallError(format!(
                "Failed to remove existing keg {}: {}",
                install_dir.display(),
                e
            ))
        })?;
    }
    if let Some(parent_dir) = install_dir.parent() {
        fs::create_dir_all(parent_dir).map_err(|e| {
            SpmError::Io(std::sync::Arc::new(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to create parent dir {}: {}",
                    parent_dir.display(),
                    e
                ),
            )))
        })?;
    } else {
        return Err(SpmError::InstallError(format!(
            "Could not determine parent directory for install path: {}",
            install_dir.display()
        )));
    }
    fs::create_dir_all(&install_dir).map_err(|e| {
        SpmError::Io(std::sync::Arc::new(std::io::Error::new(
            e.kind(),
            format!("Failed to create keg dir {}: {}", install_dir.display(), e),
        )))
    })?;
    let strip_components = 2;
    debug!(
        "Extracting bottle archive {} to {} with strip_components={}",
        bottle_path.display(),
        install_dir.display(),
        strip_components
    );
    crate::build::extract::extract_archive(bottle_path, &install_dir, strip_components, "gz")?;
    debug!(
        "Ensuring write permissions for extracted files in {}",
        install_dir.display()
    );
    ensure_write_permissions(&install_dir)?;
    debug!("Performing bottle relocation in {}", install_dir.display());
    perform_bottle_relocation(formula, &install_dir, config)?;
    ensure_llvm_symlinks(&install_dir, formula, config)?;
    crate::build::write_receipt(formula, &install_dir)?;
    debug!(
        "Bottle installation complete for {} at {}",
        formula.name(),
        install_dir.display()
    );
    Ok(install_dir)
}

fn ensure_write_permissions(path: &Path) -> Result<()> {
    if !path.exists() {
        debug!(
            "Path {} does not exist, cannot ensure write permissions.",
            path.display()
        );
        return Ok(());
    }
    for entry_result in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        let entry_path = entry_result.path();
        if entry_path == path && entry_result.depth() == 0 {
            continue;
        }
        match fs::metadata(entry_path) {
            Ok(metadata) => {
                let mut perms = metadata.permissions();
                let _is_readonly = perms.readonly();
                #[cfg(unix)]
                {
                    let current_mode = perms.mode();
                    let new_mode = current_mode | 0o200;
                    if new_mode != current_mode {
                        perms.set_mode(new_mode);
                        let _ = fs::set_permissions(entry_path, perms);
                    }
                }
                #[cfg(not(unix))]
                {
                    if _is_readonly {
                        perms.set_readonly(false);
                        let _ = fs::set_permissions(entry_path, perms);
                    }
                }
            }
            Err(_e) => {}
        }
    }
    Ok(())
}

fn perform_bottle_relocation(formula: &Formula, install_dir: &Path, config: &Config) -> Result<()> {
    let mut repl: HashMap<String, String> = HashMap::new();
    repl.insert(
        "@@HOMEBREW_CELLAR@@".into(),
        config.cellar_path().to_string_lossy().into(),
    );
    repl.insert(
        "@@HOMEBREW_PREFIX@@".into(),
        config.prefix().to_string_lossy().into(),
    );
    repl.insert(
        "@@HOMEBREW_REPOSITORY@@".into(),
        config.prefix().to_string_lossy().into(),
    );
    repl.insert(
        "@@HOMEBREW_LIBRARY@@".into(),
        config.prefix().join("Library").to_string_lossy().into(),
    );
    let opt_placeholder = format!(
        "@@HOMEBREW_OPT_{}@@",
        formula.name().to_uppercase().replace(['-', '+', '.'], "_")
    );
    repl.insert(
        opt_placeholder,
        config
            .formula_opt_link_path(formula.name())
            .to_string_lossy()
            .into(),
    );
    if let Some(p) = find_brewed_perl(config.prefix()).or_else(|| {
        if cfg!(target_os = "macos") {
            Some(PathBuf::from("/usr/bin/perl"))
        } else {
            None
        }
    }) {
        repl.insert("@@HOMEBREW_PERL@@".into(), p.to_string_lossy().into());
    }
    let llvm_dep_name = formula
        .dependencies()?
        .iter()
        .find(|d| d.name.starts_with("llvm"))
        .map(|d| d.name.clone());
    if let Some(name) = llvm_dep_name {
        let llvm_opt_path = config.formula_opt_link_path(&name);
        let llvm_lib = llvm_opt_path.join("lib");
        if llvm_lib.is_dir() {
            repl.insert(
                "@loader_path/../lib".into(),
                llvm_lib.to_string_lossy().into(),
            );
            repl.insert(
                format!(
                    "@@HOMEBREW_OPT_{}@@/lib",
                    name.to_uppercase().replace(['-', '+', '.'], "_")
                ),
                llvm_lib.to_string_lossy().into(),
            );
        }
    }
    tracing::debug!("Relocation table:");
    for (k, v) in &repl {
        tracing::debug!("{}  →  {}", k, v);
    }
    original_relocation_scan_and_patch(formula, install_dir, config, repl)
}

fn original_relocation_scan_and_patch(
    _formula: &Formula,
    install_dir: &Path,
    _config: &Config,
    replacements: HashMap<String, String>,
) -> Result<()> {
    let mut text_replaced_count = 0;
    let mut macho_patched_count = 0;
    let mut permission_errors = 0;
    let mut macho_errors = 0;
    let mut io_errors = 0;
    let mut files_to_chmod: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(install_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        let file_type = entry.file_type();
        if path
            .components()
            .any(|c| c.as_os_str().to_string_lossy().ends_with(".app"))
        {
            if file_type.is_file() {
                debug!("Skipping relocation inside .app bundle: {}", path.display());
            }
            continue;
        }
        if file_type.is_symlink() {
            debug!("Checking symlink for potential chmod: {}", path.display());
            if path
                .parent()
                .is_some_and(|p| p.ends_with("bin") || p.ends_with("sbin"))
            {
                files_to_chmod.push(path.to_path_buf());
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let (meta, initially_executable) = match fs::metadata(path) {
            Ok(m) => {
                #[cfg(unix)]
                let ie = m.permissions().mode() & 0o111 != 0;
                #[cfg(not(unix))]
                let ie = true;
                (m, ie)
            }
            Err(_e) => {
                io_errors += 1;
                continue;
            }
        };
        let is_in_exec_dir = path
            .parent()
            .is_some_and(|p| p.ends_with("bin") || p.ends_with("sbin"));
        if meta.permissions().readonly() {
            debug!(
                "Skipping readonly file during relocation: {}",
                path.display()
            );
            continue;
        }
        let mut was_modified = false;
        if cfg!(target_os = "macos")
            && (initially_executable
                || is_in_exec_dir
                || path
                    .extension()
                    .is_some_and(|e| e == "dylib" || e == "so" || e == "bundle"))
        {
            match macho::patch_macho_file(path, &replacements) {
                Ok(patched) if patched => {
                    macho_patched_count += 1;
                    was_modified = true;
                }
                Ok(_) => {}
                Err(SpmError::PathTooLongError(e)) => {
                    error!(
                        "Mach-O patch failed (path too long) for {}: {}",
                        path.display(),
                        e
                    );
                    macho_errors += 1;
                    continue;
                }
                Err(SpmError::CodesignError(e)) => {
                    error!(
                        "Mach-O patch failed (codesign error) for {}: {}",
                        path.display(),
                        e
                    );
                    macho_errors += 1;
                    continue;
                }
                Err(e @ SpmError::MachOError(_)) | Err(e @ SpmError::Object(_)) => {
                    debug!(
                        "Mach-O processing failed for {}: {}. Skipping Mach-O patch.",
                        path.display(),
                        e
                    );
                }
                Err(e) => {
                    debug!(
                        "Mach-O check/patch failed for {}: {}. Falling back to text replacer.",
                        path.display(),
                        e
                    );
                    io_errors += 1;
                }
            }
        }
        if !was_modified {
            let mut is_likely_text = true;
            if let Ok(mut f) = File::open(path) {
                let mut buf = [0; 1024];
                match f.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        if buf[..n].contains(&0) {
                            is_likely_text = false;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        is_likely_text = false;
                    }
                }
            } else {
                is_likely_text = false;
            }
            if is_likely_text {
                if let Ok(content) = fs::read_to_string(path) {
                    let mut new_content = content.clone();
                    let mut replacements_made = false;
                    for (placeholder, replacement) in &replacements {
                        if new_content.contains(placeholder) {
                            new_content = new_content.replace(placeholder, replacement);
                            replacements_made = true;
                        }
                    }
                    if replacements_made {
                        match write_text_file_atomic(path, &new_content) {
                            Ok(_) => {
                                text_replaced_count += 1;
                                was_modified = true;
                            }
                            Err(e) => {
                                error!(
                                    "Failed to write replaced text to {}: {}",
                                    path.display(),
                                    e
                                );
                                io_errors += 1;
                            }
                        }
                    }
                }
            }
        }
        if was_modified || initially_executable || is_in_exec_dir {
            files_to_chmod.push(path.to_path_buf());
        }
    }
    #[cfg(unix)]
    {
        debug!(
            "Ensuring execute permissions for {} potentially executable files/links",
            files_to_chmod.len()
        );
        let unique_files_to_chmod: HashSet<_> = files_to_chmod.into_iter().collect();
        for p in &unique_files_to_chmod {
            if !p.exists() && p.symlink_metadata().is_err() {
                continue;
            }
            match fs::symlink_metadata(p) {
                Ok(m) => {
                    if m.is_file() {
                        let mut perms = m.permissions();
                        let current_mode = perms.mode();
                        let new_mode = current_mode | 0o111;
                        if new_mode != current_mode {
                            perms.set_mode(new_mode);
                            if let Err(e) = fs::set_permissions(p, perms) {
                                debug!("Failed to set +x on {}: {}", p.display(), e);
                                permission_errors += 1;
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!(
                        "Could not stat {} during final chmod pass: {}",
                        p.display(),
                        e
                    );
                    permission_errors += 1;
                }
            }
        }
    }
    debug!(
        "Relocation scan complete. Text files replaced: {}, Mach-O files patched: {}",
        text_replaced_count, macho_patched_count
    );
    if permission_errors > 0 || macho_errors > 0 || io_errors > 0 {
        error!("Bottle relocation finished with issues: {} chmod errors, {} Mach-O errors, {} IO errors in {}.", permission_errors, macho_errors, io_errors, install_dir.display());
        if macho_errors > 0 {
            return Err(SpmError::InstallError(format!(
                "Bottle relocation failed due to {} Mach-O errors in {}",
                macho_errors,
                install_dir.display()
            )));
        }
        return Err(SpmError::InstallError(format!(
            "Bottle relocation encountered errors in {}",
            install_dir.display()
        )));
    }
    Ok(())
}

fn write_text_file_atomic(original_path: &Path, content: &str) -> Result<()> {
    let dir = original_path.parent().ok_or_else(|| {
        SpmError::Generic(format!(
            "Cannot get parent directory for {}",
            original_path.display()
        ))
    })?;
    let mut temp_file = NamedTempFile::new_in(dir)?;
    let temp_path = temp_file.path().to_path_buf();
    temp_file.write_all(content.as_bytes())?;
    temp_file.flush()?;
    temp_file.as_file().sync_all()?;
    let original_perms = fs::metadata(original_path).map(|m| m.permissions()).ok();
    temp_file.persist(original_path).map_err(|e| {
        error!(
            "Failed to persist/rename temporary text file {} over {}: {}",
            temp_path.display(),
            original_path.display(),
            e.error
        );
        SpmError::Io(std::sync::Arc::new(e.error))
    })?;
    if let Some(perms) = original_perms {
        let _ = fs::set_permissions(original_path, perms);
    }
    Ok(())
}

#[cfg(unix)]
fn find_brewed_perl(prefix: &Path) -> Option<PathBuf> {
    let opt_dir = prefix.join("opt");
    if !opt_dir.is_dir() {
        return None;
    }
    let mut best: Option<(semver::Version, PathBuf)> = None;
    match fs::read_dir(opt_dir) {
        Ok(entries) => {
            for entry_res in entries.flatten() {
                let name = entry_res.file_name();
                let s = name.to_string_lossy();
                let entry_path = entry_res.path();
                if !entry_path.is_dir() {
                    continue;
                }
                if let Some(version_part) = s.strip_prefix("perl@") {
                    let version_str_padded = if version_part.contains('.') {
                        format!("{version_part}.0")
                    } else {
                        format!("{version_part}.0.0")
                    };
                    if let Ok(v) = semver::Version::parse(&version_str_padded) {
                        let candidate_bin = entry_path.join("bin/perl");
                        if candidate_bin.is_file()
                            && (best.is_none() || v > best.as_ref().unwrap().0)
                        {
                            best = Some((v, candidate_bin));
                        }
                    }
                } else if s == "perl" {
                    let candidate_bin = entry_path.join("bin/perl");
                    if candidate_bin.is_file() && best.is_none() {
                        if let Ok(v_base) = semver::Version::parse("5.0.0") {
                            best = Some((v_base, candidate_bin));
                        }
                    }
                }
            }
        }
        Err(_e) => {}
    }
    best.map(|(_, path)| path)
}

fn ensure_llvm_symlinks(install_dir: &Path, formula: &Formula, config: &Config) -> Result<()> {
    let lib_dir = install_dir.join("lib");
    if !lib_dir.exists() {
        debug!(
            "Skipping LLVM symlink creation as lib dir is missing in {}",
            install_dir.display()
        );
        return Ok(());
    }
    let llvm_dep_name = match formula
        .dependencies()?
        .iter()
        .find(|d| d.name.starts_with("llvm"))
    {
        Some(dep) => dep.name.clone(),
        None => {
            debug!(
                "Formula {} does not list an LLVM dependency.",
                formula.name()
            );
            return Ok(());
        }
    };
    let llvm_opt_path = config.formula_opt_link_path(&llvm_dep_name);
    let llvm_lib_filename = if cfg!(target_os = "macos") {
        "libLLVM.dylib"
    } else if cfg!(target_os = "linux") {
        "libLLVM.so"
    } else {
        warn!("LLVM library filename unknown for target OS, skipping symlink.");
        return Ok(());
    };
    let llvm_lib_path_in_opt = llvm_opt_path.join("lib").join(llvm_lib_filename);
    if !llvm_lib_path_in_opt.exists() {
        debug!(
            "Required LLVM library not found at {}. Cannot create symlink in {}.",
            llvm_lib_path_in_opt.display(),
            formula.name()
        );
        return Ok(());
    }
    let symlink_target_path = lib_dir.join(llvm_lib_filename);
    if symlink_target_path.exists() || symlink_target_path.symlink_metadata().is_ok() {
        debug!(
            "Symlink or file already exists at {}. Skipping creation.",
            symlink_target_path.display()
        );
        return Ok(());
    }
    #[cfg(unix)]
    {
        if let Some(parent) = symlink_target_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        match symlink(&llvm_lib_path_in_opt, &symlink_target_path) {
            Ok(_) => debug!(
                "Created symlink {} -> {}",
                symlink_target_path.display(),
                llvm_lib_path_in_opt.display()
            ),
            Err(e) => {
                warn!(
                    "Failed to create LLVM symlink {} -> {}: {}",
                    symlink_target_path.display(),
                    llvm_lib_path_in_opt.display(),
                    e
                );
            }
        }
    }
    #[cfg(not(unix))]
    {
        debug!(
            "LLVM Symlink creation not supported on this platform for {}.",
            formula.name()
        );
    }
    Ok(())
}
