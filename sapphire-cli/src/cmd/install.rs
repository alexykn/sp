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
                     None => { continue; } // Already warned
                 };

                if resolved_dep.status != ResolutionStatus::Installed {
                    println!("==> Installing target (deps skipped): {}", target_name);
                    let installed_paths_for_env = get_all_currently_installed_opt_paths(&resolver, &keg_registry)?;
                    // Standard install call, build_from_source will handle potential fallback internally if download fails
                    install_formula_internal(
                        resolved_dep.formula.clone(),
                        resolved_dep,
                        config,
                        &installed_paths_for_env,
                        args.build_from_source,
                    ).await?;
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
                    if dep.status == ResolutionStatus::Installed {
                        dep.opt_path.as_ref().and_then(|p| if p.exists() { Some((dep.formula.name().to_string(), p.clone())) } else { None })
                    } else {
                        None
                    }
                })
                .collect();

             add_globally_installed_paths(&resolver, &keg_registry, &mut installed_opt_paths, &mut install_status)?;

            let total_to_install_initially = install_status
                .iter()
                .filter(|(_, status)| **status == ResolutionStatus::Missing || **status == ResolutionStatus::Requested)
                .count();

            let mut remaining_to_install = total_to_install_initially;
            let mut pass_count = 0;
            let max_passes = install_status.len() + 2;

            while remaining_to_install > 0 && pass_count < max_passes {
                pass_count += 1;
                let mut progress_made_this_pass = false;
                let mut newly_installed_in_pass = 0;

                let keys_to_process: Vec<String> = install_status
                    .iter()
                    .filter(|(name, status)| {
                        (**status == ResolutionStatus::Missing || **status == ResolutionStatus::Requested)
                        && resolved_graph.install_plan.iter().any(|d| d.formula.name() == *name)
                    })
                    .map(|(name, _)| name.clone())
                    .collect();

                println!("--- Installation Pass {} ({} remaining) ---", pass_count, keys_to_process.len());

                if keys_to_process.is_empty() { break; }

                for name in keys_to_process {
                     let current_status = install_status.get(&name).cloned().unwrap_or(ResolutionStatus::Missing);
                    if current_status != ResolutionStatus::Missing && current_status != ResolutionStatus::Requested { continue; }

                    let resolved_dep = match resolved_graph.install_plan.iter().find(|d| d.formula.name() == name) {
                         Some(dep) => dep,
                         None => { continue; } // Should not happen if keys_to_process is correct
                    };

                    let direct_deps = match resolved_dep.formula.dependencies() {
                        Ok(deps) => deps,
                        Err(_e) => { // Prefix unused variable
                             eprintln!("Error getting dependencies for {}: {}. Skipping.", name, _e);
                             continue;
                        }
                    };

                    let mut all_deps_met = true;
                    for dep in &direct_deps {
                        if !should_consider_dependency(dep, args) { continue; }

                        let dep_status = install_status.get(&dep.name).cloned();
                        let is_met = match dep_status {
                            Some(ResolutionStatus::Installed) => check_and_update_opt_path(&dep.name, &keg_registry, &mut installed_opt_paths),
                            Some(ResolutionStatus::SkippedOptional) => true,
                            _ => { // Check KegRegistry
                                if keg_registry.get_installed_keg(&dep.name)?.is_some() {
                                    if check_and_update_opt_path(&dep.name, &keg_registry, &mut installed_opt_paths) {
                                         install_status.insert(dep.name.clone(), ResolutionStatus::Installed); // Update status map
                                         true
                                     } else { false }
                                } else {
                                    // Check if it was built via fallback
                                    let fallback_prefix = build::fallback::get_fallback_install_prefix(&dep.name, config)?;
                                    if fallback_prefix.exists() {
                                        log::info!("Dependency '{}' met via fallback build at {}", dep.name, fallback_prefix.display());
                                        // Add fallback prefix to installed paths (maybe opt path isn't linked yet)
                                        // We should use the *standard* opt path conceptually,
                                        // assuming the fallback build gets linked later.
                                         if check_and_update_opt_path(&dep.name, &keg_registry, &mut installed_opt_paths) {
                                            install_status.insert(dep.name.clone(), ResolutionStatus::Installed);
                                            true
                                        } else {
                                            // Even if opt isn't linked, consider it met if fallback exists
                                            log::warn!("Fallback for '{}' exists but opt path is not linked. Considering met.", dep.name);
                                            install_status.insert(dep.name.clone(), ResolutionStatus::Installed);
                                            true
                                        }
                                    } else {
                                        false // Truly missing
                                    }
                                }
                            }
                        };

                        if !is_met {
                            println!("Dependency '{}' for '{}' not met.", dep.name, name);
                            all_deps_met = false;
                            break;
                        }
                    }


                    if all_deps_met {
                        println!("All dependencies met for: {}", name);
                        let all_current_installed_paths: Vec<PathBuf> = installed_opt_paths.values().cloned().collect();

                        match install_formula_internal(
                            resolved_dep.formula.clone(),
                            resolved_dep,
                            config,
                            &all_current_installed_paths, // Pass ALL known installed paths
                            args.build_from_source,
                        ).await {
                            Ok(installed_opt_path) => {
                                println!("Successfully installed {} internally.", name);
                                install_status.insert(name.clone(), ResolutionStatus::Installed);
                                installed_opt_paths.insert(name.clone(), installed_opt_path);
                                progress_made_this_pass = true;
                                newly_installed_in_pass += 1;
                            }
                            Err(e) => {
                                eprintln!("Error installing {}: {}", name, e);
                                return Err(e);
                            }
                        }
                    } else {
                         println!("Deferring installation of '{}' until dependencies are ready.", name);
                    }
                }

                remaining_to_install -= newly_installed_in_pass;

                if !progress_made_this_pass && remaining_to_install > 0 {
                    let remaining_names: Vec<_> = install_status.iter().filter(|(_, s)| **s == ResolutionStatus::Missing || **s == ResolutionStatus::Requested).map(|(n, _)| n.clone()).collect();
                    eprintln!("Error: No progress made in installation pass {}. Could not install: {:?}", pass_count, remaining_names);
                    return Err(SapphireError::DependencyError(format!("Unresolved dependencies or cycle after pass {}. Remaining: {:?}", pass_count, remaining_names)));
                }

            } // End while loop

            // Final check
             if remaining_to_install > 0 {
                 let remaining_names: Vec<_> = install_status.iter().filter(|(_, s)| **s == ResolutionStatus::Missing || **s == ResolutionStatus::Requested).map(|(n, _)| n.clone()).collect();
                 eprintln!("Error: Installation incomplete (max passes {} reached or other issue): {:?}", max_passes, remaining_names);
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
    if tags.contains(DependencyTag::TEST) { return false; }
    if tags.contains(DependencyTag::OPTIONAL) && !args.include_optional { return false; }
    if tags.contains(DependencyTag::RECOMMENDED) && args.skip_recommended { return false; }
    true
}

