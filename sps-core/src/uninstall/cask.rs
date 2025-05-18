// sps-core/src/uninstall/cask.rs
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use lazy_static::lazy_static;
use regex::Regex;
// serde_json is used for CaskInstallManifest deserialization
use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_common::model::artifact::InstalledArtifact;
use sps_common::model::cask::{Cask, ZapActionDetail};
use tracing::{debug, error, warn};
use trash; // This will be used by trash_path

// Import helpers from the common module within the uninstall scope
use super::common::{expand_tilde, is_safe_path, remove_filesystem_artifact};
use crate::check::installed::InstalledPackageInfo;
// Corrected import path if install::cask::helpers is where it lives now
use crate::install::cask::helpers::{
    cleanup_empty_parent_dirs_in_private_store,
    remove_path_robustly as remove_path_robustly_from_install_helpers,
};
use crate::install::cask::CaskInstallManifest;
#[cfg(target_os = "macos")]
use crate::utils::applescript;

lazy_static! {
    static ref VALID_PKGID_RE: Regex = Regex::new(r"^[a-zA-Z0-9._-]+$").unwrap();
    static ref VALID_LABEL_RE: Regex = Regex::new(r"^[a-zA-Z0-9._-]+$").unwrap();
    static ref VALID_SCRIPT_PATH_RE: Regex = Regex::new(r"^[a-zA-Z0-9/._-]+$").unwrap();
    static ref VALID_SIGNAL_RE: Regex = Regex::new(r"^[A-Z0-9]+$").unwrap();
    static ref VALID_BUNDLE_ID_RE: Regex =
        Regex::new(r"^[a-zA-Z0-9-]+(\.[a-zA-Z0-9-]+)+$").unwrap();
}

/// Performs a "soft" uninstall for a Cask.
/// It processes the `CASK_INSTALL_MANIFEST.json` to remove linked artifacts
/// and then updates the manifest to mark the cask as not currently installed.
/// The Cask's versioned directory in the Caskroom is NOT removed.
pub fn uninstall_cask_artifacts(info: &InstalledPackageInfo, config: &Config) -> Result<()> {
    debug!(
        "Soft uninstalling Cask artifacts for {} version {}",
        info.name, info.version
    );
    let manifest_path = info.path.join("CASK_INSTALL_MANIFEST.json");
    let mut removal_errors: Vec<String> = Vec::new();

    if manifest_path.is_file() {
        debug!(
            "Processing manifest for soft uninstall: {}",
            manifest_path.display()
        );
        match fs::read_to_string(&manifest_path) {
            Ok(manifest_str) => match serde_json::from_str::<CaskInstallManifest>(&manifest_str) {
                Ok(mut manifest) => {
                    if !manifest.is_installed {
                        debug!("Cask {} version {} is already marked as uninstalled in manifest. Nothing to do for soft uninstall.", info.name, info.version);
                        return Ok(());
                    }

                    debug!(
                        "Soft uninstalling {} artifacts listed in manifest for {} {}...",
                        manifest.artifacts.len(),
                        info.name,
                        info.version
                    );
                    for artifact in manifest.artifacts.iter().rev() {
                        if !process_artifact_uninstall_core(artifact, config, false) {
                            removal_errors.push(format!("Failed to remove artifact: {artifact:?}"));
                        }
                    }

                    manifest.is_installed = false;
                    match fs::File::create(&manifest_path) {
                        Ok(file) => {
                            let writer = std::io::BufWriter::new(file);
                            if let Err(e) = serde_json::to_writer_pretty(writer, &manifest) {
                                warn!(
                                    "Failed to update manifest {}: {}",
                                    manifest_path.display(),
                                    e
                                );
                            } else {
                                debug!(
                                    "Manifest updated successfully for soft uninstall: {}",
                                    manifest_path.display()
                                );
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Failed to open manifest for writing (soft uninstall) at {}: {}",
                                manifest_path.display(),
                                e
                            );
                        }
                    }
                }
                Err(e) => warn!(
                    "Failed to parse cask manifest {}: {}. Cannot perform detailed soft uninstall.",
                    manifest_path.display(),
                    e
                ),
            },
            Err(e) => warn!(
                "Failed to read cask manifest {}: {}. Cannot perform detailed soft uninstall.",
                manifest_path.display(),
                e
            ),
        }
    } else {
        warn!(
            "No CASK_INSTALL_MANIFEST.json found in {}. Cannot perform detailed soft uninstall for {} {}.",
            info.path.display(),
            info.name,
            info.version
        );
    }

    if removal_errors.is_empty() {
        Ok(())
    } else {
        Err(SpsError::InstallError(format!(
            "Errors during cask artifact soft removal for {}: {}",
            info.name,
            removal_errors.join("; ")
        )))
    }
}

