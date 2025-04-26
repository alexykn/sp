use std::path::PathBuf;
use std::sync::Arc;

use colored::Colorize;
use sp_core::build;
use sp_core::fetch::api;
use sp_core::model::cask::Cask;
use sp_core::utils::cache::Cache;
use sp_core::utils::config::Config;
use sp_core::utils::error::{Result, SpError};
use tokio::task::JoinError;
use tracing::{debug, error, info};

fn join_to_err(e: JoinError) -> SpError {
    SpError::Generic(format!("Task join error: {e}"))
}

pub async fn run_cask_install(token: &str, cache: Arc<Cache>, cfg: &Config) -> Result<()> {
    let cask: Cask = match api::get_cask(token).await {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to fetch cask info for {}: {}", token, e);
            return Err(e);
        }
    };

    if cask.is_installed(cfg) {
        let installed_version = cask.installed_version(cfg);
        let requested_version = cask.version.as_deref();

        let should_skip = match (installed_version.as_deref(), requested_version) {
            (Some(inst_v), Some(req_v)) => inst_v == req_v,
            (Some(_), None) => true,
            (None, _) => false,
        };

        if should_skip {
            debug!(
                "Cask {} v{} already installed.",
                token,
                installed_version.unwrap_or_default()
            );
            return Err(SpError::InstallError(format!(
                "Cask '{token}' already installed"
            )));
        } else if installed_version.is_some() {
            info!(
                "Cask {} is installed ({}), but requested version is different ({}). Reinstalling",
                token,
                installed_version.unwrap_or_default(),
                requested_version.unwrap_or("latest")
            );
        }
    }

    info!("Downloading {}", token.cyan());
    let dl_path: PathBuf = match build::cask::download_cask(&cask, cache.as_ref()).await {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to download artifact for {}: {}", token, e);
            return Err(e);
        }
    };

    info!("Processing {}", token.cyan());
    tokio::task::spawn_blocking({
        let cask_clone = cask.clone();
        let dl_clone = dl_path.clone();
        let cfg_clone = cfg.clone();
        move || -> Result<()> { build::cask::install_cask(&cask_clone, &dl_clone, &cfg_clone) }
    })
    .await
    .map_err(join_to_err)??;

    Ok(())
}
