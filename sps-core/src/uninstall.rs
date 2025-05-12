// sps-core/src/uninstall.rs
// Removed unused 'dirs' import
use std::path::Component;
use std::{
    fs,
    io,                    // Keep io for ErrorKind
    path::{Path, PathBuf}, // Combine PathBuf here
    process::{Command, Stdio},
    // error::Error as StdError, // Removed unused import
};

use lazy_static::lazy_static;
use regex::Regex;
use serde_json;
use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_common::model::artifact::InstalledArtifact;
use sps_common::model::cask::{Cask, ZapActionDetail};
use tracing::{debug, error, warn};
use trash; // Removed ZapStanza

use crate::build;
use crate::build::cask::CaskInstallManifest;
use crate::installed::InstalledPackageInfo;

lazy_static! {
    static ref VALID_PKGID_RE: Regex = Regex::new(r"^[a-zA-Z0-9._-]+$").unwrap();
    static ref VALID_LABEL_RE: Regex = Regex::new(r"^[a-zA-Z0-9._-]+$").unwrap();
    static ref VALID_SCRIPT_PATH_RE: Regex = Regex::new(r"^[a-zA-Z0-9/._-]+$").unwrap();
    static ref VALID_SIGNAL_RE: Regex = Regex::new(r"^[A-Z0-9]+$").unwrap();
    static ref VALID_BUNDLE_ID_RE: Regex =
        Regex::new(r"^[a-zA-Z0-9-]+(\.[a-zA-Z0-9-]+)+$").unwrap();
}

#[derive(Debug, Clone, Default)]
pub struct UninstallOptions {
    pub skip_zap: bool,
}

pub fn uninstall_formula_artifacts(
    info: &InstalledPackageInfo,
    config: &Config,
    _options: &UninstallOptions,
) -> Result<()> {
    debug!(
        "Uninstalling Formula artifacts for {} version {}",
        info.name, info.version
    );
    build::formula::link::unlink_formula_artifacts(&info.name, &info.version, config)?;
    if info.path.exists() {
        debug!("Removing formula keg directory: {}", info.path.display());
        let use_sudo = true;
        if !remove_filesystem_artifact(&info.path, use_sudo) {
            if info.path.exists() {
                error!(
                    "Failed remove keg {}: Check logs for sudo errors or other filesystem issues.",
                    info.path.display()
                );
                return Err(SpsError::InstallError(format!(
                    "Failed to remove keg directory: {}",
                    info.path.display()
                )));
            } else {
                debug!("Keg directory successfully removed (possibly with sudo).");
            }
        }
    } else {
        warn!(
            "Keg directory {} not found during uninstall.",
            info.path.display()
        );
    }
    Ok(())
}

pub fn uninstall_cask_artifacts(info: &InstalledPackageInfo, config: &Config) -> Result<()> {
    debug!(
        "Uninstalling Cask artifacts for {} version {}",
        info.name, info.version
    );
    let manifest_path = info.path.join("CASK_INSTALL_MANIFEST.json");
    let mut removal_errors: Vec<String> = Vec::new();

    if manifest_path.is_file() {
        debug!("Processing manifest: {}", manifest_path.display());
        match fs::read_to_string(&manifest_path) {
            Ok(manifest_str) => match serde_json::from_str::<CaskInstallManifest>(&manifest_str) {
                Ok(manifest) => {
                    debug!(
                        "Uninstalling {} artifacts listed in manifest...",
                        manifest.artifacts.len()
                    );
                    for artifact in manifest.artifacts.iter().rev() {
                        if !process_artifact_uninstall_core(artifact, config) {
                            removal_errors.push(format!("Failed: {artifact:?}"));
                        }
                    }
                }
                Err(e) => warn!(
                    "Failed to parse cask manifest {}: {}",
                    manifest_path.display(),
                    e
                ),
            },
            Err(e) => warn!(
                "Failed to read cask manifest {}: {}",
                manifest_path.display(),
                e
            ),
        }
    } else {
        warn!(
            "No CASK_INSTALL_MANIFEST.json found in {}. Cannot perform detailed uninstall.",
            info.path.display()
        );
    }

    if info.path.exists() {
        debug!("Removing cask version directory: {}", info.path.display());
        if let Err(e) = fs::remove_dir_all(&info.path) {
            error!(
                "Failed to remove cask directory {}: {}",
                info.path.display(),
                e
            );
            removal_errors.push(format!(
                "Failed to remove main dir: {}",
                info.path.display()
            ));
        } else {
            let parent_cask_dir = config.cask_dir(&info.name);
            cleanup_parent_cask_dir(&parent_cask_dir);
        }
    } else {
        warn!(
            "Cask directory {} not found during uninstall.",
            info.path.display()
        );
    }

    if removal_errors.is_empty() {
        Ok(())
    } else {
        Err(SpsError::InstallError(format!(
            "Errors during cask artifact removal for {}: {}",
            info.name,
            removal_errors.join("; ")
        )))
    }
}