/// Performs a "zap" uninstall for a Cask, removing files defined in `zap` stanzas
/// and cleaning up the private store. Also marks the cask as uninstalled in its manifest.
pub async fn zap_cask_artifacts(
    info: &InstalledPackageInfo,
    cask_def: &Cask,
    config: &Config,
) -> Result<()> {
    debug!("Starting ZAP process for cask: {}", cask_def.token);
    let home = config.home_dir();
    let cask_version_path_in_caskroom = &info.path;
    let mut zap_errors: Vec<String> = Vec::new();

    let mut primary_app_name_from_manifest: Option<String> = None;
    let manifest_path = cask_version_path_in_caskroom.join("CASK_INSTALL_MANIFEST.json");

    if manifest_path.is_file() {
        match fs::read_to_string(&manifest_path) {
            Ok(manifest_str) => match serde_json::from_str::<CaskInstallManifest>(&manifest_str) {
                Ok(mut manifest) => {
                    primary_app_name_from_manifest = manifest.primary_app_file_name.clone();
                    if manifest.is_installed {
                        manifest.is_installed = false;
                        if let Ok(file) = fs::File::create(&manifest_path) {
                            let writer = std::io::BufWriter::new(file);
                            if let Err(e) = serde_json::to_writer_pretty(writer, &manifest) {
                                warn!(
                                    "Failed to update manifest during zap for {}: {}",
                                    manifest_path.display(),
                                    e
                                );
                            }
                        } else {
                            warn!(
                                "Failed to open manifest for writing during zap at {}",
                                manifest_path.display()
                            );
                        }
                    }
                }
                Err(e) => warn!(
                    "Failed to parse manifest during zap {}: {}",
                    manifest_path.display(),
                    e
                ),
            },
            Err(e) => warn!(
                "Failed to read manifest during zap {}: {}",
                manifest_path.display(),
                e
            ),
        }
    } else {
        warn!("No manifest found at {} during zap. Private store cleanup might be incomplete if app name changed.", manifest_path.display());
    }

    if !cleanup_private_store(
        &cask_def.token,
        &info.version,
        primary_app_name_from_manifest.as_deref(),
        config,
    ) {
        let msg = format!(
            "Failed to clean up private store for cask {} version {}",
            cask_def.token, info.version
        );
        warn!("{}", msg);
        zap_errors.push(msg);
    }

    let zap_stanzas = match &cask_def.zap {
        Some(stanzas) => stanzas,
        None => {
            debug!("No zap stanza found for cask {}", cask_def.token);
            // Proceed to Caskroom cleanup even if no specific zap actions
            if !remove_filesystem_artifact(cask_version_path_in_caskroom, true) {
                // use_sudo = true for Caskroom
                if cask_version_path_in_caskroom.exists() {
                    zap_errors.push(format!(
                        "Failed to remove Caskroom version directory during zap: {}",
                        cask_version_path_in_caskroom.display()
                    ));
                }
            }
            if let Some(parent_token_dir) = cask_version_path_in_caskroom.parent() {
                if parent_token_dir.exists() && parent_token_dir.is_dir() {
                    match fs::read_dir(parent_token_dir) {
                        Ok(mut entries) => {
                            if entries.next().is_none()
                                && !remove_filesystem_artifact(parent_token_dir, true)
                                && parent_token_dir.exists()
                            {
                                warn!("Failed to remove empty Caskroom token directory during zap: {}", parent_token_dir.display());
                            }
                        }
                        Err(e) => warn!(
                            "Failed to read Caskroom token dir {} during zap: {}",
                            parent_token_dir.display(),
                            e
                        ),
                    }
                }
            }
            return if zap_errors.is_empty() {
                Ok(())
            } else {
                Err(SpsError::Generic(zap_errors.join("; ")))
            };
        }
    };

    for stanza_map in zap_stanzas {
        for (action_key, action_detail) in &stanza_map.0 {
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
                                // Logged within trash_path
                            }
                        } else {
                            zap_errors
                                .push(format!("Skipped unsafe trash path {}", target.display()));
                        }
                    }
                }
                ZapActionDetail::Delete(paths) | ZapActionDetail::Rmdir(paths) => {
                    for path_str in paths {
                        let target = expand_tilde(path_str, &home);
                        if is_safe_path(&target, &home, config) {
                            let use_sudo = target.starts_with("/Library")
                                || target.starts_with("/Applications");
                            let exists_before =
                                target.exists() || target.symlink_metadata().is_ok();
                            if exists_before {
                                if action_key == "rmdir" && !target.is_dir() {
                                    warn!("Zap rmdir target is not a directory: {}. Attempting as file delete.", target.display());
                                }
                                if !remove_filesystem_artifact(&target, use_sudo)
                                    && (target.exists() || target.symlink_metadata().is_ok())
                                {
                                    zap_errors.push(format!(
                                        "Failed to {} {}",
                                        action_key,
                                        target.display()
                                    ));
                                }
                            } else {
                                debug!(
                                    "Zap target {} not found, skipping removal.",
                                    target.display()
                                );
                            }
                        } else {
                            zap_errors.push(format!(
                                "Skipped unsafe {} path {}",
                                action_key,
                                target.display()
                            ));
                        }
                    }
                }
                ZapActionDetail::Pkgutil(ids_sv) => {
                    for id in ids_sv.clone().into_vec() {
                        if !VALID_PKGID_RE.is_match(&id) {
                            warn!("Invalid pkgutil ID format for zap: '{}'. Skipping.", id);
                            zap_errors.push(format!("Invalid pkgutil ID: {id}"));
                            continue;
                        }
                        if !forget_pkgutil_receipt(&id) {
                            // Error logged in helper
                        }
                    }
                }
                ZapActionDetail::Launchctl(labels_sv) => {
                    for label in labels_sv.clone().into_vec() {
                        if !VALID_LABEL_RE.is_match(&label) {
                            warn!(
                                "Invalid launchctl label format for zap: '{}'. Skipping.",
                                label
                            );
                            zap_errors.push(format!("Invalid launchctl label: {label}"));
                            continue;
                        }
                        let potential_paths = vec![
                            home.join("Library/LaunchAgents")
                                .join(format!("{label}.plist")),
                            PathBuf::from("/Library/LaunchAgents").join(format!("{label}.plist")),
                            PathBuf::from("/Library/LaunchDaemons").join(format!("{label}.plist")),
                        ];
                        let path_to_try = potential_paths.into_iter().find(|p| p.exists());
                        if !unload_and_remove_launchd(&label, path_to_try.as_deref()) {
                            // Error logged in helper
                        }
                    }
                }
                ZapActionDetail::Script { executable, args } => {
                    let script_path_str = executable;
                    if !VALID_SCRIPT_PATH_RE.is_match(script_path_str) {
                        error!(
                            "Zap script path contains invalid characters: '{}'. Skipping.",
                            script_path_str
                        );
                        zap_errors.push(format!("Skipped invalid script path: {script_path_str}"));
                        continue;
                    }
                    let script_full_path = PathBuf::from(script_path_str);
                    if !script_full_path.exists() {
                        if !script_full_path.is_absolute() {
                            if let Ok(found_path) = which::which(&script_full_path) {
                                debug!(
                                    "Found zap script {} in PATH: {}",
                                    script_full_path.display(),
                                    found_path.display()
                                );
                                run_zap_script(
                                    &found_path,
                                    args.as_ref().map(|v| v.as_slice()),
                                    &mut zap_errors,
                                );
                            } else {
                                error!(
                                    "Zap script '{}' not found (absolute or in PATH). Skipping.",
                                    script_full_path.display()
                                );
                                zap_errors.push(format!(
                                    "Zap script not found: {}",
                                    script_full_path.display()
                                ));
                            }
                        } else {
                            error!(
                                "Absolute zap script path '{}' not found. Skipping.",
                                script_full_path.display()
                            );
                            zap_errors.push(format!(
                                "Zap script not found: {}",
                                script_full_path.display()
                            ));
                        }
                        continue;
                    }
                    run_zap_script(
                        &script_full_path,
                        args.as_ref().map(|v| v.as_slice()),
                        &mut zap_errors,
                    );
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
                        let bundle_id_or_pattern = parts[1].trim();

                        if !VALID_SIGNAL_RE.is_match(&signal) {
                            warn!(
                                "Invalid signal name '{}' in spec '{}'. Skipping.",
                                signal, signal_spec
                            );
                            zap_errors.push(format!("Invalid signal name: {signal}"));
                            continue;
                        }

                        debug!("Sending signal {} to processes matching ID/pattern '{}' (using pkill -f)", signal, bundle_id_or_pattern);
                        let mut cmd = Command::new("pkill");
                        cmd.arg(format!("-{signal}")); // Standard signal format for pkill
                        cmd.arg("-f");
                        cmd.arg(bundle_id_or_pattern);
                        cmd.stdout(Stdio::null()).stderr(Stdio::piped());
                        match cmd.status() {
                            Ok(status) => {
                                if status.success() {
                                    debug!("Successfully sent signal {} via pkill to processes matching '{}'.", signal, bundle_id_or_pattern);
                                } else if status.code() == Some(1) {
                                    debug!("No running processes found matching ID/pattern '{}' for signal {} via pkill.", bundle_id_or_pattern, signal);
                                } else {
                                    warn!("pkill command failed for signal {} / ID/pattern '{}' with status: {}", signal, bundle_id_or_pattern, status);
                                }
                            }
                            Err(e) => {
                                error!(
                                    "Failed to execute pkill for signal {} / ID/pattern '{}': {}",
                                    signal, bundle_id_or_pattern, e
                                );
                                zap_errors.push(format!("Failed to run pkill for signal {signal}"));
                            }
                        }
                    }
                }
            }
        }
    }

    debug!(
        "Zap: Removing Caskroom version directory: {}",
        cask_version_path_in_caskroom.display()
    );
    if !remove_filesystem_artifact(cask_version_path_in_caskroom, true)
        && cask_version_path_in_caskroom.exists()
    {
        let msg = format!(
            "Failed to remove Caskroom version directory during zap: {}",
            cask_version_path_in_caskroom.display()
        );
        error!("{}", msg);
        zap_errors.push(msg);
    }

    if let Some(parent_token_dir) = cask_version_path_in_caskroom.parent() {
        if parent_token_dir.exists() && parent_token_dir.is_dir() {
            match fs::read_dir(parent_token_dir) {
                Ok(mut entries) => {
                    if entries.next().is_none() {
                        debug!(
                            "Zap: Removing empty Caskroom token directory: {}",
                            parent_token_dir.display()
                        );
                        if !remove_filesystem_artifact(parent_token_dir, true)
                            && parent_token_dir.exists()
                        {
                            warn!(
                                "Failed to remove empty Caskroom token directory during zap: {}",
                                parent_token_dir.display()
                            );
                        }
                    }
                }
                Err(e) => warn!(
                    "Failed to read Caskroom token directory {} during zap cleanup: {}",
                    parent_token_dir.display(),
                    e
                ),
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
        Err(SpsError::InstallError(format!(
            "Zap for {} failed with errors: {}",
            cask_def.token,
            zap_errors.join("; ")
        )))
    }
}

fn process_artifact_uninstall_core(
    artifact: &InstalledArtifact,
    config: &Config,
    use_sudo_for_zap: bool,
) -> bool {
    debug!("Processing artifact removal: {:?}", artifact);
    match artifact {
        InstalledArtifact::AppBundle { path } => {
            debug!("Uninstall: Removing AppBundle at {}", path.display());
            match path.symlink_metadata() {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    debug!("AppBundle at {} is a symlink; unlinking.", path.display());
                    match std::fs::remove_file(path) {
                        Ok(_) => true,
                        Err(e) => {
                            warn!("Failed to unlink symlink at {}: {}", path.display(), e);
                            false
                        }
                    }
                }
                Ok(metadata) if metadata.file_type().is_dir() => {
                    #[cfg(target_os = "macos")]
                    {
                        if path.exists() {
                            if let Err(e) = applescript::quit_app_gracefully(path) {
                                warn!(
                                    "Attempt to gracefully quit app at {} failed: {} (proceeding)",
                                    path.display(),
                                    e
                                );
                            }
                        } else {
                            debug!(
                                "App bundle at {} does not exist, skipping quit.",
                                path.display()
                            );
                        }
                    }
                    let use_sudo = path.starts_with(config.applications_dir())
                        || path.starts_with("/Applications")
                        || use_sudo_for_zap;
                    remove_filesystem_artifact(path, use_sudo)
                }
                Ok(_) | Err(_) => {
                    let use_sudo = path.starts_with(config.applications_dir())
                        || path.starts_with("/Applications")
                        || use_sudo_for_zap;
                    remove_filesystem_artifact(path, use_sudo)
                }
            }
        }
        InstalledArtifact::BinaryLink { link_path, .. }
        | InstalledArtifact::ManpageLink { link_path, .. }
        | InstalledArtifact::CaskroomLink { link_path, .. } => {
            debug!("Uninstall: Removing link at {}", link_path.display());
            remove_filesystem_artifact(link_path, use_sudo_for_zap)
        }
        InstalledArtifact::PkgUtilReceipt { id } => {
            debug!("Uninstall: Forgetting PkgUtilReceipt {}", id);
            forget_pkgutil_receipt(id)
        }
        InstalledArtifact::Launchd { label, path } => {
            debug!("Uninstall: Unloading Launchd {} (path: {:?})", label, path);
            unload_and_remove_launchd(label, path.as_deref())
        }
        InstalledArtifact::MovedResource { path } => {
            debug!("Uninstall: Removing MovedResource at {}", path.display());
            remove_filesystem_artifact(path, use_sudo_for_zap)
        }
        InstalledArtifact::CaskroomReference { path } => {
            debug!(
                "Uninstall: Removing CaskroomReference at {}",
                path.display()
            );
            remove_filesystem_artifact(path, use_sudo_for_zap)
        }
    }
}

