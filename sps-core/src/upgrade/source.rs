// sps-core/src/upgrade/source.rs

use std::path::{Path, PathBuf};

use sps_common::config::Config;
use sps_common::error::{Result as SpsResult, SpsError};
use sps_common::model::formula::Formula;
use tracing::{debug, error};

use crate::check::installed::InstalledPackageInfo;
use crate::{build, uninstall};

/// Upgrades a formula that was/will be installed from source.
///
/// This involves:
/// 1. Uninstalling the old version of the formula.
/// 2. Building and installing the new version from source.
/// 3. Linking the new version.
pub async fn upgrade_source_formula(
    formula: &Formula,
    new_source_download_path: &Path,
    old_install_info: &InstalledPackageInfo,
    config: &Config,
    all_installed_dependency_paths: &[PathBuf], // For build environment
) -> SpsResult<PathBuf> {
    debug!(
        "Upgrading source-built formula {} from {} to {}",
        formula.name(),
        old_install_info.version,
        formula.version_str_full()
    );

    // 1. Uninstall the old version
    debug!(
        "Uninstalling old source-built version: {} at {}",
        old_install_info.version,
        old_install_info.path.display()
    );
    let uninstall_opts = uninstall::UninstallOptions { skip_zap: true };
    uninstall::formula::uninstall_formula_artifacts(old_install_info, config, &uninstall_opts)
        .map_err(|e| {
            error!(
                "Failed to uninstall old version {} of formula {}: {}",
                old_install_info.version,
                formula.name(),
                e
            );
            SpsError::InstallError(format!(
                "Failed to uninstall old version during source upgrade of {}: {e}",
                formula.name()
            ))
        })?;
    debug!(
        "Successfully uninstalled old source-built version of {}",
        formula.name()
    );

    // 2. Build and install the new version from source
    debug!(
        "Building new version of {} from source path {}",
        formula.name(),
        new_source_download_path.display()
    );
    let installed_keg_path = build::compile::build_from_source(
        new_source_download_path,
        formula,
        config,
        all_installed_dependency_paths,
    )
    .await
    .map_err(|e| {
        error!(
            "Failed to build new version of formula {} from source: {}",
            formula.name(),
            e
        );
        SpsError::InstallError(format!(
            "Failed to build new version from source during upgrade of {}: {e}",
            formula.name()
        ))
    })?;
    debug!(
        "Successfully built and installed new version of {} to {}",
        formula.name(),
        installed_keg_path.display()
    );

    // 3. Linking is handled by the worker after this function returns the path.

    Ok(installed_keg_path)
}
