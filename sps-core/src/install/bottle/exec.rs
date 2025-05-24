use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;

use reqwest::Client;
use semver;
use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_common::model::formula::{BottleFileSpec, Formula, FormulaDependencies};
use sps_net::oci;
use sps_net::validation::verify_checksum;
use tempfile::NamedTempFile;
use tracing::{debug, error, warn};
use walkdir::WalkDir;

use super::macho;
use crate::install::bottle::get_current_platform;
use crate::install::extract::extract_archive;

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
        return Err(SpsError::DownloadError(
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
    let cache_dir = config.cache_dir().join("bottles");
    fs::create_dir_all(&cache_dir).map_err(|e| SpsError::Io(std::sync::Arc::new(e)))?;
    let bottle_cache_path = cache_dir.join(&filename);
    if bottle_cache_path.is_file() {
        debug!("Bottle found in cache: {}", bottle_cache_path.display());
        if !bottle_file_spec.sha256.is_empty() {
            match verify_checksum(&bottle_cache_path, &bottle_file_spec.sha256) {
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
                return Err(SpsError::DownloadError(
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
        match sps_net::http::fetch_formula_source_or_bottle(
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
                    debug!(
                        "fetch_formula_source_or_bottle returned path {}. Expected: {}. Assuming correct.",
                        downloaded_path.display(),
                        bottle_cache_path.display()
                    );
                    if !bottle_cache_path.exists() {
                        error!(
                            "Downloaded path {} exists, but expected final cache path {} does not!",
                            downloaded_path.display(),
                            bottle_cache_path.display()
                        );
                        return Err(SpsError::Generic(format!(
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
        return Err(SpsError::Generic(format!(
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

pub fn get_bottle_for_platform(formula: &Formula) -> Result<(String, &BottleFileSpec)> {
    let stable_spec = formula.bottle.stable.as_ref().ok_or_else(|| {
        SpsError::Generic(format!(
            "Formula '{}' has no stable bottle specification.",
            formula.name
        ))
    })?;
    if stable_spec.files.is_empty() {
        return Err(SpsError::Generic(format!(
            "Formula '{}' has no bottle files listed in stable spec.",
            formula.name
        )));
    }
    let current_platform = get_current_platform();
    if current_platform == "unknown" || current_platform.contains("unknown") {
        debug!(
            "Could not reliably determine current platform ('{}'). Bottle selection might be incorrect.",
            current_platform
        );
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
                        debug!(
                            "No bottle found for exact platform '{}'. Using compatible older bottle '{}'.",
                            current_platform, target_tag
                        );
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
    Err(SpsError::DownloadError(
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
    let install_dir = formula.install_prefix(config.cellar_dir().as_path())?;
    if install_dir.exists() {
        debug!(
            "Removing existing keg directory before installing: {}",
            install_dir.display()
        );
        fs::remove_dir_all(&install_dir).map_err(|e| {
            SpsError::InstallError(format!(
                "Failed to remove existing keg {}: {}",
                install_dir.display(),
                e
            ))
        })?;
    }
    if let Some(parent_dir) = install_dir.parent() {
        fs::create_dir_all(parent_dir).map_err(|e| {
            SpsError::Io(std::sync::Arc::new(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to create parent dir {}: {}",
                    parent_dir.display(),
                    e
                ),
            )))
        })?;
    } else {
        return Err(SpsError::InstallError(format!(
            "Could not determine parent directory for install path: {}",
            install_dir.display()
        )));
    }
    fs::create_dir_all(&install_dir).map_err(|e| {
        SpsError::Io(std::sync::Arc::new(std::io::Error::new(
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
    extract_archive(bottle_path, &install_dir, strip_components, "gz")?;
    debug!(
        "Ensuring write permissions for extracted files in {}",
        install_dir.display()
    );
    ensure_write_permissions(&install_dir)?;
    debug!("Performing bottle relocation in {}", install_dir.display());
    perform_bottle_relocation(formula, &install_dir, config)?;
    ensure_llvm_symlinks(&install_dir, formula, config)?;
    crate::install::bottle::write_receipt(formula, &install_dir, "bottle")?;
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
        config.cellar_dir().to_string_lossy().into(),
    );
    repl.insert(
        "@@HOMEBREW_PREFIX@@".into(),
        config.sps_root().to_string_lossy().into(),
    );
    let _prefix_path_str = config.sps_root().to_string_lossy();
    let library_path_str = config
        .sps_root()
        .join("Library")
        .to_string_lossy()
        .to_string(); // Assuming Library is under sps_root for this placeholder
                      // HOMEBREW_REPOSITORY usually points to the Homebrew/brew git repo, not relevant for sps in
                      // this context. If needed for a specific formula, it should point to
                      // /opt/sps or similar.
    repl.insert(
        "@@HOMEBREW_REPOSITORY@@".into(),
        config.sps_root().to_string_lossy().into(),
    );
    repl.insert("@@HOMEBREW_LIBRARY@@".into(), library_path_str.to_string());

    let formula_opt_path = config.formula_opt_path(formula.name());
    let formula_opt_str = formula_opt_path.to_string_lossy();
    let install_dir_str = install_dir.to_string_lossy();
    if formula_opt_str != install_dir_str {
        repl.insert(formula_opt_str.to_string(), install_dir_str.to_string());
        debug!(
            "Adding self-opt relocation: {} -> {}",
            formula_opt_str, install_dir_str
        );
    }

    if formula.name().starts_with("python@") {
        let version_full = formula.version_str_full();
        let mut parts = version_full.split('.');
        if let (Some(major), Some(minor)) = (parts.next(), parts.next()) {
            let framework_version = format!("{major}.{minor}");
            let framework_dir = install_dir
                .join("Frameworks")
                .join("Python.framework")
                .join("Versions")
                .join(&framework_version);
            let python_lib = framework_dir.join("Python");
            let python_bin = framework_dir
                .join("bin")
                .join(format!("python{major}.{minor}"));

            let absolute_python_lib_path_obj = install_dir
                .join("Frameworks")
                .join("Python.framework")
                .join("Versions")
                .join(&framework_version)
                .join("Python");
            let new_id_abs = absolute_python_lib_path_obj.to_str().ok_or_else(|| {
                SpsError::InstallError(format!(
                    "Failed to convert absolute Python library path to string: {}",
                    absolute_python_lib_path_obj.display()
                ))
            })?;

            debug!(
                "Setting absolute ID for {}: {}",
                python_lib.display(),
                new_id_abs
            );
            let status_id = StdCommand::new("install_name_tool")
                .args(["-id", new_id_abs, python_lib.to_str().unwrap()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| SpsError::Io(Arc::new(e)))?;
            if !status_id.success() {
                error!("install_name_tool -id failed for {}", python_lib.display());
                return Err(SpsError::InstallError(format!(
                    "Failed to set absolute id on Python dynamic library: {}",
                    python_lib.display()
                )));
            }

            debug!("Skipping -add_rpath as absolute paths are used for Python linkage.");

            let old_load_placeholder = format!(
                "@@HOMEBREW_CELLAR@@/{}/{}/Frameworks/Python.framework/Versions/{}/Python",
                formula.name(),
                version_full,
                framework_version
            );
            let old_load_resource_placeholder = format!(
                "@@HOMEBREW_CELLAR@@/{}/{}/Frameworks/Python.framework/Versions/{}/Resources/Python.app/Contents/MacOS/Python",
                formula.name(),
                version_full,
                framework_version
            );
            let install_dir_str_ref = install_dir.to_string_lossy();
            let abs_old_load = format!(
                "{install_dir_str_ref}/Frameworks/Python.framework/Versions/{framework_version}/Python"
            );
            let abs_old_load_resource = format!(
                "{install_dir_str_ref}/Frameworks/Python.framework/Versions/{framework_version}/Resources/Python.app/Contents/MacOS/Python"
            );

            let run_change = |old: &str, new: &str, target: &Path| -> Result<()> {
                if !target.exists() {
                    debug!(
                        "Target {} does not exist, skipping install_name_tool -change.",
                        target.display()
                    );
                    return Ok(());
                }
                debug!(
                    "Running install_name_tool -change {} {} {}",
                    old,
                    new,
                    target.display()
                );
                let output = StdCommand::new("install_name_tool")
                    .args(["-change", old, new, target.to_str().unwrap()])
                    .output()
                    .map_err(|e| SpsError::Io(Arc::new(e)))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if !stderr.contains("file not found")
                        && !stderr.contains("no LC_LOAD_DYLIB command specifying file")
                        && !stderr.contains("is not a Mach-O file")
                        && !stderr.contains("object file format invalid")
                        && !stderr.trim().is_empty()
                    {
                        error!(
                            "install_name_tool -change failed unexpectedly for target {}: {}",
                            target.display(),
                            stderr.trim()
                        );
                    } else {
                        debug!(
                            "install_name_tool -change: old path '{}' likely not found or target '{}' not relevant (stderr: {}).",
                            old,
                            target.display(),
                            stderr.trim()
                        );
                    }
                }
                Ok(())
            };

            debug!("Patching main executable: {}", python_bin.display());
            run_change(&old_load_placeholder, new_id_abs, &python_bin)?;
            run_change(&old_load_resource_placeholder, new_id_abs, &python_bin)?;
            run_change(&abs_old_load, new_id_abs, &python_bin)?;
            run_change(&abs_old_load_resource, new_id_abs, &python_bin)?;

            let python_app = framework_dir
                .join("Resources")
                .join("Python.app")
                .join("Contents")
                .join("MacOS")
                .join("Python");

            if python_app.exists() {
                debug!(
                    "Explicitly patching Python.app executable: {}",
                    python_app.display()
                );
                run_change(&old_load_placeholder, new_id_abs, &python_app)?;
                run_change(&abs_old_load, new_id_abs, &python_app)?;
                run_change(&old_load_resource_placeholder, new_id_abs, &python_app)?;
                run_change(&abs_old_load_resource, new_id_abs, &python_app)?;
            } else {
                warn!(
                    "Python.app executable not found at {}, skipping explicit patch.",
                    python_app.display()
                );
            }

            codesign_path(&python_lib)?;
            codesign_path(&python_bin)?;
            if python_app.exists() {
                codesign_path(&python_app)?;
            } else {
                debug!(
                    "Python.app binary not found at {}, skipping codesign.",
                    python_app.display()
                );
            }
        }
    }

    if let Some(perl_path) = find_brewed_perl(config.sps_root()).or_else(|| {
        if cfg!(target_os = "macos") {
            Some(PathBuf::from("/usr/bin/perl"))
        } else {
            None
        }
    }) {
        repl.insert(
            "@@HOMEBREW_PERL@@".into(),
            perl_path.to_string_lossy().into(),
        );
    }

    match formula.dependencies() {
        Ok(deps) => {
            if let Some(openjdk) = deps
                .iter()
                .find(|d| d.name.starts_with("openjdk"))
                .map(|d| d.name.clone())
            {
                let openjdk_opt = config.formula_opt_path(&openjdk);
                repl.insert(
                    "@@HOMEBREW_JAVA@@".into(),
                    openjdk_opt
                        .join("libexec/openjdk.jdk/Contents/Home")
                        .to_string_lossy()
                        .into(),
                );
            }
        }
        Err(e) => {
            warn!(
                "Could not check formula dependencies during relocation for {}: {}",
                formula.name(),
                e
            );
        }
    }

    repl.insert("HOMEBREW_RELOCATE_RPATHS".into(), "1".into());

    let opt_placeholder = format!(
        "@@HOMEBREW_OPT_{}@@",
        formula.name().to_uppercase().replace(['-', '+', '.'], "_")
    );
    repl.insert(
        opt_placeholder,
        config
            .formula_opt_path(formula.name())
            .to_string_lossy()
            .into(),
    );

    match formula.dependencies() {
        Ok(deps) => {
            let llvm_dep_name = deps
                .iter()
                .find(|d| d.name.starts_with("llvm"))
                .map(|d| d.name.clone());
            if let Some(name) = llvm_dep_name {
                let llvm_opt_path = config.formula_opt_path(&name);
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
        }
        Err(e) => {
            warn!(
                "Could not check formula dependencies during LLVM relocation for {}: {}",
                formula.name(),
                e
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
                debug!("Failed to get metadata for {}: {}", path.display(), _e);
                io_errors += 1;
                continue;
            }
        };
        let is_in_exec_dir = path
            .parent()
            .is_some_and(|p| p.ends_with("bin") || p.ends_with("sbin"));
        if meta.permissions().readonly() {
            #[cfg(unix)]
            {
                let mut perms = meta.permissions();
                let current_mode = perms.mode();
                let new_mode = current_mode | 0o200;
                if new_mode != current_mode {
                    perms.set_mode(new_mode);
                    if fs::set_permissions(path, perms).is_err() {
                        debug!(
                            "Skipping readonly file (and couldn't make writable): {}",
                            path.display()
                        );
                        continue;
                    } else {
                        debug!("Made readonly file writable: {}", path.display());
                    }
                }
            }
            #[cfg(not(unix))]
            {
                debug!(
                    "Skipping potentially readonly file on non-unix: {}",
                    path.display()
                );
                continue;
            }
        }
        let mut was_modified = false;
        let mut skipped_paths_for_file = Vec::new();
        if cfg!(target_os = "macos")
            && (initially_executable
                || is_in_exec_dir
                || path
                    .extension()
                    .is_some_and(|e| e == "dylib" || e == "so" || e == "bundle"))
        {
            match macho::patch_macho_file(path, &replacements) {
                Ok((true, skipped_paths)) => {
                    macho_patched_count += 1;
                    was_modified = true;
                    skipped_paths_for_file = skipped_paths;
                }
                Ok((false, skipped_paths)) => {
                    // Not Mach-O or no patches needed, but might have skipped paths
                    skipped_paths_for_file = skipped_paths;
                }
                Err(SpsError::PathTooLongError(e)) => { // Specifically catch path too long
                    error!(
                        "Mach-O patch failed (path too long) for {}: {}",
                        path.display(),
                        e
                    );
                    macho_errors += 1;
                    // Continue scanning other files even if one fails this way
                    continue;
                }
                Err(SpsError::CodesignError(e)) => { // Specifically catch codesign errors
                     error!(
                        "Mach-O patch failed (codesign error) for {}: {}",
                        path.display(),
                        e
                     );
                     macho_errors += 1;
                     // Continue scanning other files
                     continue;
                }
                 // Catch generic MachOError or Object error, treat as non-fatal for text replace
                Err(e @ SpsError::MachOError(_))
                | Err(e @ SpsError::Object(_))
                | Err(e @ SpsError::Generic(_)) // Catch Generic errors from patch_macho too
                | Err(e @ SpsError::Io(_)) => { // Catch IO errors from patch_macho
                    debug!(
                        "Mach-O processing/patching failed for {}: {}. Skipping Mach-O patch for this file.",
                        path.display(),
                        e
                    );
                     // Don't increment macho_errors here, as we fallback to text replace
                     io_errors += 1; // Count as IO or generic error instead
                }
                 // Catch other specific errors if needed
                 Err(e) => {
                      debug!(
                         "Unexpected error during Mach-O check/patch for {}: {}. Falling back to text replacer.",
                         path.display(),
                         e
                      );
                      io_errors += 1;
                 }
            }

            // Handle paths that were too long for Mach-O patching with install_name_tool fallback
            if !skipped_paths_for_file.is_empty() {
                debug!(
                    "Applying install_name_tool fallback for {} skipped paths in {}",
                    skipped_paths_for_file.len(),
                    path.display()
                );
                debug!(
                    "Applying install_name_tool fallback for {} skipped paths in {}",
                    skipped_paths_for_file.len(),
                    path.display()
                );
                for skipped in &skipped_paths_for_file {
                    match apply_install_name_tool_change(&skipped.old_path, &skipped.new_path, path)
                    {
                        Ok(()) => {
                            debug!(
                                "Successfully applied install_name_tool fallback: '{}' -> '{}' in {}",
                                skipped.old_path, skipped.new_path, path.display()
                            );
                            was_modified = true;
                        }
                        Err(e) => {
                            warn!(
                                "install_name_tool fallback failed for '{}' -> '{}' in {}: {}",
                                skipped.old_path,
                                skipped.new_path,
                                path.display(),
                                e
                            );
                            macho_errors += 1;
                        }
                    }
                }
            }
        }
        // Fallback to text replacement if not modified by Mach-O patching
        if !was_modified {
            // Heuristic check for text file (avoid reading huge binaries)
            let mut is_likely_text = false;
            if meta.len() < 5 * 1024 * 1024 {
                if let Ok(mut f) = File::open(path) {
                    let mut buf = [0; 1024];
                    match f.read(&mut buf) {
                        Ok(n) if n > 0 => {
                            if !buf[..n].contains(&0) {
                                is_likely_text = true;
                            }
                        }
                        Ok(_) => {
                            is_likely_text = true;
                        }
                        Err(_) => {}
                    }
                }
            }

            if is_likely_text {
                // Read the file content as string
                if let Ok(content) = fs::read_to_string(path) {
                    let mut new_content = content.clone();
                    let mut replacements_made = false;
                    for (placeholder, replacement) in &replacements {
                        if new_content.contains(placeholder) {
                            new_content = new_content.replace(placeholder, replacement);
                            replacements_made = true;
                        }
                    }
                    // Write back only if changes were made
                    if replacements_made {
                        match write_text_file_atomic(path, &new_content) {
                            Ok(_) => {
                                text_replaced_count += 1;
                                was_modified = true; // Mark as modified for chmod check
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
                } else if meta.len() > 0 {
                    debug!(
                        "Could not read {} as string for text replacement.",
                        path.display()
                    );
                    io_errors += 1;
                }
            } else if meta.len() >= 5 * 1024 * 1024 {
                debug!(
                    "Skipping text replacement for large file: {}",
                    path.display()
                );
            } else {
                debug!(
                    "Skipping text replacement for likely binary file: {}",
                    path.display()
                );
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
                debug!("Skipping chmod for non-existent path: {}", p.display());
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
        debug!(
            "Bottle relocation finished with issues: {} chmod errors, {} Mach-O errors, {} IO errors in {}.",
            permission_errors,
            macho_errors,
            io_errors,
            install_dir.display()
        );
        if macho_errors > 0 {
            return Err(SpsError::InstallError(format!(
                "Bottle relocation failed due to {} Mach-O errors in {}",
                macho_errors,
                install_dir.display()
            )));
        }
    }
    Ok(())
}

fn codesign_path(target: &Path) -> Result<()> {
    debug!("Re‑signing: {}", target.display());
    let status = StdCommand::new("codesign")
        .args([
            "-s",
            "-",
            "--force",
            "--preserve-metadata=identifier,entitlements",
            target
                .to_str()
                .ok_or_else(|| SpsError::Generic("Non‑UTF8 path for codesign".into()))?,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| SpsError::Io(Arc::new(e)))?;
    if !status.success() {
        return Err(SpsError::CodesignError(format!(
            "codesign failed for {}",
            target.display()
        )));
    }
    Ok(())
}
fn write_text_file_atomic(original_path: &Path, content: &str) -> Result<()> {
    let dir = original_path.parent().ok_or_else(|| {
        SpsError::Generic(format!(
            "Cannot get parent directory for {}",
            original_path.display()
        ))
    })?;
    // Ensure the directory exists
    fs::create_dir_all(dir).map_err(|e| SpsError::Io(Arc::new(e)))?; // Use Arc::new

    let mut temp_file = NamedTempFile::new_in(dir)?;
    let temp_path = temp_file.path().to_path_buf(); // Store path before consuming temp_file

    // Write content
    temp_file.write_all(content.as_bytes())?;
    // Ensure data is flushed from application buffer to OS buffer
    temp_file.flush()?;
    // Attempt to sync data from OS buffer to disk (best effort)
    let _ = temp_file.as_file().sync_all();

    // Try to preserve original permissions
    let original_perms = fs::metadata(original_path).map(|m| m.permissions()).ok();

    // Atomically replace the original file with the temporary file
    temp_file.persist(original_path).map_err(|e| {
        error!(
            "Failed to persist/rename temporary text file {} over {}: {}",
            temp_path.display(), // Use stored path for logging
            original_path.display(),
            e.error // Log the underlying IO error
        );
        // Return the IO error wrapped in our error type
        SpsError::Io(Arc::new(e.error)) // Use Arc::new
    })?;

    // Restore original permissions if we captured them
    if let Some(perms) = original_perms {
        // Ignore errors setting permissions, best effort
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
                        let parts: Vec<&str> = version_part.split('.').collect();
                        match parts.len() {
                            1 => format!("{}.0.0", parts[0]), // e.g., perl@5 -> 5.0.0
                            2 => format!("{}.{}.0", parts[0], parts[1]), /* e.g., perl@5.30 -> */
                            // 5.30.0
                            _ => version_part.to_string(), // Already 3+ parts
                        }
                    } else {
                        format!("{version_part}.0.0") // e.g., perl@5 -> 5.0.0 (handles case with no
                                                      // dot)
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
        Err(_e) => {
            debug!("Failed to read opt directory during perl search: {}", _e);
        }
    }
    best.map(|(_, path)| path)
}

#[cfg(not(unix))]
fn find_brewed_perl(_prefix: &Path) -> Option<PathBuf> {
    None
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

    let llvm_dep_name = match formula.dependencies() {
        Ok(deps) => deps
            .iter()
            .find(|d| d.name.starts_with("llvm"))
            .map(|dep| dep.name.clone()),
        Err(e) => {
            warn!(
                "Could not check formula dependencies for LLVM symlink creation in {}: {}",
                formula.name(),
                e
            );
            return Ok(()); // Don't error, just skip
        }
    };

    // Proceed only if llvm_dep_name is Some
    let llvm_dep_name = match llvm_dep_name {
        Some(name) => name,
        None => {
            debug!(
                "Formula {} does not list an LLVM dependency.",
                formula.name()
            );
            return Ok(());
        }
    };

    let llvm_opt_path = config.formula_opt_path(&llvm_dep_name);
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
                // Log as warning, don't fail install
                warn!(
                    "Failed to create LLVM symlink {} -> {}: {}",
                    symlink_target_path.display(),
                    llvm_lib_path_in_opt.display(),
                    e
                );
            }
        }

        let rustlib_dir = install_dir.join("lib").join("rustlib");
        if rustlib_dir.is_dir() {
            if let Ok(entries) = fs::read_dir(&rustlib_dir) {
                for entry in entries.flatten() {
                    let triple_path = entry.path();
                    if triple_path.is_dir() {
                        let triple_lib_dir = triple_path.join("lib");
                        if triple_lib_dir.is_dir() {
                            let nested_symlink = triple_lib_dir.join(llvm_lib_filename);

                            if nested_symlink.exists() || nested_symlink.symlink_metadata().is_ok()
                            {
                                debug!(
                                    "Symlink or file already exists at {}, skipping.",
                                    nested_symlink.display()
                                );
                                continue;
                            }

                            if let Some(parent) = nested_symlink.parent() {
                                let _ = fs::create_dir_all(parent);
                            }
                            match symlink(&llvm_lib_path_in_opt, &nested_symlink) {
                                Ok(_) => debug!(
                                    "Created symlink {} -> {}",
                                    nested_symlink.display(),
                                    llvm_lib_path_in_opt.display()
                                ),
                                Err(e) => warn!(
                                    "Failed to create LLVM symlink {} -> {}: {}",
                                    nested_symlink.display(),
                                    llvm_lib_path_in_opt.display(),
                                    e
                                ),
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Applies a path change using install_name_tool as a fallback for Mach-O files
/// where the path replacement is too long for direct binary patching.
fn apply_install_name_tool_change(old_path: &str, new_path: &str, target: &Path) -> Result<()> {
    if !target.exists() {
        debug!(
            "Target {} does not exist, skipping install_name_tool fallback.",
            target.display()
        );
        return Ok(());
    }

    debug!(
        "Running install_name_tool -change {} {} {}",
        old_path,
        new_path,
        target.display()
    );

    let output = StdCommand::new("install_name_tool")
        .args(["-change", old_path, new_path, target.to_str().unwrap()])
        .output()
        .map_err(|e| SpsError::Io(Arc::new(e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("file not found")
            && !stderr.contains("no LC_LOAD_DYLIB command specifying file")
            && !stderr.contains("is not a Mach-O file")
            && !stderr.contains("object file format invalid")
            && !stderr.trim().is_empty()
        {
            error!(
                "install_name_tool -change failed unexpectedly for target {}: {}",
                target.display(),
                stderr.trim()
            );
            return Err(SpsError::InstallError(format!(
                "install_name_tool failed for {}: {}",
                target.display(),
                stderr.trim()
            )));
        } else {
            debug!(
                "install_name_tool -change: old path '{}' likely not found or target '{}' not relevant (stderr: {}).",
                old_path,
                target.display(),
                stderr.trim()
            );
            return Ok(()); // No changes made, skip re-signing
        }
    }

    // Re-sign the binary after making changes (required on Apple Silicon)
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        debug!(
            "Re-signing binary after install_name_tool change: {}",
            target.display()
        );
        codesign_path(target)?;
    }

    debug!(
        "Successfully applied install_name_tool fallback for {} -> {} in {}",
        old_path,
        new_path,
        target.display()
    );

    Ok(())
}
