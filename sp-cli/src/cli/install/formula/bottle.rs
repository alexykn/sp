use std::path::PathBuf;
use std::sync::Arc;

use colored::Colorize;
use reqwest::Client;
use sp_core::build;
use sp_core::build::get_formula_opt_path;
use sp_core::utils::config::Config;
use sp_core::utils::error::{Result, SpError};
use tokio::task::JoinError;
use tracing::{info, warn};

use super::FormulaInstallInfo; // Use info from parent module

fn join_to_err(e: JoinError) -> SpError {
    SpError::Generic(format!("Task join error: {e}"))
}

pub async fn run_bottle_install(
    info: FormulaInstallInfo,
    cfg: Config,
    client: Arc<Client>,
) -> Result<PathBuf> {
    let formula = &info.formula;
    let name = formula.name();
    let final_opt_path = get_formula_opt_path(formula, &cfg);

    // Check if already installed (defensive check)
    let keg_registry = sp_core::keg::KegRegistry::new(cfg.clone());
    if let Some(keg) = keg_registry.get_installed_keg(name)? {
        if keg.version == *formula.version() && keg.revision == formula.revision {
            info!(
                "{} v{} is already installed.",
                name,
                formula.version_str_full()
            );
            return Ok(keg_registry.get_opt_path(name));
        } else {
            warn!(
                "Installed version ({:?}) of {} differs from requested ({}). Reinstalling.",
                keg.version,
                name,
                formula.version_str_full()
            );
        }
    }

    // Download
    info!("Downloading {}", name.cyan());
    let bottle_path =
        build::formula::bottle::download_bottle(formula, &cfg, client.as_ref()).await?;

    // Install (blocking)
    info!("Processing {}", name.cyan());
    let install_dir: PathBuf = tokio::task::spawn_blocking({
        let formula_clone = formula.clone();
        let cfg_clone = cfg.clone();
        let bottle_clone = bottle_path.clone();
        move || -> Result<PathBuf> {
            build::formula::bottle::install_bottle(&bottle_clone, &formula_clone, &cfg_clone)
        }
    })
    .await
    .map_err(join_to_err)??;

    build::formula::link::link_formula_artifacts(formula, &install_dir, &cfg)?;

    Ok(final_opt_path)
}
