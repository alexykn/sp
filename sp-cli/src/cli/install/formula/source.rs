use std::path::PathBuf;

use colored::Colorize;
use sp_core::build;
use sp_core::build::get_formula_opt_path;
use sp_core::utils::config::Config;
use sp_core::utils::error::Result;
use tracing::{info, warn};

use super::FormulaInstallInfo;

pub async fn run_source_install(info: FormulaInstallInfo, cfg: Config) -> Result<PathBuf> {
    let formula = &info.formula;
    let name = formula.name();
    let resolved_graph = info.resolved_graph;
    let final_opt_path = get_formula_opt_path(formula, &cfg);

    info!("Processing source build for: {}", name.cyan());

    // Check if already installed (defensive check)
    let keg_registry = sp_core::keg::KegRegistry::new(cfg.clone());
    if let Some(keg) = keg_registry.get_installed_keg(name)? {
        if keg.version == *formula.version() && keg.revision == formula.revision {
            info!(
                "Formula {} v{} is already installed (source build requested but present).",
                name,
                formula.version_str_full()
            );
            return Ok(keg_registry.get_opt_path(name));
        } else {
            warn!(
                "Installed version ({:?}) of {} differs from requested ({}). Reinstalling from source.",
                keg.version,
                name,
                formula.version_str_full()
            );
        }
    }

    info!("Downloading source for {}", name);
    let source_path = build::formula::source::download_source(formula, &cfg).await?;

    info!("Compiling {}", name);
    let build_dep_paths = resolved_graph.build_dependency_opt_paths.clone();
    let runtime_dep_paths = resolved_graph.runtime_dependency_opt_paths.clone();
    let all_dep_paths = [build_dep_paths, runtime_dep_paths].concat();

    let install_dir: PathBuf = build::formula::source::build_from_source(
        &source_path, // Pass references directly
        formula,
        &cfg,
        &all_dep_paths,
    )
    .await?; // Await the result directly

    info!("Linking artifacts for {}", name);
    build::formula::link::link_formula_artifacts(formula, &install_dir, &cfg)?;

    Ok(final_opt_path)
}
