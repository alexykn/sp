use std::path::PathBuf;
use std::sync::Arc;

use colored::Colorize;
use sapphire_core::build;
use sapphire_core::fetch::api;
use sapphire_core::model::cask::Cask;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::{Result, SapphireError};
use tokio::task::JoinError;
use tracing::{debug, error, info};

fn join_to_err(e: JoinError) -> SapphireError {
    SapphireError::Generic(format!("Task join error: {e}"))
}

pub async fn run_cask_install(token: &str, cache: Arc<Cache>, cfg: &Config) -> Result<()> {
    info!("Processing cask: {}", token.cyan());
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
            return Err(SapphireError::InstallError(format!(
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

    info!("Downloading artifact for {}", token);
    let dl_path: PathBuf = match build::cask::download_cask(&cask, cache.as_ref()).await {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to download artifact for {}: {}", token, e);
            return Err(e);
        }
    };

    debug!("Installing cask {} from {}...", token, dl_path.display());
    tokio::task::spawn_blocking({
        let cask_clone = cask.clone();
        let dl_clone = dl_path.clone();
        let cfg_clone = cfg.clone();
        move || -> Result<()> { build::cask::install_cask(&cask_clone, &dl_clone, &cfg_clone) }
    })
    .await
    .map_err(join_to_err)??;

    debug!("Cask {} installation task completed successfully", token);
    Ok(())
}
