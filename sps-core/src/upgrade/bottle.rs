// sps-core/src/upgrade/bottle.rs

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sps_common::config::Config;
use sps_common::error::{Result as SpsResult, SpsError};
use sps_common::model::formula::Formula;
use tracing::{debug, error};

use crate::check::installed::InstalledPackageInfo;
use crate::{install, uninstall};

/// Upgrades a formula that is installed from a bottle.
///
/// This involves:
/// 1. Uninstalling the old version of the formula.
/// 2. Installing the new bottle.
/// 3. Linking the new version.
pub async fn upgrade_bottle_formula(
    formula: &Formula,
    new_bottle_download_path: &Path,
    old_install_info: &InstalledPackageInfo,
    config: &Config,
    http_client: Arc<reqwest::Client>, /* Added for download_bottle if needed, though path is
                                        * pre-downloaded */
) -> SpsResult<PathBuf> {
    debug!(
        "Upgrading bottle formula {} from {} to {}",
        formula.name(),
        old_install_info.version,
        formula.version_str_full()
    );

    // 1. Uninstall the old version
    debug!(
        "Uninstalling old bottle version: {} at {}",
        old_install_info.version,
        old_install_info.path.display()
    );
    let uninstall_opts = uninstall::UninstallOptions { skip_zap: true }; // Zap is not relevant for formula upgrades
    uninstall::formula::uninstall_formula_artifacts(old_install_info, config, &uninstall_opts)
        .map_err(|e| {
            error!(
                "Failed to uninstall old version {} of formula {}: {}",
                old_install_info.version,
                formula.name(),
                e
            );
            SpsError::InstallError(format!(
                "Failed to uninstall old version during upgrade of {}: {e}",
                formula.name()
            ))
        })?;
    debug!("Successfully uninstalled old version of {}", formula.name());

    // 2. Install the new bottle
    // The new_bottle_download_path is already provided, so we call install_bottle directly.
    // If download was part of this function, http_client would be used.
    let _ = http_client; // Mark as used if not directly needed by install_bottle

    debug!(
        "Installing new bottle for {} from {}",
        formula.name(),
        new_bottle_download_path.display()
    );
    let installed_keg_path =
        install::bottle::exec::install_bottle(new_bottle_download_path, formula, config).map_err(
            |e| {
                error!(
                    "Failed to install new bottle for formula {}: {}",
                    formula.name(),
                    e
                );
                SpsError::InstallError(format!(
                    "Failed to install new bottle during upgrade of {}: {e}",
                    formula.name()
                ))
            },
        )?;
    debug!(
        "Successfully installed new bottle for {} to {}",
        formula.name(),
        installed_keg_path.display()
    );

    // 3. Link the new version (linking is handled by the worker after this function returns the
    //    path)
    // The install::bottle::exec::install_bottle writes the receipt, but linking is separate.
    // The worker will call link_formula_artifacts after this.

    Ok(installed_keg_path)
}