/// Helper to check KegRegistry for an installed package and update the opt_paths map if needed.
fn check_and_update_opt_path(
    name: &str,
    keg_registry: &KegRegistry,
    installed_opt_paths: &mut HashMap<String, PathBuf>
) -> bool {
    if installed_opt_paths.contains_key(name) {
        return true;
    }
    // Check standard opt path first
    let opt_path = keg_registry.get_opt_path(name);
    if opt_path.exists() {
         installed_opt_paths.insert(name.to_string(), opt_path);
         return true;
    }
    // Check if installed via keg registry (might not be linked yet)
    if keg_registry.get_installed_keg(name).unwrap_or(None).is_some() {
        // Opt path doesn't exist, but keg does. Still needs linking.
        // For dependency checking, consider it met but don't add to map yet.
        log::warn!("Keg found for {} but opt path {} doesn't exist.", name, opt_path.display());
        // Don't add to map yet, but return true for dependency check.
        return true;
    }
    false
}

/// Helper to get opt paths for ALL known installed dependencies (from resolver + registry)
 fn get_all_currently_installed_opt_paths(
     resolver: &DependencyResolver, // Use pub(crate) resolver.resolved directly
     keg_registry: &KegRegistry
 ) -> Result<Vec<PathBuf>> {
     let mut paths = HashSet::new(); // Use HashSet to avoid duplicates

     // Access the resolved map using pub(crate) field access
     for (_, resolved_dep) in resolver.resolved.iter() {
         if resolved_dep.status == ResolutionStatus::Installed {
             if let Some(opt_path) = &resolved_dep.opt_path {
                 if opt_path.exists() {
                     paths.insert(opt_path.clone());
                 }
             }
         }
     }

     // Add paths from anything else installed according to KegRegistry (using list_installed_kegs)
     if let Ok(installed_kegs) = keg_registry.list_installed_kegs() { // Corrected method call
         for keg in installed_kegs {
             let opt_path = keg_registry.get_opt_path(&keg.name);
             if opt_path.exists() {
                 paths.insert(opt_path);
             }
         }
     }

     // Add paths from fallback builds if they exist
     let config = Config::load()?; // Load config to get cache dir
     for dep_name in ["m4", "autoconf", "libtool"] { // Check known fallback tools
         let fallback_prefix = build::fallback::get_fallback_install_prefix(dep_name, &config)?;
         if fallback_prefix.exists() {
             // Conceptually, the 'opt' path for a fallback should be the fallback prefix itself
             // if we're passing it to build environment.
             paths.insert(fallback_prefix);
         }
     }


     Ok(paths.into_iter().collect())
 }


 /// Pre-populate installed_opt_paths and install_status with globally installed kegs
 fn add_globally_installed_paths(
     _resolver: &DependencyResolver, // Use pub(crate) resolver.resolved directly
     keg_registry: &KegRegistry,
     installed_opt_paths: &mut HashMap<String, PathBuf>,
     install_status: &mut HashMap<String, ResolutionStatus>
 ) -> Result<()> {
      if let Ok(installed_kegs) = keg_registry.list_installed_kegs() { // Corrected method call
         for keg in installed_kegs {
             let name = keg.name.clone();
             let opt_path = keg_registry.get_opt_path(&name);
             if opt_path.exists() { // Only add if opt path exists
                 installed_opt_paths.entry(name.clone()).or_insert(opt_path);
                 install_status.entry(name).or_insert(ResolutionStatus::Installed);
             } else if keg.path.exists() { // Keg exists but not linked
                 // Still mark as installed in the status map
                  install_status.entry(name).or_insert(ResolutionStatus::Installed);
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
    all_installed_paths: &[PathBuf],
    force_build: bool,
) -> Result<PathBuf> {
    let name = formula.name();
    println!("==> Starting installation process for: {}", name);

    match resolved_info.status {
        ResolutionStatus::Missing | ResolutionStatus::Requested => {
            println!("==> Installing formula: {}", name);
            // Proceed with download/build/fallback logic below
        }
        ResolutionStatus::Installed => {
            let keg_path_option = &resolved_info.keg_path; // Option<PathBuf>
            let opt_path_option = &resolved_info.opt_path; // Option<PathBuf>

            let keg_path = keg_path_option.as_deref().ok_or_else(|| SapphireError::Generic(format!("Installed formula {} missing keg path in resolved info", name)))?;
            let opt_path = opt_path_option.as_deref().ok_or_else(|| SapphireError::Generic(format!("Installed formula {} missing opt path in resolved info", name)))?;

            // Check if the actual keg directory exists
            if !keg_path.exists() {
                 log::warn!("Formula {} marked installed, but keg path {} does not exist. Attempting re-install.", name, keg_path.display());
                 // Fall through to install logic
            } else {
                // Keg exists, check if opt link exists
                 println!("Formula {} is already installed (Version: {} at {}).", name, resolved_info.formula.version_str_full(), keg_path.display());
                 if !opt_path.exists() {
                    println!("==> Opt link missing, linking installed formula: {}", name);
                    build::formula::link::link_formula_artifacts(&formula, keg_path)?;
                 }
                 return Ok(opt_path.to_path_buf()); // Return the opt path
            }
        }
        ResolutionStatus::SkippedOptional => {
            println!("Formula {} was skipped (optional/recommended).", name);
            return Err(SapphireError::Generic(format!("Attempted to install skipped formula {}", name)));
        }
    }


    let install_dir = build::formula::get_formula_cellar_path(&formula); // Standard cellar path
    let opt_path = build::get_formula_opt_path(&formula); // Standard opt path

    let download_path_result = if build::formula::has_bottle_for_current_platform(&formula) && !force_build {
         build::formula::bottle::download_bottle(&formula, config).await
     } else {
         build::formula::source::download_source(&formula, config).await
     };

    let mut source_or_bottle_path: Option<PathBuf> = None;
    let mut use_bottle = false;
    let mut trigger_fallback = false;

    match download_path_result {
        Ok(path) => {
            source_or_bottle_path = Some(path);
            use_bottle = build::formula::has_bottle_for_current_platform(&formula) && !force_build;
        }
        Err(SapphireError::DownloadError(_, _, _)) => {
            // Specific download error occurred, check if it's a fallback candidate
            if ["m4", "autoconf", "libtool"].contains(&name) {
                log::warn!("Download failed for {}. Attempting fallback build...", name);
                trigger_fallback = true;
                // source_or_bottle_path remains None
            } else {
                 log::error!("Download failed for {}: {}", name, download_path_result.unwrap_err());
                 return Err(SapphireError::InstallError(format!("Download failed for {}", name)));
            }
        }
        Err(e) => { // Other errors during download attempt
             log::error!("Error during download attempt for {}: {}", name, e);
             return Err(SapphireError::InstallError(format!("Download preparation failed for {}: {}", name, e)));
        }
    };


    // Ensure parent directory for install_dir exists
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SapphireError::Io(e))?;
    }


    // Perform installation: bottle, source build, or fallback build
    if use_bottle && source_or_bottle_path.is_some() {
        let bottle_path = source_or_bottle_path.unwrap();
        println!("==> Pouring bottle: {}", bottle_path.display());
        build::formula::bottle::install_bottle(&bottle_path, &formula)?;
    } else if trigger_fallback {
         // Fallback is handled within build_from_source when path is None
         // Pass None to signal fallback attempt
         build::formula::source::build_from_source(None, &formula, config, all_installed_paths).await?;
          // Fallback build might install to a different prefix. Need to link it.
         let fallback_prefix = build::fallback::get_fallback_install_prefix(name, config)?;
         if fallback_prefix.exists() {
             println!("==> Linking fallback build from {} to standard prefix", fallback_prefix.display());
             // This is complex: needs to find artifacts in fallback_prefix and link them
             // to the standard locations (e.g., config.prefix/bin, config.prefix/lib).
             // For now, just log a warning.
             log::warn!("Post-fallback linking not fully implemented. Manual linking might be required.");
             // Maybe try linking the entire fallback dir to the standard cellar location? Risky.
             // Or link individual standard directories (bin, lib, include)
             link_fallback_artifacts(&fallback_prefix, config, &install_dir, &formula)?;
         } else {
             return Err(SapphireError::InstallError(format!("Fallback build triggered for {} but no fallback directory found at {}", name, fallback_prefix.display())));
         }

    } else if let Some(source_path) = source_or_bottle_path {
        // Build from source (standard path)
        if force_build {
            println!("==> Building from source (forced): {}", name);
        } else {
            println!("==> Building from source (no bottle available or download failed for non-fallback tool): {}", name);
        }
        // Pass the downloaded source path
        build::formula::source::build_from_source(Some(source_path), &formula, config, all_installed_paths).await?;
    } else {
         // Should not happen if logic above is correct
         return Err(SapphireError::Generic(format!("Installation logic error for {}", name)));
    }

    // Standard linking (should happen after bottle, source, or fallback install)
    // Link from the *standard* cellar path, assuming fallback link step worked.
    println!("==> Linking formula: {}", name);
    if install_dir.exists() {
        build::formula::link::link_formula_artifacts(&formula, &install_dir)?;
    } else {
        log::error!("Standard install directory {} does not exist after installation attempt. Cannot link.", install_dir.display());
        return Err(SapphireError::InstallError(format!("Installation directory {} not found after build/install of {}", install_dir.display(), name)));
    }


    println!("🍺 Successfully installed {} ({})", formula.name(), install_dir.display());

    Ok(opt_path) // Return the standard opt path
}


/// Links artifacts from a fallback build prefix to standard locations.
/// This is a simplified version and might need refinement.
fn link_fallback_artifacts(
    fallback_prefix: &PathBuf,
    config: &Config,
    _standard_cellar_path: &PathBuf, // Unused for now, maybe needed later
    formula: &Formula,
) -> Result<()> {
    log::info!("Attempting to link fallback artifacts from {} for {}", fallback_prefix.display(), formula.name());
    let main_prefix = &config.prefix;
    let mut symlinks_created: Vec<String> = Vec::new(); // Track links for potential manifest

    // Link common directories (bin, lib, include, share)
    for dir_name in ["bin", "lib", "include", "share"] {
        let source_dir = fallback_prefix.join(dir_name);
        let target_dir = main_prefix.join(dir_name);

        if source_dir.is_dir() {
            log::debug!("Linking contents of {}", source_dir.display());
            std::fs::create_dir_all(&target_dir)?; // Ensure target parent exists

            for entry_res in std::fs::read_dir(&source_dir)? {
                let entry = entry_res?;
                let source_item_path = entry.path();
                let item_name = entry.file_name();
                let target_item_path = target_dir.join(&item_name);

                // Remove existing file/link at target if it exists
                if target_item_path.exists() || target_item_path.symlink_metadata().is_ok() {
                     log::warn!("Removing existing item at link target: {}", target_item_path.display());
                     if target_item_path.is_dir() && !target_item_path.symlink_metadata()?.file_type().is_symlink() {
                         std::fs::remove_dir_all(&target_item_path)?;
                     } else {
                         std::fs::remove_file(&target_item_path)?;
                     }
                }

                // Create symlink
                log::debug!("Linking {} -> {}", target_item_path.display(), source_item_path.display());
                match std::os::unix::fs::symlink(&source_item_path, &target_item_path) {
                    Ok(_) => symlinks_created.push(target_item_path.to_string_lossy().to_string()),
                    Err(e) => log::error!("Failed to link {} -> {}: {}", target_item_path.display(), source_item_path.display(), e),
                }
            }
        } else {
             log::debug!("Source directory {} does not exist in fallback prefix, skipping.", source_dir.display());
        }
    }

    // TODO: Write a manifest? Where should it live for fallback builds?
    // Maybe write it inside the fallback_prefix directory?
    if !symlinks_created.is_empty() {
         let manifest_path = fallback_prefix.join("FALLBACK_MANIFEST.json");
         log::info!("Writing fallback link manifest to {}", manifest_path.display());
         let manifest_json = serde_json::to_string_pretty(&symlinks_created)
             .map_err(|e| SapphireError::Generic(e.to_string()))?;
         if let Err(e) = std::fs::write(&manifest_path, manifest_json) {
              log::error!("Failed to write fallback manifest: {}", e);
         }
    }


    log::info!("Finished linking fallback artifacts for {}", formula.name());
    Ok(())
}


// --- Cask Installation ---

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
                } else {
                     println!("==> Reinstalling cask '{}' (version {}) due to force flag", name, installed_version);
                }
            }
        }

        install_cask_dependencies(&cask, cache, force_build).await?;

        let download_path = build::cask::download_cask(&cask, cache).await.map_err(|e| {
            SapphireError::Generic(format!("Failed to download cask '{}': {}", name, e))
        })?;

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
            let config = Config::load()?;

            // --- Handle Formula Dependencies for Cask ---
            if let Some(formula_deps) = &deps.formula {
                if !formula_deps.is_empty() {
                    println!("==> Installing formula dependencies for cask {}: {:?}", cask.token, formula_deps);
                    let args_for_deps = InstallArgs {
                        names: formula_deps.clone(),
                        skip_deps: false,
                        cask: false,
                        build_from_source: force_build,
                        include_optional: false,
                        skip_recommended: false,
                    };
                    execute(&args_for_deps, &config).await?;
                }
            }

            // --- Handle Cask Dependencies for Cask ---
            if let Some(cask_deps) = &deps.cask {
                if !cask_deps.is_empty() {
                    println!("==> Installing cask dependencies for cask {}: {:?}", cask.token, cask_deps);
                    for dep in cask_deps {
                        install_cask(dep, cache, force_build).await?;
                    }
                }
            }
        }
        Ok(())
    })
}