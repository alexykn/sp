use sapphire_core::utils::error::{SapphireError, Result};
use sapphire_core::model::formula::Formula;
use sapphire_core::utils::config::Config;
use sapphire_core::build;
use futures::future::BoxFuture;
use sapphire_core::fetch::api;
use sapphire_core::utils::cache::Cache;
use clap::Args;
use sapphire_core::model::cask::Cask;
use sapphire_core::dependency::{DependencyResolver, ResolutionContext, ResolvedDependency, DependencyTag, ResolutionStatus}; // Import resolver components
use sapphire_core::keg::KegRegistry; // Import KegRegistry
use std::sync::Arc; // Use Arc for formula sharing across threads
use std::collections::{HashMap, HashSet}; // Added for state tracking
use std::path::PathBuf; // Added for path handling
use log; // Import log crate

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Names of formulas or casks to install
    #[arg(required = true)]
    names: Vec<String>,

    /// Skip installing dependencies
    #[arg(long)]
    skip_deps: bool,

    /// Install as casks (applications) instead of formulas
    #[arg(long)]
    cask: bool,

    /// Force building from source, even if bottles are available
    #[arg(long)]
    build_from_source: bool,

    /// Include optional dependencies
    #[arg(long)]
    include_optional: bool,

    /// Skip recommended dependencies
    #[arg(long)]
    skip_recommended: bool,
}