// --- Helpers ---

#[cfg(target_os = "macos")]
use crate::macos::applescript;

fn process_artifact_uninstall_core(artifact: &InstalledArtifact, config: &Config) -> bool {
    debug!("Processing artifact removal: {:?}", artifact);
    match artifact {
        // --- Artifacts removed during STANDARD uninstall ---
        InstalledArtifact::AppBundle { path } => {
            debug!(
                "Standard uninstall: Removing AppBundle at {}",
                path.display()
            );

            // Attempt to quit the app before removing (P4 Fix)
            #[cfg(target_os = "macos")]
            {
                if path.exists() {
                    // Only try to quit if it exists
                    if let Err(e) = applescript::quit_app_gracefully(path) {
                        warn!(
                            "Attempt to gracefully quit app at {} failed or had issues: {} (proceeding with uninstall)",
                            path.display(),
                            e
                        );
                    }
                } else {
                    debug!(
                        "App bundle at {} does not exist, skipping quit attempt.",
                        path.display()
                    );
                }
            }

            let use_sudo =
                path.starts_with(config.applications_dir()) || path.starts_with("/Applications");
            remove_filesystem_artifact(path, use_sudo)
        }
        InstalledArtifact::BinaryLink { link_path, .. } => {
            debug!(
                "Standard uninstall: Removing BinaryLink {}",
                link_path.display()
            );
            remove_filesystem_artifact(link_path, false)
        }
        InstalledArtifact::ManpageLink { link_path, .. } => {
            debug!(
                "Standard uninstall: Removing ManpageLink {}",
                link_path.display()
            );
            remove_filesystem_artifact(link_path, false)
        }
        InstalledArtifact::PkgUtilReceipt { id } => {
            debug!("Standard uninstall: Forgetting PkgUtilReceipt {}", id);
            forget_pkgutil_receipt(id)
        }
        InstalledArtifact::Launchd { label, path } => {
            debug!("Standard uninstall: Unloading Launchd {}", label);
            unload_and_remove_launchd(label, path.as_deref())
        }

        // --- Artifacts IGNORED during STANDARD uninstall ---
        InstalledArtifact::MovedResource { path } => {
            debug!(
                "Standard uninstall: Ignoring MovedResource at {}",
                path.display()
            );
            true
        }
        InstalledArtifact::CaskroomLink { link_path, .. } => {
            debug!(
                "Standard uninstall: Ignoring CaskroomLink {} (removed with dir)",
                link_path.display()
            );
            true
        }
        InstalledArtifact::CaskroomReference { path } => {
            debug!(
                "Standard uninstall: Ignoring CaskroomReference {} (removed with dir)",
                path.display()
            );
            true
        }
    }
}

