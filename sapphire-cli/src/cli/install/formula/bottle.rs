use std::path::PathBuf;
use std::sync::Arc;

use colored::Colorize;
use reqwest::Client;
use sapphire_core::build;
use sapphire_core::build::get_formula_opt_path;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::{Result, SapphireError};
use tokio::task::JoinError;
use tracing::{info, warn};

use super::FormulaInstallInfo; // Use info from parent module

fn join_to_err(e: JoinError) -> SapphireError {
    SapphireError::Generic(format!("Task join error: {e}"))
}

pub async fn run_bottle_install(
    info: FormulaInstallInfo,
    cfg: Config,
    client: Arc<Client>,
) -> Result<PathBuf> {
    let formula = &info.formula;
    let name = formula.name();
    let final_opt_path = get_formula_opt_path(formula, &cfg);

    info!("Processing bottle for: {}...", name.cyan());

    // Check if already installed (defensive check)
    let keg_registry = sapphire_core::keg::KegRegistry::new(cfg.clone());
    if let Some(keg) = keg_registry.get_installed_keg(name)? {
        if keg.version == *formula.version() && keg.revision == formula.revision {
            info!(
                "Formula {} v{} is already installed.",
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
    info!("Downloading bottle for {}...", name);
    let bottle_path =
        build::formula::bottle::download_bottle(formula, &cfg, client.as_ref()).await?;

    // Install (blocking)
    info!("Pouring bottle for {}...", name);
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

    // Link
    info!("Linking artifacts for {}...", name);
    build::formula::link::link_formula_artifacts(formula, &install_dir, &cfg)?;

    info!("Poured and linked {}", name.green());
    Ok(final_opt_path)
}
