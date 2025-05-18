// sps-core/src/upgrade/cask.rs

use std::path::Path;

use sps_common::config::Config;
use sps_common::error::{Result as SpsResult, SpsError};
use sps_common::model::cask::Cask;
use sps_common::pipeline::JobAction; // Required for install_cask
use tracing::{debug, error};

use crate::check::installed::InstalledPackageInfo;
use crate::{install, uninstall};

/// Upgrades a cask package.
///
/// This involves:
/// 1. Soft-uninstalling the old version of the cask (removes artifacts, marks manifest as
///    uninstalled).
/// 2. Installing the new version of the cask, which handles syncing for app bundles if applicable.
pub async fn upgrade_cask_package(
    cask: &Cask,
    new_cask_download_path: &Path,
    old_install_info: &InstalledPackageInfo,
    config: &Config,
) -> SpsResult<()> {
    debug!(
        "Upgrading cask {} from {} to {}",
        cask.token,
        old_install_info.version,
        cask.version.as_deref().unwrap_or("latest")
    );

    // 1. Soft-uninstall the old version
    // This removes linked artifacts and updates the old manifest's is_installed flag.
    // It does not remove the old Caskroom version directory itself yet.
    debug!(
        "Soft-uninstalling old cask version: {} at {}",
        old_install_info.version,
        old_install_info.path.display()
    );
    uninstall::cask::uninstall_cask_artifacts(old_install_info, config).map_err(|e| {
        error!(
            "Failed to soft-uninstall old version {} of cask {}: {}",
            old_install_info.version, cask.token, e
        );
        SpsError::InstallError(format!(
            "Failed to soft-uninstall old version during upgrade of {}: {e}",
            cask.token
        ))
    })?;
    debug!(
        "Successfully soft-uninstalled old version of {}",
        cask.token
    );

    // 2. Install the new version
    // The install_cask function, particularly install_app_from_staged,
    // should handle the upgrade logic (like syncing app data) when
    // passed the JobAction::Upgrade.
    debug!(
        "Installing new version for cask {} from {}",
        cask.token,
        new_cask_download_path.display()
    );

    let job_action_for_install = JobAction::Upgrade {
        from_version: old_install_info.version.clone(),
        old_install_path: old_install_info.path.clone(),
    };

    install::cask::install_cask(
        cask,
        new_cask_download_path,
        config,
        &job_action_for_install,
    )
    .map_err(|e| {
        error!(
            "Failed to install new version of cask {}: {}",
            cask.token, e
        );
        SpsError::InstallError(format!(
            "Failed to install new version during upgrade of {}: {e}",
            cask.token
        ))
    })?;
    debug!("Successfully installed new version of cask {}", cask.token);

    // Note: The old Caskroom version directory (e.g., /opt/sps/cask_room/foo/1.0)
    // is typically left behind after a soft uninstall + new install.
    // A separate `sps cleanup` command would be responsible for removing such old versions.
    // If immediate removal of the old Caskroom dir is desired here,
    // `uninstall::cask::zap_cask_artifacts` could be selectively used,
    // or a direct call to `remove_filesystem_artifact` on `old_install_info.path`
    // after ensuring all necessary data has been migrated by `install_app_from_staged`.
    // For now, we stick to the soft-uninstall + new install pattern.

    Ok(())
}