pub async fn execute(args: &InstallArgs, config: &Config) -> Result<()> {
    let cache = Cache::new(&config.cache_dir).map_err(|e| {
        SapphireError::Generic(format!("Failed to initialize cache: {}", e))
    })?;

    let formulary = sapphire_core::formulary::Formulary::new(config.clone());
    let keg_registry = KegRegistry::new(config.clone());

    let context = ResolutionContext {
        formulary: &formulary,
        keg_registry: &keg_registry,
        sapphire_prefix: &config.prefix,
        include_optional: args.include_optional,
        include_test: false, // Tests usually aren't installed by default
        skip_recommended: args.skip_recommended,
        force_build: args.build_from_source,
    };

    let mut resolver = DependencyResolver::new(context);

    if args.cask {
        for name in &args.names {
            install_cask(name, &cache, args.build_from_source).await?;
        }
    } else {
        let resolved_graph = resolver.resolve_targets(&args.names)?;

        if args.skip_deps {
            println!("Skipping dependency installation due to --skip-deps flag.");
            for target_name in &args.names {
                 let resolved_dep = match resolved_graph.install_plan.iter().find(|d| d.formula.name() == target_name) {
                     Some(dep) => dep,
                     // If the target itself wasn't resolved (e.g., doesn't exist), resolver would have errored.
                     // If it was resolved but skipped (e.g., optional), resolver handles it.
                     // If it's just not in the final plan for some reason, skip here.
                     None => {
                         log::warn!("Target '{}' not found in final install plan despite resolution.", target_name);
                         continue;
                      }
                 };

                // Check status from the resolved plan
                if resolved_dep.status != ResolutionStatus::Installed {
                    println!("==> Installing target (deps skipped): {}", target_name);
                    // Get paths of *already* installed kegs for the build env
                    let installed_paths_for_env = get_all_currently_installed_opt_paths(&resolver, &keg_registry)?;
                    // Call internal install function
                    install_formula_internal(
                        resolved_dep.formula.clone(),
                        resolved_dep, // Pass the resolved dependency info
                        config,
                        &installed_paths_for_env, // Pass existing paths
                        args.build_from_source,
                    ).await?; // Removed fallback triggering logic here
                } else {
                    println!("Target {} already installed.", target_name);
                }
            }
        } else {
            // --- Multi-pass Installation Logic ---
            println!("==> Processing installation plan...");
            let mut install_status: HashMap<String, ResolutionStatus> = resolved_graph.install_plan
                .iter()
                .map(|dep| (dep.formula.name().to_string(), dep.status.clone()))
                .collect();

            let mut installed_opt_paths: HashMap<String, PathBuf> = resolved_graph.install_plan
                .iter()
                .filter_map(|dep| {
                    // Ensure opt path exists before adding
                    if dep.status == ResolutionStatus::Installed {
                        dep.opt_path.as_ref().and_then(|p| {
                            if p.exists() {
                                Some((dep.formula.name().to_string(), p.clone()))
                            } else {
                                // Log if opt path for installed dep doesn't exist
                                log::warn!("Opt path {} for installed dependency {} does not exist.", p.display(), dep.formula.name());
                                None
                            }
                        })
                    } else {
                        None
                    }
                })
                .collect();

             // Add paths/status for things installed outside this run
             add_globally_installed_paths(&resolver, &keg_registry, &mut installed_opt_paths, &mut install_status)?;

            let total_to_install_initially = install_status
                .iter()
                .filter(|(_, status)| **status == ResolutionStatus::Missing || **status == ResolutionStatus::Requested)
                .count();

            let mut remaining_to_install = total_to_install_initially;
            let mut pass_count = 0;
            // Increase max_passes slightly, simple count might be too tight for complex graphs
            let max_passes = resolved_graph.install_plan.len() + 2;

            while remaining_to_install > 0 && pass_count < max_passes {
                pass_count += 1;
                let mut progress_made_this_pass = false;
                let mut newly_installed_in_pass = 0;

                // Get list of items needing installation in this pass
                let keys_to_process: Vec<String> = resolved_graph.install_plan // Iterate over original plan order
                    .iter()
                    .filter_map(|resolved_dep| {
                        let name = resolved_dep.formula.name();
                        // Only consider items that are currently marked as Missing or Requested
                        if install_status.get(name).map_or(false, |s| *s == ResolutionStatus::Missing || *s == ResolutionStatus::Requested) {
                            Some(name.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();


                println!("--- Installation Pass {} ({} remaining) ---", pass_count, keys_to_process.len());

                // If nothing left to process in the plan that needs installing, break
                if keys_to_process.is_empty() {
                     if remaining_to_install > 0 {
                        // This state indicates inconsistency
                        log::error!("Installation loop inconsistency: {} items reported remaining, but none found needing installation in plan.", remaining_to_install);
                     }
                     break;
                 }

                for name in keys_to_process {
                    // Double-check status in case it was installed in this pass already
                    let current_status = install_status.get(&name).cloned().unwrap_or(ResolutionStatus::Missing);
                    if current_status != ResolutionStatus::Missing && current_status != ResolutionStatus::Requested { continue; }

                    // Find the corresponding ResolvedDependency in the original plan
                    let resolved_dep = resolved_graph.install_plan.iter()
                                         .find(|d| d.formula.name() == name)
                                         .expect("Item in keys_to_process must be in install_plan"); // Should always find

                    // Get dependencies defined within the formula itself
                    let direct_deps = match resolved_dep.formula.dependencies() {
                        Ok(deps) => deps,
                        Err(e) => {
                             log::error!("Error getting dependencies for {}: {}. Skipping formula.", name, e);
                             // Consider how to handle this - maybe mark as failed? For now, skip.
                             continue;
                        }
                    };

                    // Check if all *required* dependencies are met (Installed)
                    let mut all_deps_met = true;
                    for dep in &direct_deps {
                        // Skip optional/test/skipped-recommended dependencies based on flags
                        if !should_consider_dependency(dep, args) { continue; }

                        // Check status from our live tracking map
                        let dep_status = install_status.get(&dep.name).cloned();
                        let is_met = match dep_status {
                            Some(ResolutionStatus::Installed) => {
                                // Verify opt path exists for installed deps
                                check_and_update_opt_path(&dep.name, &keg_registry, &mut installed_opt_paths)
                            }
                            Some(ResolutionStatus::SkippedOptional) => true, // Skipped = considered met
                            _ => false, // Missing, Requested, or None means not met yet
                        };

                        if !is_met {
                            log::debug!("Dependency '{}' for '{}' not met (status: {:?}).", dep.name, name, dep_status);
                            all_deps_met = false;
                            break; // No need to check further deps for this formula in this pass
                        }
                    }


                    if all_deps_met {
                        log::info!("All dependencies met for: {}", name);
                        // Get the current set of ALL installed opt paths for the build env
                        let all_current_installed_paths: Vec<PathBuf> = installed_opt_paths.values().cloned().collect();

                        // Attempt to install the formula
                        match install_formula_internal(
                            resolved_dep.formula.clone(),
                            resolved_dep, // Pass resolved info
                            config,
                            &all_current_installed_paths,
                            args.build_from_source,
                        ).await {
                            Ok(installed_opt_path) => {
                                log::info!("Successfully installed {} internally.", name);
                                // Update status and path maps
                                install_status.insert(name.clone(), ResolutionStatus::Installed);
                                // Only insert if opt path is valid and exists
                                if installed_opt_path.exists() {
                                     installed_opt_paths.insert(name.clone(), installed_opt_path);
                                } else {
                                     log::error!("install_formula_internal for {} succeeded but returned non-existent opt path {}", name, installed_opt_path.display());
                                     // Decide how critical this is - maybe error out?
                                     // For now, just warn and don't add to map.
                                }
                                progress_made_this_pass = true;
                                newly_installed_in_pass += 1;
                            }
                            Err(e) => {
                                log::error!("Error installing {}: {}", name, e);
                                // Stop the entire installation process on error
                                return Err(e);
                            }
                        }
                    } else {
                         log::debug!("Deferring installation of '{}' until dependencies are ready.", name);
                    }
                } // End loop through keys_to_process

                remaining_to_install -= newly_installed_in_pass;

                // Check for stall condition
                if !progress_made_this_pass && remaining_to_install > 0 {
                    let remaining_names: Vec<_> = install_status
                        .iter()
                        .filter(|(_, s)| **s == ResolutionStatus::Missing || **s == ResolutionStatus::Requested)
                        .map(|(n, _)| n.clone())
                        .collect();
                    log::error!("No progress made in installation pass {}. Could not install: {:?}", pass_count, remaining_names);
                    // Also log dependencies of remaining items
                    for r_name in &remaining_names {
                         if let Some(r_dep) = resolved_graph.install_plan.iter().find(|d| d.formula.name() == r_name) {
                             if let Ok(deps) = r_dep.formula.dependencies() {
                                 log::error!("  Dependencies for {}: {:?}", r_name, deps.iter().map(|d| (&d.name, install_status.get(&d.name))).collect::<Vec<_>>());
                             }
                         }
                    }
                    return Err(SapphireError::DependencyError(format!("Unresolved dependencies or cycle after pass {}. Remaining: {:?}", pass_count, remaining_names)));
                }

            } // End while loop

            // Final check after loop
            if remaining_to_install > 0 {
                 let remaining_names: Vec<_> = install_status.iter().filter(|(_, s)| **s == ResolutionStatus::Missing || **s == ResolutionStatus::Requested).map(|(n, _)| n.clone()).collect();
                 log::error!("Installation incomplete (max passes {} reached or other issue). Remaining: {:?}", max_passes, remaining_names);
                 return Err(SapphireError::DependencyError(format!("Installation incomplete. Remaining: {:?}", remaining_names)));
            }

            println!("==> Installation plan processed successfully.");
        }
    }

    Ok(())
}

/// Helper function to decide if a dependency should be processed based on install args
fn should_consider_dependency(dep: &sapphire_core::dependency::Dependency, args: &InstallArgs) -> bool {
    let tags = dep.tags;
    // Ignore test dependencies during installation
    if tags.contains(DependencyTag::TEST) { return false; }
    // Handle optional based on flag
    if tags.contains(DependencyTag::OPTIONAL) && !args.include_optional { return false; }
    // Handle recommended based on flag
    if tags.contains(DependencyTag::RECOMMENDED) && args.skip_recommended { return false; }
    // Otherwise, consider it (RUNTIME, BUILD, or included OPTIONAL/RECOMMENDED)
    true
}

/// Helper to check KegRegistry for an installed package and update the opt_paths map if needed.
fn check_and_update_opt_path(
    name: &str,
    keg_registry: &KegRegistry,
    installed_opt_paths: &mut HashMap<String, PathBuf>
) -> bool {
    // If already tracked with an existing path, it's met
    if installed_opt_paths.get(name).map_or(false, |p| p.exists()) {
        return true;
    }
    // Check standard opt path directly
    let opt_path = keg_registry.get_opt_path(name);
    if opt_path.exists() {
         log::debug!("Found existing opt path for {}: {}", name, opt_path.display());
         installed_opt_paths.insert(name.to_string(), opt_path);
         return true;
    }
    // Check if keg exists even if opt link doesn't (means installed but not linked)
    if keg_registry.get_installed_keg(name).unwrap_or(None).is_some() {
        log::warn!("Keg found for {} but opt path {} does not exist (needs linking). Considering dependency met.", name, opt_path.display());
        // Don't add to map yet, but return true for dependency check.
        return true;
    }
    // --- Fallback Check Removed ---
    log::debug!("Dependency {} not found in opt paths or keg registry.", name);
    false
}

/// Helper to get opt paths for ALL known installed dependencies (from resolver + registry)
 fn get_all_currently_installed_opt_paths(
     resolver: &DependencyResolver, // Use pub(crate) resolver.resolved directly
     keg_registry: &KegRegistry
 ) -> Result<Vec<PathBuf>> {
     let mut paths = HashSet::new(); // Use HashSet to avoid duplicates

     // Add paths from the current resolution context that are marked installed
     for (_, resolved_dep) in resolver.resolved.iter() {
         if resolved_dep.status == ResolutionStatus::Installed {
             if let Some(opt_path) = &resolved_dep.opt_path {
                 if opt_path.exists() {
                     paths.insert(opt_path.clone());
                 }
             }
         }
     }

     // Add paths from anything *else* installed according to KegRegistry
     if let Ok(installed_kegs) = keg_registry.list_installed_kegs() {
         for keg in installed_kegs {
             let opt_path = keg_registry.get_opt_path(&keg.name);
             if opt_path.exists() {
                 // Insert adds if not present, harmless if already added from resolver map
                 paths.insert(opt_path);
             }
         }
     }

     // --- Fallback Path Addition Removed ---

     Ok(paths.into_iter().collect())
 }


 /// Pre-populate installed_opt_paths and install_status with globally installed kegs
 fn add_globally_installed_paths(
     _resolver: &DependencyResolver, // Not strictly needed anymore? Maybe keep for future use.
     keg_registry: &KegRegistry,
     installed_opt_paths: &mut HashMap<String, PathBuf>,
     install_status: &mut HashMap<String, ResolutionStatus>
 ) -> Result<()> {
      if let Ok(installed_kegs) = keg_registry.list_installed_kegs() {
         for keg in installed_kegs {
             let name = keg.name.clone();
             let opt_path = keg_registry.get_opt_path(&name);
             // If the opt path exists, record it and mark as installed
             if opt_path.exists() {
                 installed_opt_paths.entry(name.clone()).or_insert(opt_path);
                 install_status.entry(name).or_insert(ResolutionStatus::Installed);
             } else if keg.path.exists() { // Keg exists but not linked
                 // Still mark as installed in the status map, but don't add to opt paths
                 // This signals the dependency is met but needs linking later.
                 install_status.entry(name).or_insert(ResolutionStatus::Installed);
             }
             // If neither opt nor keg path exists, do nothing (shouldn't happen with list_installed_kegs)
         }
     }
     Ok(())
 }


/// Internal function to handle the actual installation of a single formula.
async fn install_formula_internal(
    formula: Arc<Formula>,
    resolved_info: &ResolvedDependency, // Use resolved info for status checks
    config: &Config,
    all_installed_paths: &[PathBuf],
    force_build: bool,
) -> Result<PathBuf> { // Return the installed Opt path
    let name = formula.name();
    println!("==> Starting installation process for: {}", name);

    // Check status from resolved dependency info passed in
    match resolved_info.status {
        ResolutionStatus::Missing | ResolutionStatus::Requested => {
            println!("==> Installing formula: {}", name);
            // Proceed with download/build logic below
        }
        ResolutionStatus::Installed => {
            let keg_path = resolved_info.keg_path.as_deref().ok_or_else(|| SapphireError::Generic(format!("Installed formula {} missing keg path in resolved info", name)))?;
            let opt_path = resolved_info.opt_path.as_deref().ok_or_else(|| SapphireError::Generic(format!("Installed formula {} missing opt path in resolved info", name)))?;

            if !keg_path.exists() {
                 log::warn!("Formula {} marked installed, but keg path {} does not exist. Attempting re-install.", name, keg_path.display());
                 // Fall through to install logic below
            } else {
                 println!("Formula {} is already installed (Version: {} at {}).", name, resolved_info.formula.version_str_full(), keg_path.display());
                 // Ensure it's linked
                 if !opt_path.exists() {
                    println!("==> Opt link missing, linking installed formula: {}", name);
                    // Use the keg_path obtained from resolved_info
                    build::formula::link::link_formula_artifacts(&formula, keg_path)?;
                 }
                 // Return the opt_path associated with this installed version
                 return Ok(opt_path.to_path_buf());
            }
        }
        ResolutionStatus::SkippedOptional => {
            // This case should ideally be filtered out before calling internal install,
            // but handle defensively.
            log::warn!("Attempted to internally install skipped formula {}", name);
            return Err(SapphireError::Generic(format!("Attempted to install skipped formula {}", name)));
        }
    }


    let install_dir = build::formula::get_formula_cellar_path(&formula); // Standard cellar path
    let opt_path = build::get_formula_opt_path(&formula); // Standard opt path

    // Determine whether to use bottle or source
    let use_bottle = build::formula::has_bottle_for_current_platform(&formula) && !force_build;

    // Attempt download (bottle or source)
    let download_path_result = if use_bottle {
         build::formula::bottle::download_bottle(&formula, config).await
     } else {
         build::formula::source::download_source(&formula, config).await
     };

    // Handle download result - NO FALLBACK
    let source_or_bottle_path = match download_path_result {
        Ok(path) => path,
        Err(e) => {
             log::error!("Download failed for {}: {}", name, e);
             // Directly return the download error
             return Err(SapphireError::InstallError(format!("Download failed for {}: {}", name, e)));
        }
    };

    // Ensure parent directory for install_dir exists
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SapphireError::Io(e))?;
    }

    // Perform installation: bottle or source build
    if use_bottle {
        println!("==> Pouring bottle: {}", source_or_bottle_path.display());
        // install_bottle should return the install_dir on success
        build::formula::bottle::install_bottle(&source_or_bottle_path, &formula)?;
    } else {
        // Build from source
        if force_build {
            println!("==> Building from source (forced): {}", name);
        } else {
            println!("==> Building from source (no bottle available): {}", name);
        }
        // Call build_from_source with the downloaded path (&Path)
        build::formula::source::build_from_source(&source_or_bottle_path, &formula, config, all_installed_paths).await?;
    }

    // --- Fallback Build Logic Removed ---
    // --- Fallback Linking Logic Removed ---

    // Standard linking (after successful bottle pour or source build)
    println!("==> Linking formula: {}", name);
    // Ensure install_dir exists before linking
    if install_dir.exists() {
        build::formula::link::link_formula_artifacts(&formula, &install_dir)?;
    } else {
        // This indicates a problem with the bottle pour or source build if dir doesn't exist
        log::error!("Standard install directory {} does not exist after installation attempt. Cannot link.", install_dir.display());
        return Err(SapphireError::InstallError(format!("Installation directory {} not found after build/install of {}", install_dir.display(), name)));
    }


    println!("🍺 Successfully installed {} ({})", formula.name(), install_dir.display());

    // Return the standard opt path upon successful installation
    Ok(opt_path)
}


// --- Removed link_fallback_artifacts function ---


// --- Cask Installation (Remains the same) ---

fn install_cask<'a>(
    name: &'a str,
    cache: &'a Cache,
    force_build: bool, // Effectively force_reinstall for casks
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        println!("==> Installing cask: {}", name);

        let cask = api::get_cask(name).await.map_err(|e| {
             SapphireError::Generic(format!("Failed to fetch cask '{}': {}", name, e))
        })?;

        if cask.is_installed() {
            if let Some(installed_version) = cask.installed_version() {
                let current_version = cask.version.clone().unwrap_or_else(|| "unknown".to_string());

                if installed_version == current_version && !force_build {
                    println!("==> Cask '{}' is already installed (version {})", name, installed_version);
                    return Ok(());
                } else if installed_version != current_version {
                    println!("==> Upgrading cask '{}' from {} to {}", name, installed_version, current_version);
                    // Consider adding uninstall logic here before proceeding if upgrading
                } else {
                     println!("==> Reinstalling cask '{}' (version {}) due to force flag", name, installed_version);
                     // Consider adding uninstall logic here before proceeding if reinstalling
                }
            } else {
                 // Installed but couldn't determine version - treat as needing reinstall/upgrade
                 println!("==> Cask '{}' is installed, but version unknown. Proceeding with installation.", name);
            }
        }

        // Install dependencies first
        install_cask_dependencies(&cask, cache, force_build).await?;

        // Download the cask artifact
        let download_path = build::cask::download_cask(&cask, cache).await.map_err(|e| {
            SapphireError::Generic(format!("Failed to download cask '{}': {}", name, e))
        })?;

        // Install from the downloaded artifact
        build::cask::install_cask(&cask, &download_path)?;

        println!("==> Successfully installed cask {}", cask.display_name());
        Ok(())
    })
}