fn remove_filesystem_artifact(path: &Path, use_sudo: bool) -> bool {
    match path.symlink_metadata() {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            let is_real_dir = file_type.is_dir();

            debug!(
                "Removing filesystem artifact ({}) at: {}",
                if is_real_dir {
                    "directory"
                } else if file_type.is_symlink() {
                    "symlink"
                } else {
                    "file"
                },
                path.display()
            );

            let remove_op = || -> io::Result<()> {
                if is_real_dir {
                    fs::remove_dir_all(path)
                } else {
                    fs::remove_file(path)
                }
            };

            if let Err(e) = remove_op() {
                if use_sudo && e.kind() == io::ErrorKind::PermissionDenied {
                    warn!(
                        "Direct removal failed (Permission Denied). Trying with sudo rm -rf: {}",
                        path.display()
                    );
                    let output = Command::new("sudo").arg("rm").arg("-rf").arg(path).output();
                    match output {
                        Ok(out) if out.status.success() => {
                            debug!("Successfully removed {} with sudo.", path.display());
                            true
                        }
                        Ok(out) => {
                            error!(
                                "Failed to remove {} with sudo: {}",
                                path.display(),
                                String::from_utf8_lossy(&out.stderr).trim()
                            );
                            false
                        }
                        Err(sudo_err) => {
                            error!(
                                "Error executing sudo rm for {}: {}",
                                path.display(),
                                sudo_err
                            );
                            false
                        }
                    }
                } else if e.kind() != io::ErrorKind::NotFound {
                    error!("Failed to remove artifact {}: {}", path.display(), e);
                    false
                } else {
                    debug!("Artifact {} already removed.", path.display());
                    true
                }
            } else {
                debug!("Successfully removed artifact: {}", path.display());
                true
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            debug!("Artifact not found (already removed?): {}", path.display());
            true
        }
        Err(e) => {
            warn!(
                "Failed to get metadata for artifact {}: {}",
                path.display(),
                e
            );
            false
        }
    }
}

fn forget_pkgutil_receipt(id: &str) -> bool {
    if id.contains('/') || id.contains("..") {
        error!(
            "Invalid pkgutil receipt id contains disallowed characters: {}",
            id
        );
        return false;
    }
    debug!("Forgetting package receipt (requires sudo): {}", id);
    let output = Command::new("sudo")
        .arg("pkgutil")
        .arg("--forget")
        .arg(id)
        .output();
    match output {
        Ok(out) if out.status.success() => {
            debug!("Successfully forgot package receipt {}", id);
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !stderr.contains("No receipt for") {
                error!("Failed to forget package receipt {}: {}", id, stderr.trim());
            } else {
                debug!("Package receipt {} already forgotten or never existed.", id);
            }
            true
        }
        Err(e) => {
            error!("Failed to execute sudo pkgutil --forget {}: {}", id, e);
            false
        }
    }
}

fn unload_and_remove_launchd(label: &str, path: Option<&Path>) -> bool {
    if label.contains('/') || label.contains("..") {
        error!(
            "Invalid launchd label contains disallowed characters: {}",
            label
        );
        return false;
    }
    debug!("Unloading launchd service: {}", label);
    let unload_output = Command::new("launchctl")
        .arg("unload")
        .arg("-w") // -w removes the disabled key on unload
        .arg(label)
        .stderr(Stdio::piped()) // Capture stderr
        .output();

    match unload_output {
        Ok(out) if out.status.success() => {
            debug!("Successfully unloaded launchd service {}", label);
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // Ignore "Could not find specified service" or "service not loaded" errors
            if !stderr.contains("Could not find specified service")
                && !stderr.contains("service is not loaded")
            {
                warn!(
                    "launchctl unload {} failed (but proceeding): {}",
                    label,
                    stderr.trim()
                );
            } else {
                debug!("Launchd service {} already unloaded or not found.", label);
            }
        }
        Err(e) => {
            warn!(
                "Failed to execute launchctl unload {} (but proceeding): {}",
                label, e
            );
        }
    }

    // Attempt to remove the plist file if a path was provided or inferred
    if let Some(plist_path) = path {
        debug!("Removing launchd plist file: {}", plist_path.display());
        // Determine if sudo is needed based on path
        let use_sudo = plist_path.starts_with("/Library/LaunchDaemons")
            || plist_path.starts_with("/Library/LaunchAgents");
        if !remove_filesystem_artifact(plist_path, use_sudo) {
            // Log if removal failed, but don't necessarily fail the whole uninstall
            warn!("Failed to remove launchd plist: {}", plist_path.display());
            return false; // Indicate failure if plist removal fails
        }
    } else {
        debug!(
            "No path provided for launchd plist removal for label {}",
            label
        );
    }

    true // Return true if unload succeeded or was ignored, and plist removal succeeded or wasn't
         // needed
}

