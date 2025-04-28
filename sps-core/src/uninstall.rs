// sps-core/src/uninstall.rs
use std::{
    fs,
    io, // Keep io for ErrorKind
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
};

use serde_json;
use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use tracing::{debug, error, warn};

use crate::build;
use crate::build::cask::{CaskInstallManifest, InstalledArtifact};
use crate::installed::InstalledPackageInfo;

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
        fs::remove_dir_all(&info.path).map_err(|e| {
            error!("Failed remove keg {}: {}", info.path.display(), e);
            SpsError::Io(Arc::new(e))
        })?;
    } else {
        warn!(
            "Keg directory {} not found during uninstall.",
            info.path.display()
        );
    }
    Ok(())
}

pub fn uninstall_cask_artifacts(
    info: &InstalledPackageInfo,
    config: &Config,
    options: &UninstallOptions,
) -> Result<()> {
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
                        if options.skip_zap && is_zap_artifact(artifact, config) {
                            debug!("Skipping zap artifact: {:?}", artifact);
                            continue;
                        }
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

// --- Helpers (Full Implementations Moved from cli/uninstall.rs) ---

fn is_zap_artifact(artifact: &InstalledArtifact, config: &Config) -> bool {
    let home_lib = config.home_dir().join("Library");
    let user_app_support = home_lib.join("Application Support");
    let user_caches = home_lib.join("Caches");
    let user_logs = home_lib.join("Logs");
    let user_prefs = home_lib.join("Preferences");
    let sys_lib = std::path::PathBuf::from("/Library");
    let sys_app_support = sys_lib.join("Application Support");
    let sys_caches = sys_lib.join("Caches");
    let sys_logs = sys_lib.join("Logs");
    let sys_prefs = sys_lib.join("Preferences");
    let sys_launch_agents = sys_lib.join("LaunchAgents");
    let sys_launch_daemons = sys_lib.join("LaunchDaemons");

    match artifact {
        InstalledArtifact::App { path }
        | InstalledArtifact::CaskroomLink {
            target_path: path, ..
        }
        | InstalledArtifact::BinaryLink {
            target_path: path, ..
        }
        | InstalledArtifact::CaskroomReference { path } => {
            path.starts_with(&user_app_support)
                || path.starts_with(&user_caches)
                || path.starts_with(&user_logs)
                || path.starts_with(&user_prefs)
                || path.starts_with(&sys_app_support)
                || path.starts_with(&sys_caches)
                || path.starts_with(&sys_logs)
                || path.starts_with(&sys_prefs)
                || path.starts_with(&sys_launch_agents)
                || path.starts_with(&sys_launch_daemons)
                || path.to_string_lossy().contains("CrashReporter")
        }
        // ZapTarget artifacts are inherently part of the zap process
        InstalledArtifact::ZapTarget { .. } => true,
        InstalledArtifact::PkgUtilReceipt { .. } => true,
        InstalledArtifact::Launchd { .. } => true,
    }
}

fn process_artifact_uninstall_core(artifact: &InstalledArtifact, config: &Config) -> bool {
    debug!("Processing artifact removal: {:?}", artifact);
    match artifact {
        InstalledArtifact::App { path } => {
            let use_sudo =
                path.starts_with(config.applications_dir()) || path.starts_with("/Applications");
            remove_filesystem_artifact(path, use_sudo)
        }
        InstalledArtifact::CaskroomLink { link_path, .. } => {
            remove_filesystem_artifact(link_path, false)
        }
        InstalledArtifact::BinaryLink { link_path, .. } => {
            remove_filesystem_artifact(link_path, false)
        }
        InstalledArtifact::PkgUtilReceipt { id } => forget_pkgutil_receipt(id),
        InstalledArtifact::Launchd { label, path } => {
            unload_and_remove_launchd(label, path.as_deref())
        }
        InstalledArtifact::CaskroomReference { path: _ } => {
            debug!("Ignoring CaskroomReference artifact during detailed uninstall.");
            true
        }
        // Handle ZapTarget by performing the specified action
        InstalledArtifact::ZapTarget {
            target_path,
            action: _,
        } => {
            debug!("Processing ZapTarget on {}", target_path.display());
            let use_sudo =
                target_path.starts_with("/Library") || target_path.starts_with("/Applications");
            remove_filesystem_artifact(target_path, use_sudo)
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
    if let Some(plist_path) = path {
        if plist_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            error!(
                "Invalid launchd plist path contains '..': {}",
                plist_path.display()
            );
            return false;
        }
    }

    debug!("Unloading launchd agent/daemon (requires sudo): {}", label);
    let mut overall_success = true;

    let unload_output = Command::new("sudo")
        .arg("launchctl")
        .arg("unload")
        .arg("-w")
        .arg(label)
        .stderr(Stdio::piped())
        .output();

    match unload_output {
        Ok(out) => {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stderr.contains("Could not find specified service")
                    && !stderr.contains("service not loaded")
                {
                    warn!("Failed to unload launchd item {}: {}", label, stderr.trim());
                } else {
                    debug!("Launchd item {} already unloaded or not found.", label);
                }
            } else {
                debug!("Successfully unloaded launchd item {}.", label);
            }
        }
        Err(e) => {
            error!(
                "Failed to execute sudo launchctl unload for {}: {}",
                label, e
            );
            overall_success = false;
        }
    }

    if let Some(plist_path) = path {
        let use_sudo = plist_path.starts_with("/Library/LaunchDaemons")
            || plist_path.starts_with("/Library/LaunchAgents");
        debug!(
            "Attempting removal of launchd plist: {}",
            plist_path.display()
        );
        if !remove_filesystem_artifact(plist_path, use_sudo) && plist_path.exists() {
            warn!(
                "Failed to remove launchd plist file: {}",
                plist_path.display()
            );
            overall_success = false;
        }
    }
    overall_success
}

fn cleanup_parent_cask_dir(parent_cask_dir: &Path) {
    if parent_cask_dir.is_dir() {
        match std::fs::read_dir(parent_cask_dir) {
            Ok(mut entries) => {
                if entries.next().is_none() {
                    debug!(
                        "Removing empty parent cask directory: {}",
                        parent_cask_dir.display()
                    );
                    if let Err(e) = std::fs::remove_dir(parent_cask_dir) {
                        warn!(
                            "Failed to remove empty parent cask directory {}: {}",
                            parent_cask_dir.display(),
                            e
                        );
                    }
                }
            }
            Err(e) => warn!(
                "Failed to read parent cask directory {} to check if empty: {}",
                parent_cask_dir.display(),
                e
            ),
        }
    }
}