fn forget_pkgutil_receipt(id: &str) -> bool {
    if !VALID_PKGID_RE.is_match(id) {
        error!("Invalid pkgutil ID format: '{}'. Skipping forget.", id);
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
            if !stderr.contains("No receipt for") && !stderr.trim().is_empty() {
                error!("Failed to forget package receipt {}: {}", id, stderr.trim());
                // Return false if pkgutil fails for a reason other than "No receipt"
                return false;
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
    if !VALID_LABEL_RE.is_match(label) {
        error!(
            "Invalid launchd label format: '{}'. Skipping unload/remove.",
            label
        );
        return false;
    }
    debug!("Unloading launchd service (if loaded): {}", label);
    let unload_output = Command::new("launchctl")
        .arg("unload")
        .arg("-w")
        .arg(label)
        .stderr(Stdio::piped())
        .output();

    let mut unload_successful_or_not_loaded = false;
    match unload_output {
        Ok(out) if out.status.success() => {
            debug!("Successfully unloaded launchd service {}", label);
            unload_successful_or_not_loaded = true;
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("Could not find specified service")
                || stderr.contains("service is not loaded")
                || stderr.trim().is_empty()
            {
                debug!("Launchd service {} already unloaded or not found.", label);
                unload_successful_or_not_loaded = true;
            } else {
                warn!(
                    "launchctl unload {} failed (but proceeding with plist removal attempt): {}",
                    label,
                    stderr.trim()
                );
                // Proceed to try plist removal even if unload reports other errors
            }
        }
        Err(e) => {
            warn!("Failed to execute launchctl unload {} (but proceeding with plist removal attempt): {}", label, e);
        }
    }

    if let Some(plist_path) = path {
        if plist_path.exists() {
            debug!("Removing launchd plist file: {}", plist_path.display());
            let use_sudo = plist_path.starts_with("/Library/LaunchDaemons")
                || plist_path.starts_with("/Library/LaunchAgents");
            if !remove_filesystem_artifact(plist_path, use_sudo) {
                warn!("Failed to remove launchd plist: {}", plist_path.display());
                return false; // If plist removal fails, consider the operation failed
            }
        } else {
            debug!(
                "Launchd plist path {} does not exist, skip removal.",
                plist_path.display()
            );
        }
    } else {
        debug!(
            "No path provided for launchd plist removal for label {}",
            label
        );
    }
    unload_successful_or_not_loaded // Success depends on unload and optional plist removal
}

fn trash_path(path: &Path) -> bool {
    if !path.exists() && path.symlink_metadata().is_err() {
        debug!("Path for trashing not found: {}", path.display());
        return true;
    }
    match trash::delete(path) {
        Ok(_) => {
            debug!("Trashed: {}", path.display());
            true
        }
        Err(e) => {
            warn!("Failed to trash {} (proceeding anyway): {}. This might require manual cleanup or installing a 'trash' utility.", path.display(), e);
            true
        }
    }
}

/// Helper for zap scripts.
fn run_zap_script(script_path: &Path, args: Option<&[String]>, errors: &mut Vec<String>) {
    debug!(
        "Running zap script: {} with args {:?}",
        script_path.display(),
        args.unwrap_or_default()
    );
    let mut cmd = Command::new(script_path);
    if let Some(script_args) = args {
        cmd.args(script_args);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    match cmd.output() {
        Ok(output) => {
            log_command_output(
                "Zap script",
                &script_path.display().to_string(),
                &output,
                errors,
            );
        }
        Err(e) => {
            let msg = format!(
                "Failed to execute zap script '{}': {}",
                script_path.display(),
                e
            );
            error!("{}", msg);
            errors.push(msg);
        }
    }
}

/// Logs the output of a command and adds to error list if it failed.
fn log_command_output(
    context: &str,
    command_str: &str,
    output: &std::process::Output,
    errors: &mut Vec<String>,
) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        error!(
            "{} '{}' failed with status {}: {}",
            context,
            command_str,
            output.status,
            stderr.trim()
        );
        if !stdout.trim().is_empty() {
            error!("{} stdout: {}", context, stdout.trim());
        }
        errors.push(format!("{context} '{command_str}' failed"));
    } else {
        debug!("{} '{}' executed successfully.", context, command_str);
        if !stdout.trim().is_empty() {
            debug!("{} stdout: {}", context, stdout.trim());
        }
        if !stderr.trim().is_empty() {
            debug!("{} stderr: {}", context, stderr.trim());
        }
    }
}

// Helper function specifically for cleaning up the private store.
// This was originally inside zap_cask_artifacts.
fn cleanup_private_store(
    cask_token: &str,
    version: &str,
    app_name: Option<&str>, // The actual .app name, not the token
    config: &Config,
) -> bool {
    debug!(
        "Cleaning up private store for cask {} version {}",
        cask_token, version
    );

    let private_version_dir = config.cask_store_version_path(cask_token, version);

    if let Some(app) = app_name {
        let app_path_in_private_store = private_version_dir.join(app);
        if app_path_in_private_store.exists()
            || app_path_in_private_store.symlink_metadata().is_ok()
        {
            debug!(
                "Removing app from private store: {}",
                app_path_in_private_store.display()
            );
            // Use the helper from install::cask::helpers, assuming it's correctly located and
            // public
            if !remove_path_robustly_from_install_helpers(&app_path_in_private_store, config, false)
            {
                // use_sudo=false for private store
                warn!(
                    "Failed to remove app from private store: {}",
                    app_path_in_private_store.display()
                );
                // Potentially return false or collect errors, depending on desired strictness
            }
        }
    }

    // After attempting to remove specific app, remove the version directory if it exists
    // This also handles cases where app_name was None.
    if private_version_dir.exists() {
        debug!(
            "Removing private store version directory: {}",
            private_version_dir.display()
        );
        match fs::remove_dir_all(&private_version_dir) {
            Ok(_) => debug!(
                "Successfully removed private store version directory {}",
                private_version_dir.display()
            ),
            Err(e) => {
                warn!(
                    "Failed to remove private store version directory {}: {}",
                    private_version_dir.display(),
                    e
                );
                return false; // If the version dir removal fails, consider it a failure
            }
        }
    }

    // Clean up empty parent token directory.
    cleanup_empty_parent_dirs_in_private_store(
        &private_version_dir, // Start from the version dir (or its parent if it was just removed)
        &config.cask_store_dir(),
    );

    true
}