fn cleanup_parent_cask_dir(parent_cask_dir: &Path) {
    if parent_cask_dir.exists() {
        match std::fs::read_dir(parent_cask_dir) {
            Ok(mut entries) => {
                if entries.next().is_none() {
                    debug!(
                        "Parent cask directory {} is empty, removing it.",
                        parent_cask_dir.display()
                    );
                    if let Err(e) = std::fs::remove_dir(parent_cask_dir) {
                        warn!(
                            "Failed to remove empty parent cask directory {}: {}",
                            parent_cask_dir.display(),
                            e
                        );
                    }
                } else {
                    debug!(
                        "Parent cask directory {} is not empty, keeping it.",
                        parent_cask_dir.display()
                    );
                }
            }
            Err(e) => {
                warn!(
                    "Failed to read parent cask directory {} for cleanup check: {}",
                    parent_cask_dir.display(),
                    e
                );
            }
        }
    }
}

// --- Zap Helpers ---

// Helper function to expand tilde
fn expand_tilde(path_str: &str, home: &Path) -> PathBuf {
    if let Some(stripped) = path_str.strip_prefix("~/") {
        home.join(stripped)
    } else {
        PathBuf::from(path_str)
    }
}

fn is_safe_path(path: &Path, home: &Path, config: &Config) -> bool {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        warn!("Zap path rejected (contains '..'): {}", path.display());
        return false;
    }
    let allowed_roots = [
        home.join("Library"),
        home.join(".config"),
        PathBuf::from("/Applications"),
        PathBuf::from("/Library"),
        config.cache_dir.clone(),
    ];
    if allowed_roots.iter().any(|root| path.starts_with(root)) {
        if path == Path::new("/")
            || path == home
            || path == Path::new("/Applications")
            || path == Path::new("/Library")
        {
            warn!("Zap path rejected (too broad): {}", path.display());
            return false;
        }
        return true;
    }
    warn!(
        "Zap path rejected (outside allowed areas): {}",
        path.display()
    );
    false
}

fn trash_path(path: &Path) -> bool {
    match trash::delete(path) {
        Ok(_) => {
            debug!("Trashed: {}", path.display());
            true
        }
        Err(e) => {
            warn!(
                "Failed to trash {} (proceeding anyway): {}",
                path.display(),
                e
            );
            true
        }
    }
}

