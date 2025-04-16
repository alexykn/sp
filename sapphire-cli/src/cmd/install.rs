// src/cmd/install.rs
// *** Corrected async/await usage ***

use clap::Args;
use futures::future::BoxFuture;
use log::debug;
use reqwest::Client;
use sapphire_core::build;
use sapphire_core::dependency::{
    DependencyResolver, DependencyTag, ResolutionContext, ResolutionStatus, ResolvedDependency,
};
use sapphire_core::fetch::api;
use sapphire_core::keg::KegRegistry;
use sapphire_core::model::cask::Cask;
use sapphire_core::model::formula::Formula;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::{Result, SapphireError};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use indicatif::ProgressBar;
use colored::Colorize;
use std::time::Duration;
use std::sync::Arc;

#[derive(Debug, Args)]
pub struct InstallArgs {
    #[arg(required = true)]
    names: Vec<String>,
    #[arg(long)]
    skip_deps: bool,
    #[arg(long)]
    cask: bool,
    #[arg(long)]
    include_optional: bool,
    #[arg(long)]
    skip_recommended: bool,
}

pub async fn execute(args: &InstallArgs, config: &Config) -> Result<()> {
    let cache = Cache::new(&config.cache_dir)
        .map_err(|e| SapphireError::Generic(format!("Failed to initialize cache: {}", e)))?;

    let formulary = sapphire_core::formulary::Formulary::new(config.clone());
    let keg_registry = KegRegistry::new(config.clone());

    let context = ResolutionContext {
        formulary: &formulary,
        keg_registry: &keg_registry,
        sapphire_prefix: &config.prefix,
        include_optional: args.include_optional,
        include_test: false,
        skip_recommended: args.skip_recommended,
        force_build: false,
    };

    let mut resolver = DependencyResolver::new(context);

    if args.cask {
        for name in &args.names {
            // Cask (app) installation with dnf5-inspired design
            println!(
                "{}",
                format!("\nInstalling cask: {}", name).blue().bold()
            );
            debug!("Installing cask: {}", name);
            install_cask(name, &cache, false).await?;
            println!(
                "{}",
                format!("✔ Installed cask: {}", name).green().bold()
            );
        }
    } else {
        let resolved_graph = resolver.resolve_targets(&args.names)?;

        if args.skip_deps {
            debug!("Skipping dependency installation due to --skip-deps flag.");
            for target_name in &args.names {
                let resolved_dep = match resolved_graph
                    .install_plan
                    .iter()
                    .find(|d| d.formula.name() == target_name)
                {
                    Some(dep) => dep,
                    None => {
                    log::debug!(
                            "Target '{}' not found in final install plan despite resolution.",
                            target_name
                        );
                        continue;
                    }
                };
                if resolved_dep.status != ResolutionStatus::Installed {
                    let pb = ProgressBar::new_spinner();
                    pb.set_message(format!("Installing {} (deps skipped)", target_name));
                    pb.enable_steady_tick(Duration::from_millis(100));
                    let installed_paths_for_env =
                        get_all_currently_installed_opt_paths(&resolver, &keg_registry)?;
                    install_formula_internal(
                        resolved_dep.formula.clone(),
                        resolved_dep,
                        config,
                        &installed_paths_for_env,
                        /* build_from_source disabled */ false,
                    )
                    .await?;
                    pb.finish_with_message(format!("Installed {}", target_name));
                } else {
                    debug!("Target {} already installed.", target_name);
                }
            }
        } else {
            // Multi-pass Installation Logic
            // Spinner for processing installation plan

            let mut install_status: HashMap<String, ResolutionStatus> = resolved_graph
                .install_plan
                .iter()
                .map(|dep| (dep.formula.name().to_string(), dep.status.clone()))
                .collect();
            let mut installed_opt_paths: HashMap<String, PathBuf> = resolved_graph
                .install_plan
                .iter()
                .filter_map(|dep| {
                    if dep.status == ResolutionStatus::Installed {
                        dep.opt_path.as_ref().and_then(|p| {
                            if p.exists() {
                                Some((dep.formula.name().to_string(), p.clone()))
                            } else {
                                log::debug!(
                                    "Opt path {} for installed dependency {} does not exist.",
                                    p.display(),
                                    dep.formula.name()
                                );
                                None
                            }
                        })
                    } else {
                        None
                    }
                })
                .collect();
            add_globally_installed_paths(
                &resolver,
                &keg_registry,
                &mut installed_opt_paths,
                &mut install_status,
            )?;
            let total_to_install_initially = install_status
                .iter()
                .filter(|(_, status)| {
                    **status == ResolutionStatus::Missing || **status == ResolutionStatus::Requested
                })
                .count();
            let mut remaining_to_install = total_to_install_initially;
            let mut pass_count = 0;
            let max_passes = resolved_graph.install_plan.len() + 2;

            while remaining_to_install > 0 && pass_count < max_passes {
                pass_count += 1;
                let mut progress_made_this_pass = false;
                let mut newly_installed_in_pass = 0;
                let keys_to_process: Vec<String> = resolved_graph
                    .install_plan
                    .iter()
                    .filter_map(|resolved_dep| {
                        let name = resolved_dep.formula.name();
                        if install_status.get(name).map_or(false, |s| {
                            *s == ResolutionStatus::Missing || *s == ResolutionStatus::Requested
                        }) {
                            Some(name.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                debug!(
                    "Installation pass {} ({} remaining)",
                    pass_count,
                    keys_to_process.len()
                );
                if keys_to_process.is_empty() {
                    if remaining_to_install > 0 {
                        log::error!("Installation loop inconsistency: {} items reported remaining, but none found needing installation in plan.", remaining_to_install);
                    }
                    break;
                }

                for name in keys_to_process {
                    let current_status = install_status
                        .get(&name)
                        .cloned()
                        .unwrap_or(ResolutionStatus::Missing);
                    if current_status != ResolutionStatus::Missing
                        && current_status != ResolutionStatus::Requested
                    {
                        continue;
                    }
                    let resolved_dep = resolved_graph
                        .install_plan
                        .iter()
                        .find(|d| d.formula.name() == name)
                        .expect("Item in keys_to_process must be in install_plan");
                    let direct_deps = match resolved_dep.formula.dependencies() {
                        Ok(deps) => deps,
                        Err(e) => {
                            log::error!(
                                "Error getting dependencies for {}: {}. Skipping formula.",
                                name,
                                e
                            );
                            continue;
                        }
                    };
                    let mut all_deps_met = true;
                    for dep in &direct_deps {
                        if !should_consider_dependency(dep, args) {
                            continue;
                        }
                        let dep_status = install_status.get(&dep.name).cloned();
                        let is_met = match dep_status {
                            Some(ResolutionStatus::Installed) => check_and_update_opt_path(
                                &dep.name,
                                &keg_registry,
                                &mut installed_opt_paths,
                            ),
                            Some(ResolutionStatus::SkippedOptional) => true,
                            _ => false,
                        };
                        if !is_met {
                            let keg_exists = keg_registry.get_installed_keg(&dep.name)?.is_some();
                            if keg_exists
                                && install_status
                                    .get(&dep.name)
                                    .map_or(false, |s| *s == ResolutionStatus::Installed)
                            {
                            log::debug!("Keg found for {} but opt path does not exist (needs linking). Considering dependency met for install check.", dep.name);
                            } else {
                                log::debug!("Dependency '{}' for '{}' not met (status: {:?}, opt_path_exists: {}).", dep.name, name, dep_status, installed_opt_paths.contains_key(&dep.name));
                                all_deps_met = false;
                                break;
                            }
                        }
                    }

                    if all_deps_met {
                        log::debug!("All dependencies met for: {}", name);
                        let all_current_installed_paths: Vec<PathBuf> =
                            installed_opt_paths.values().cloned().collect();
                        match install_formula_internal(
                            resolved_dep.formula.clone(),
                            resolved_dep,
                            config,
                            &all_current_installed_paths,
                            /* build_from_source disabled */ false,
                        )
                        .await
                        {
                            // Await here
                            Ok(installed_opt_path) => {
                                log::debug!("Successfully installed {} internally.", name);
                                install_status.insert(name.clone(), ResolutionStatus::Installed);
                                if installed_opt_path.exists() {
                                    installed_opt_paths.insert(name.clone(), installed_opt_path);
                                } else {
                                    log::error!( "Install succeeded for {}, but opt path {} was not found immediately after. Check linking.", name, installed_opt_path.display() );
                                }
                                progress_made_this_pass = true;
                                newly_installed_in_pass += 1;
                            }
                            Err(e) => {
                                log::error!("Error installing {}: {}", name, e);
                                return Err(e);
                            }
                        }
                    } else {
                        log::debug!(
                            "Deferring installation of '{}' until dependencies are ready.",
                            name
                        );
                    }
                }
                remaining_to_install -= newly_installed_in_pass;
                if !progress_made_this_pass && remaining_to_install > 0 {
                    let remaining_names: Vec<_> = install_status
                        .iter()
                        .filter(|(_, s)| {
                            **s == ResolutionStatus::Missing || **s == ResolutionStatus::Requested
                        })
                        .map(|(n, _)| n.clone())
                        .collect();
                    log::error!(
                        "No progress made in installation pass {}. Could not install: {:?}",
                        pass_count,
                        remaining_names
                    );
                    for r_name in &remaining_names {
                        if let Some(r_dep) = resolved_graph
                            .install_plan
                            .iter()
                            .find(|d| d.formula.name() == r_name)
                        {
                            if let Ok(deps) = r_dep.formula.dependencies() {
                                log::error!(
                                    "  Dependencies for {}: {:?}",
                                    r_name,
                                    deps.iter()
                                        .map(|d| (&d.name, install_status.get(&d.name)))
                                        .collect::<Vec<_>>()
                                );
                            }
                        }
                    }
                    return Err(SapphireError::DependencyError(format!(
                        "Unresolved dependencies or cycle after pass {}. Remaining: {:?}",
                        pass_count, remaining_names
                    )));
                }
            }
            if remaining_to_install > 0 {
                let remaining_names: Vec<_> = install_status
                    .iter()
                    .filter(|(_, s)| {
                        **s == ResolutionStatus::Missing || **s == ResolutionStatus::Requested
                    })
                    .map(|(n, _)| n.clone())
                    .collect();
                log::error!("Installation incomplete (max passes {} reached or other issue). Remaining: {:?}", max_passes, remaining_names);
                return Err(SapphireError::DependencyError(format!(
                    "Installation incomplete. Remaining: {:?}",
                    remaining_names
                )));
            }
        }
    }
    Ok(())
}

// Helper functions (should_consider_dependency, check_and_update_opt_path, get_all_currently_installed_opt_paths, add_globally_installed_paths) remain unchanged
fn should_consider_dependency(
    dep: &sapphire_core::dependency::Dependency,
    args: &InstallArgs,
) -> bool {
    let tags = dep.tags;
    if tags.contains(DependencyTag::TEST) {
        return false;
    }
    if tags.contains(DependencyTag::OPTIONAL) && !args.include_optional {
        return false;
    }
    if tags.contains(DependencyTag::RECOMMENDED) && args.skip_recommended {
        return false;
    }
    true
}
fn check_and_update_opt_path(
    name: &str,
    keg_registry: &KegRegistry,
    installed_opt_paths: &mut HashMap<String, PathBuf>,
) -> bool {
    if installed_opt_paths.get(name).map_or(false, |p| p.exists()) {
        return true;
    }
    let opt_path = keg_registry.get_opt_path(name);
    if opt_path.exists() {
        log::debug!(
            "Found existing opt path for {}: {}",
            name,
            opt_path.display()
        );
        installed_opt_paths.insert(name.to_string(), opt_path);
        return true;
    }
    log::debug!("Dependency {} not found via opt path.", name);
    false
}
fn get_all_currently_installed_opt_paths(
    resolver: &DependencyResolver,
    keg_registry: &KegRegistry,
) -> Result<Vec<PathBuf>> {
    let mut paths = HashSet::new();
    for (_, resolved_dep) in resolver.resolved.iter() {
        if resolved_dep.status == ResolutionStatus::Installed {
            if let Some(opt_path) = &resolved_dep.opt_path {
                if opt_path.exists() {
                    paths.insert(opt_path.clone());
                } else {
                    log::warn!(
                        "Resolved dependency {} marked installed, but opt path {} does not exist.",
                        resolved_dep.formula.name(),
                        opt_path.display()
                    );
                }
            }
        }
    }
    if let Ok(installed_kegs) = keg_registry.list_installed_kegs() {
        for keg in installed_kegs {
            let opt_path = keg_registry.get_opt_path(&keg.name);
            if opt_path.exists() {
                paths.insert(opt_path);
            }
        }
    }
    Ok(paths.into_iter().collect())
}
fn add_globally_installed_paths(
    _resolver: &DependencyResolver,
    keg_registry: &KegRegistry,
    installed_opt_paths: &mut HashMap<String, PathBuf>,
    install_status: &mut HashMap<String, ResolutionStatus>,
) -> Result<()> {
    if let Ok(installed_kegs) = keg_registry.list_installed_kegs() {
        for keg in installed_kegs {
            let name = keg.name.clone();
            let opt_path = keg_registry.get_opt_path(&name);
            if opt_path.exists() {
                installed_opt_paths.entry(name.clone()).or_insert(opt_path);
                install_status
                    .entry(name)
                    .or_insert(ResolutionStatus::Installed);
            } else if keg.path.exists() {
                install_status
                    .entry(name)
                    .or_insert(ResolutionStatus::Installed);
            }
        }
    }
    Ok(())
}

/// Internal function to handle the actual installation of a single formula.
async fn install_formula_internal(
    formula: Arc<Formula>,
    resolved_info: &ResolvedDependency,
    config: &Config,
    _all_installed_paths: &[PathBuf],
    _force_build: bool,
) -> Result<PathBuf> {
    let name = formula.name();
    println!(
        "{}",
        format!("\nStarting installation process for: {}", name).blue().bold()
    );

    let client = Client::new(); // Create client for potential bottle download

    let status = &resolved_info.status;
    let keg_path = resolved_info.keg_path.as_deref();
    let opt_path_expected: PathBuf =
        resolved_info.opt_path.as_ref().cloned().unwrap_or_else(|| {
            log::warn!(
                "Opt path missing in resolved info for {}, recalculating.",
                name
            );
            build::get_formula_opt_path(&formula)
        });

    if *status == ResolutionStatus::Installed {
        let keg_path_actual = keg_path.ok_or_else(|| {
            SapphireError::Generic(format!(
                "Installed formula {} missing keg path in resolved info",
                name
            ))
        })?;
        if !keg_path_actual.exists() {
            log::warn!("Formula {} marked installed, but keg path {} does not exist. Attempting re-install.", name, keg_path_actual.display());
            // Fall through to install logic
        } else {
            println!(
                "Formula {} is already installed (Version: {} at {}).",
                name,
                resolved_info.formula.version_str_full(),
                keg_path_actual.display()
            );
            if !opt_path_expected.exists() {
                println!(
                    "{}",
                    format!("Opt link missing, linking installed formula: {}", name).blue().bold()
                );
                build::formula::link::link_formula_artifacts(&formula, keg_path_actual)?;
                if !opt_path_expected.exists() {
                    return Err(SapphireError::InstallError(format!(
                        "Failed to create opt link {} after installation.",
                        opt_path_expected.display()
                    )));
                }
            }
            return Ok(opt_path_expected.to_path_buf());
        }
    } else if *status == ResolutionStatus::SkippedOptional {
        log::debug!("Attempted to internally install skipped formula {}", name);
        return Err(SapphireError::Generic(format!(
            "Attempted to install skipped formula {}",
            name
        )));
    }

    println!(
        "{}",
        format!("Installing formula: {}", name).blue().bold()
    );

    let install_dir = build::formula::get_formula_cellar_path(&formula);
    let opt_path = build::get_formula_opt_path(&formula);
    // Only bottle installation is supported; ensure a bottle is available
    let use_bottle = build::formula::has_bottle_for_current_platform(&formula);
    if !use_bottle {
        return Err(SapphireError::InstallError(format!(
            "No bottle available for {} on this platform",
            name
        )));
    }

    // Download the bottle
    let bottle_path = match build::formula::bottle::download_bottle(&formula, config, &client).await {
        Ok(path) => path,
        Err(e) => {
            log::error!("Download failed for {}: {}", name, e);
            return Err(SapphireError::InstallError(format!(
                "Download failed for {}: {}",
                name, e
            )));
        }
    };

    // Prepare installation directory
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SapphireError::Io(e))?;
    }

    // Pour the bottle
    println!(
        "{}",
        format!("Pouring bottle: {}", bottle_path.display()).blue().bold()
    );
    build::formula::bottle::install_bottle(&bottle_path, &formula, config)?;

    println!(
        "{}",
        format!("Linking formula: {}", name).blue().bold()
    );
    if install_dir.exists() {
        build::formula::link::link_formula_artifacts(&formula, &install_dir)?; // Synchronous link
    } else {
        log::error!(
            "Standard install directory {} does not exist after installation attempt. Cannot link.",
            install_dir.display()
        );
        return Err(SapphireError::InstallError(format!(
            "Installation directory {} not found after build/install of {}",
            install_dir.display(),
            name
        )));
    }

    if !opt_path.exists() {
        log::error!(
            "Linking step for {} completed, but opt link {} was not found.",
            name,
            opt_path.display()
        );
        return Err(SapphireError::InstallError(format!(
            "Linking failed for {}: Opt link {} not found after installation.",
            name,
            opt_path.display()
        )));
    } else {
        println!(
            "{}",
            format!("Successfully installed {}", name).blue().bold()
        );
        return Ok(opt_path);
    }

}

// Cask Installation (Remains the same)
fn install_cask<'a>(
    name: &'a str,
    cache: &'a Cache,
    force_build: bool,
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        println!(
            "{}",
            format!("Installing cask: {}", name).blue().bold()
        );
        let cask: Cask = match api::get_cask(name).await {
            Ok(c) => c,
            Err(e) => {
                log::error!("Failed to fetch cask metadata for '{}': {}", name, e);
                return Err(SapphireError::NotFound(format!(
                    "Cask '{}' not found or API error: {}",
                    name, e
                )));
            }
        };
        if cask.is_installed() {
            if let Some(installed_version) = cask.installed_version() {
                let current_version = cask.version.clone().unwrap_or_else(|| "latest".to_string());
                if installed_version == current_version && !force_build {
                    println!(
                        "{}",
                        format!("Cask '{}' version {} is already installed.", name, installed_version)
                            .yellow()
                            .bold()
                    );
                    return Ok(());
                } else if installed_version != current_version {
                    println!(
                        "{}",
                        format!("Upgrading cask '{}' from {} to {}", name, installed_version, current_version)
                            .blue()
                            .bold()
                    );
                } else {
                    println!(
                        "{}",
                        format!("Reinstalling cask '{}' (version {}) due to force flag", name, installed_version)
                            .blue()
                            .bold()
                    );
                }
            } else {
                println!(
                    "{}",
                    format!("Cask '{}' is installed, but version unknown. Proceeding with installation.", name)
                        .blue()
                        .bold()
                );
            }
        }
        install_cask_dependencies(&cask, cache, force_build).await?;
        let download_path = match build::cask::download_cask(&cask, cache).await {
            Ok(path) => path,
            Err(e) => {
                log::error!("Failed to download cask '{}': {}", name, e);
                return Err(SapphireError::InstallError(format!(
                    "Download failed for cask '{}': {}",
                    name, e
                )));
            }
        };
        if let Err(e) = build::cask::install_cask(&cask, &download_path) {
            log::error!("Failed to install cask '{}': {}", name, e);
            return Err(SapphireError::InstallError(format!(
                "Installation failed for cask '{}': {}",
                name, e
            )));
        }
        println!("✅ Successfully installed cask: {}", cask.display_name());
        Ok(())
    })
}
fn install_cask_dependencies<'a>(
    cask: &'a Cask,
    cache: &'a Cache,
    _force_build: bool,
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        if let Some(deps) = &cask.depends_on {
            let config_result = Config::load();
            if let Some(formula_deps) = &deps.formula {
                if !formula_deps.is_empty() {
                    println!(
                        "{}",
                        format!("Installing formula dependencies for cask {}: {:?}", cask.token, formula_deps)
                            .blue()
                            .bold()
                    );
                    let config = match config_result.as_ref() {
                        Ok(c) => c,
                        Err(e) => {
                            return Err(SapphireError::Config(format!(
                                "Failed to load config for dependency install: {}",
                                e
                            )))
                        }
                    };
                    let args_for_deps = InstallArgs {
                        names: formula_deps.clone(),
                        skip_deps: false,
                        cask: false,
                        include_optional: false,
                        skip_recommended: false,
                    };
                    if let Err(e) = execute(&args_for_deps, config).await {
                        log::error!(
                            "Failed to install formula dependency for cask {}: {}",
                            cask.token,
                            e
                        );
                        return Err(e);
                    }
                }
            }
            if let Some(cask_deps) = &deps.cask {
                if !cask_deps.is_empty() {
                    println!(
                        "{}",
                        format!("Installing cask dependencies for cask {}: {:?}", cask.token, cask_deps)
                            .blue()
                            .bold()
                    );
                    for dep_name in cask_deps {
                        if let Err(e) = install_cask(dep_name, cache, false).await {
                            log::error!(
                                "Failed to install cask dependency '{}' for cask {}: {}",
                                dep_name,
                                cask.token,
                                e
                            );
                            return Err(e);
                        }
                    }
                }
            }
        }
        Ok(())
    })
}