fn install_cask_dependencies<'a>(
    cask: &'a Cask,
    cache: &'a Cache,
    force_build: bool, // Pass force_reinstall flag
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        if let Some(deps) = &cask.depends_on {
            // Load config only if needed
            let config_result = Config::load();

            // --- Handle Formula Dependencies for Cask ---
            if let Some(formula_deps) = &deps.formula {
                if !formula_deps.is_empty() {
                    println!("==> Installing formula dependencies for cask {}: {:?}", cask.token, formula_deps);
                     let config = config_result.as_ref().map_err(|e| SapphireError::Generic(format!("{}", e)))?; // Convert error to SapphireError
                     let args_for_deps = InstallArgs {
                        names: formula_deps.clone(),
                        skip_deps: false, // Don't skip dependencies of dependencies
                        cask: false,
                        build_from_source: force_build, // Propagate flag? Maybe casks shouldn't force source for formula deps?
                        include_optional: false, // Typically don't include optional deps for cask dependencies
                        skip_recommended: false,
                    };
                    // Recursively call execute for formula dependencies
                    execute(&args_for_deps, config).await?;
                }
            }

            // --- Handle Cask Dependencies for Cask ---
            if let Some(cask_deps) = &deps.cask {
                if !cask_deps.is_empty() {
                    println!("==> Installing cask dependencies for cask {}: {:?}", cask.token, cask_deps);
                    for dep_name in cask_deps {
                        // Recursively call install_cask for cask dependencies
                        install_cask(dep_name, cache, force_build).await?;
                    }
                }
            }
        }
        Ok(())
    })
}