/// Removes the app from the private cask store and cleans up empty parent directories.
/// 
/// # Arguments
/// 
/// * `cask_token` - The cask token.
/// * `version` - The cask version.
/// * `app_name` - The optional app name to remove from the private store.
/// * `config` - The configuration.
/// 
/// # Returns
/// 
/// `true` if the cleanup was successful or if nothing needed to be cleaned up, `false` otherwise.
fn cleanup_private_store(
    cask_token: &str,
    version: &str,
    app_name: Option<&str>,
    config: &Config,
) -> bool {
    debug!(
        "Cleaning up private store for cask {} version {}",
        cask_token, version
    );
    
    let private_version_dir = config.private_cask_version_path(cask_token, version);
    
    // If the app name is provided, try to remove that specific app
    if let Some(app) = app_name {
        let app_path = private_version_dir.join(app);
        if app_path.exists() || app_path.symlink_metadata().is_ok() {
            debug!("Removing app from private store: {}", app_path.display());
            let _ = super::build::cask::helpers::remove_path_robustly(&app_path, config, false);
        }
    }
    
    // Remove any other content in the version directory
    if private_version_dir.exists() {
        debug!("Removing private store version directory: {}", private_version_dir.display());
        match fs::remove_dir_all(&private_version_dir) {
            Ok(_) => debug!("Successfully removed private store version directory"),
            Err(e) => {
                warn!("Failed to remove private store version directory: {}", e);
                return false;
            }
        }
    }
    
    // Clean up empty parent directories
    super::build::cask::helpers::cleanup_empty_parent_dirs_in_private_store(
        &private_version_dir,
        config.private_cask_store_base_dir(),
    );
    
    true
}

pub async fn zap_cask_artifacts(
    info: &InstalledPackageInfo,
    cask_def: &Cask,
    config: &Config,
) -> Result<()> {
    debug!("Starting zap process for cask: {}", cask_def.token);
    let home = config.home_dir();
    let cask_version_path = &info.path;
    let mut zap_errors: Vec<String> = Vec::new();
    
    // Read manifest to get primary app name for private store cleanup
    let mut primary_app_name = None;
    let manifest_path = info.path.join("CASK_INSTALL_MANIFEST.json");
    if manifest_path.is_file() {
        if let Ok(manifest_str) = fs::read_to_string(&manifest_path) {
            if let Ok(manifest) = serde_json::from_str::<CaskInstallManifest>(&manifest_str) {
                primary_app_name = manifest.primary_app_file_name.as_deref();
            }
        }
    }
    
    // Clean up the private store
    if !cleanup_private_store(&cask_def.token, &info.version, primary_app_name, config) {
        zap_errors.push(format!("Failed to clean up private store for {}", cask_def.token));
    }

    let zap_stanzas = match &cask_def.zap {
        Some(stanzas) => stanzas,
        None => {
            debug!(
                "No zap stanza found for cask {}. Zap finished.",
                cask_def.token
            );
            return Ok(());
        }
    };

    for stanza in zap_stanzas {
        for (action_key, action_detail) in &stanza.0 {
            debug!(
                "Processing zap action: {} = {:?}",
                action_key, action_detail
            );
            match action_detail {
                ZapActionDetail::Trash(paths) => {
                    for path_str in paths {
                        let target = expand_tilde(path_str, &home);
                        if is_safe_path(&target, &home, config) {
                            if !trash_path(&target) {
                                // Logged within trash_path if there's an actual error
                            }
                        } else {
                            zap_errors
                                .push(format!("Skipped unsafe trash path {}", target.display()));
                        }
                    }
                }
                ZapActionDetail::Delete(paths) => {
                    for path_str in paths {
                        let target = expand_tilde(path_str, &home);
                        if is_safe_path(&target, &home, config) {
                            let use_sudo = target.starts_with("/Library");
                            let exists_before =
                                target.exists() || target.symlink_metadata().is_ok();
                            if exists_before {
                                if !remove_filesystem_artifact(&target, use_sudo)
                                    && (target.exists() || target.symlink_metadata().is_ok())
                                {
                                    zap_errors
                                        .push(format!("Failed to delete {}", target.display()));
                                }
                            } else {
                                debug!(
                                    "Zap target {} not found, skipping removal.",
                                    target.display()
                                );
                            }
                        } else {
                            zap_errors
                                .push(format!("Skipped unsafe delete path {}", target.display()));
                        }
                    }
                }
                ZapActionDetail::Rmdir(paths) => {
                    for path_str in paths {
                        let target = expand_tilde(path_str, &home);
                        if is_safe_path(&target, &home, config) {
                            let use_sudo = target.starts_with("/Library");
                            let exists_before =
                                target.exists() || target.symlink_metadata().is_ok();
                            if exists_before {
                                if !target.is_dir() {
                                    warn!(
                                        "Zap rmdir target is not a directory: {}",
                                        target.display()
                                    );
                                } else if !remove_filesystem_artifact(&target, use_sudo)
                                    && (target.exists() || target.symlink_metadata().is_ok())
                                {
                                    zap_errors
                                        .push(format!("Failed to rmdir {}", target.display()));
                                }
                            } else {
                                debug!(
                                    "Zap target {} not found, skipping removal.",
                                    target.display()
                                );
                            }
                        } else {
                            zap_errors
                                .push(format!("Skipped unsafe rmdir path {}", target.display()));
                        }
                    }
                }
                ZapActionDetail::Pkgutil(ids_sv) => {
                    for id in ids_sv.clone().into_vec() {
                        if !forget_pkgutil_receipt(&id) {
                            // Error/warning logged within helper
                        }
                    }
                }
                ZapActionDetail::Launchctl(labels_sv) => {
                    for label in labels_sv.clone().into_vec() {
                        let potential_paths = vec![
                            home.join("Library/LaunchAgents")
                                .join(format!("{label}.plist")),
                            PathBuf::from("/Library/LaunchAgents").join(format!("{label}.plist")),
                            PathBuf::from("/Library/LaunchDaemons").join(format!("{label}.plist")),
                        ];
                        let path_to_try = potential_paths.into_iter().find(|p| p.exists());

                        if !unload_and_remove_launchd(&label, path_to_try.as_deref()) {
                            // Error/warning logged within helper
                        }
                    }
                }
                ZapActionDetail::Script { executable, args } => {
                    let script_rel_path_str = executable;
                    if !VALID_SCRIPT_PATH_RE.is_match(script_rel_path_str) {
                        error!(
                            "Zap script path contains invalid characters: '{}'. Skipping.",
                            script_rel_path_str
                        );
                        zap_errors.push(format!(
                            "Skipped invalid script path: {script_rel_path_str}"
                        ));
                        continue;
                    }
                    let script_rel_path = PathBuf::from(script_rel_path_str);
                    if script_rel_path.is_absolute()
                        || script_rel_path
                            .components()
                            .any(|c| matches!(c, Component::ParentDir))
                    {
                        error!(
                            "Zap script path is absolute or contains '..': '{}'. Skipping.",
                            script_rel_path.display()
                        );
                        zap_errors.push(format!(
                            "Skipped unsafe script path: {}",
                            script_rel_path.display()
                        ));
                        continue;
                    }
                    let script_full_path = cask_version_path.join(&script_rel_path);
                    if !script_full_path.exists() || !script_full_path.is_file() {
                        error!(
                            "Zap script path '{}' not found within cask directory '{}'. Skipping.",
                            script_rel_path.display(),
                            cask_version_path.display()
                        );
                        zap_errors.push(format!(
                            "Skipped non-existent script: {}",
                            script_rel_path.display()
                        ));
                        continue;
                    }
                    if !script_full_path.starts_with(cask_version_path) {
                        error!("Resolved zap script path '{}' is outside cask directory '{}'. Skipping.", script_full_path.display(), cask_version_path.display());
                        zap_errors.push(format!(
                            "Skipped external script: {}",
                            script_rel_path.display()
                        ));
                        continue;
                    }
                    let safe_args = args.clone().unwrap_or_default();
                    debug!(
                        "Running zap script: {} with args {:?}",
                        script_full_path.display(),
                        safe_args
                    );
                    let mut cmd = Command::new(&script_full_path);
                    cmd.args(&safe_args);
                    cmd.current_dir(cask_version_path);
                    cmd.stdout(Stdio::piped());
                    cmd.stderr(Stdio::piped());
                    match cmd.output() {
                        Ok(output) => {
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            if !output.status.success() {
                                error!(
                                    "Zap script '{}' failed with status {}: {}",
                                    script_rel_path.display(),
                                    output.status,
                                    stderr.trim()
                                );
                                if !stdout.trim().is_empty() {
                                    error!("Zap script stdout: {}", stdout.trim());
                                }
                                zap_errors.push(format!(
                                    "Zap script '{}' failed",
                                    script_rel_path.display()
                                ));
                            } else {
                                debug!(
                                    "Zap script '{}' executed successfully.",
                                    script_rel_path.display()
                                );
                                if !stdout.trim().is_empty() {
                                    debug!("Zap script stdout: {}", stdout.trim());
                                }
                                if !stderr.trim().is_empty() {
                                    debug!("Zap script stderr: {}", stderr.trim());
                                }
                            }
                        }
                        Err(e) => {
                            error!(
                                "Failed to execute zap script '{}': {}",
                                script_rel_path.display(),
                                e
                            );
                            zap_errors.push(format!(
                                "Failed to run zap script '{}'",
                                script_rel_path.display()
                            ));
                        }
                    }
                }
                ZapActionDetail::Signal(signals) => {
                    for signal_spec in signals {
                        let parts: Vec<&str> = signal_spec.splitn(2, '/').collect();
                        if parts.len() != 2 {
                            warn!("Invalid signal spec format '{}', expected SIGNAL/bundle.id. Skipping.", signal_spec);
                            zap_errors.push(format!("Invalid signal spec: {signal_spec}"));
                            continue;
                        }
                        let signal = parts[0].trim().to_uppercase();
                        let bundle_id = parts[1].trim();
                        if !VALID_SIGNAL_RE.is_match(&signal) {
                            warn!(
                                "Invalid signal name '{}' in spec '{}'. Skipping.",
                                signal, signal_spec
                            );
                            zap_errors.push(format!("Invalid signal name: {signal}"));
                            continue;
                        }
                        if !VALID_BUNDLE_ID_RE.is_match(bundle_id) {
                            warn!(
                                "Invalid bundle ID '{}' in spec '{}'. Skipping.",
                                bundle_id, signal_spec
                            );
                            zap_errors.push(format!("Invalid bundle ID: {bundle_id}"));
                            continue;
                        }
                        debug!("Sending signal {} to processes matching bundle ID '{}' (using pkill -f)", signal, bundle_id);
                        let mut cmd = Command::new("pkill");
                        cmd.arg(format!("-{signal}"));
                        cmd.arg("-f");
                        cmd.arg(bundle_id);
                        cmd.stdout(Stdio::null());
                        cmd.stderr(Stdio::piped());
                        match cmd.status() {
                            Ok(status) => {
                                if status.success() {
                                    debug!("Successfully sent signal {} via pkill to processes matching '{}'.", signal, bundle_id);
                                } else if status.code() == Some(1) {
                                    debug!("No running processes found matching bundle ID '{}' for signal {} via pkill.", bundle_id, signal);
                                } else {
                                    warn!("pkill command failed for signal {} / bundle ID '{}' with status: {}", signal, bundle_id, status);
                                }
                            }
                            Err(e) => {
                                error!(
                                    "Failed to execute pkill for signal {} / bundle ID '{}': {}",
                                    signal, bundle_id, e
                                );
                                zap_errors.push(format!("Failed to run pkill for signal {signal}"));
                            }
                        }
                    }
                }
            }
        }
    }

    if zap_errors.is_empty() {
        debug!(
            "Zap process completed successfully for cask: {}",
            cask_def.token
        );
        Ok(())
    } else {
        error!(
            "Zap process for {} completed with errors: {}",
            cask_def.token,
            zap_errors.join("; ")
        );
        Ok(())
    }
}
