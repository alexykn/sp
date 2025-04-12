use sapphire_core::utils::error::{SapphireError, Result};
use sapphire_core::model::formula::Formula;
use sapphire_core::utils::config::Config;
use sapphire_core::build;
//use std::collections::HashSet; Unused
use futures::future::BoxFuture;
use sapphire_core::fetch::api;
use sapphire_core::utils::cache::Cache;
use clap::Args;
use sapphire_core::model::cask::Cask;
use sapphire_core::dependency::{DependencyResolver, ResolutionContext, ResolvedDependency, DependencyTag, ResolutionStatus}; // Import resolver components
use sapphire_core::keg::KegRegistry; // Import KegRegistry
use std::sync::Arc; // Use Arc for formula sharing across threads


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
        // Keep cask installation simple for now, resolve deps individually if needed
        for name in &args.names {
             install_cask(name, &cache, args.build_from_source).await?; // Pass force_build flag
        }
    } else {
        // Resolve dependencies for all targets *together*
        let resolved_graph = resolver.resolve_targets(&args.names)?;

        // Install dependencies first based on the plan
        if !args.skip_deps {
             println!("==> Processing installation plan...");
             for resolved_dep in &resolved_graph.install_plan {
                 match resolved_dep.status {
                     ResolutionStatus::Missing | ResolutionStatus::Requested => {
                         println!("==> Installing required dependency: {}", resolved_dep.formula.name());
                         // Pass the already resolved build dependency paths for *this specific dependency*
                         // This requires resolving deps for the dependency itself if not already done implicitly...
                         // For simplicity now, we'll rely on the build_env setup inside install_formula_internal
                         // to fetch necessary build deps if needed, but ideally, resolution is fully upfront.
                         install_formula_internal(
                             resolved_dep.formula.clone(), // Pass Arc<Formula>
                             resolved_dep,                 // Pass the ResolvedDependency info
                             config,
                             &resolved_graph.build_dependency_opt_paths, // Pass build paths relevant for *this* build
                             args.build_from_source,
                             false // Don't re-resolve deps inside internal call
                         ).await?;
                     }
                     ResolutionStatus::Installed => {
                          println!("Dependency {} already installed.", resolved_dep.formula.name());
                     }
                      ResolutionStatus::SkippedOptional => { /* Should not be in install_plan */ }
                 }
             }
        } else {
             println!("Skipping dependency installation due to --skip-deps flag.");
             // Still need to install the targets themselves
             for target_name in &args.names {
                 if let Some(resolved_dep) = resolved_graph.install_plan.iter().find(|d| d.formula.name() == target_name) {
                      if resolved_dep.status != ResolutionStatus::Installed {
                         println!("==> Installing target (deps skipped): {}", target_name);
                         install_formula_internal(
                             resolved_dep.formula.clone(),
                             resolved_dep,
                             config,
                             &resolved_graph.build_dependency_opt_paths, // Pass relevant build paths
                             args.build_from_source,
                             false // Deps were skipped
                         ).await?;
                      } else {
                          println!("Target {} already installed.", target_name);
                      }
                 } else {
                      // This might happen if the target itself was skipped (e.g., optional)
                      eprintln!("Warning: Target {} not found in the final install plan.", target_name);
                 }
             }
        }
    }


    Ok(())
}

/// Internal function to handle the actual installation of a single formula.
/// Assumes dependencies have been handled by the caller if `process_deps` is false.
async fn install_formula_internal(
    formula: Arc<Formula>, // Use Arc<Formula>
    resolved_info: &ResolvedDependency, // Pass resolved info
    config: &Config,
    // Pass build dependency paths needed for *this* formula's build
    build_dep_paths: &[std::path::PathBuf],
    force_build: bool,
    _process_deps: bool, // Flag to indicate if this call should handle deps (now usually false)
) -> Result<()> {
    let name = formula.name();
    println!("==> Starting installation process for: {}", name);

    // Check installation status using resolved_info
    match resolved_info.status {
        ResolutionStatus::Installed => {
            println!("Formula {} is already installed (Version: {} at {}). Skipping installation.", name, resolved_info.formula.version_str_full(), resolved_info.keg_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(||"N/A".to_string()));
            // Ensure it's linked? Linking might happen separately.
            if let Some(keg_path) = &resolved_info.keg_path {
                 let opt_path = resolved_info.opt_path.as_ref().cloned().unwrap_or_else(|| build::get_formula_opt_path(&formula));
                 if !opt_path.exists() {
                     println!("==> Linking installed formula: {}", name);
                     build::formula::link::link_formula_artifacts(&formula, keg_path)?;
                 }
            }
            return Ok(());
        }
        ResolutionStatus::SkippedOptional => {
            println!("Formula {} was skipped (optional/recommended).", name);
            return Ok(());
        }
        ResolutionStatus::Missing | ResolutionStatus::Requested => {
            // Proceed with installation
            println!("==> Installing formula: {}", name);
        }
    }


    let install_dir = build::formula::get_formula_cellar_path(&formula);

    // --- Build Tool Check --- (Optional, but helpful for user feedback)
    let build_tools_needed = formula_needs_build_tools(&formula);
    if build_tools_needed {
         println!("Formula {} requires build tools (autoconf, automake, libtool, m4, pkg-config)", name);
         // We assume the resolver ensured these are present in build_dep_paths if needed.
         // A check could be added here to verify paths for these tools exist in build_dep_paths.
         for tool in ["autoconf", "automake", "libtool", "m4", "pkg-config"].iter() {
             if !build_dep_paths.iter().any(|p| p.ends_with(format!("opt/{}", tool))) {
                  // This check is basic, might need refinement based on actual path format
                  println!("Warning: Build tool '{}' might be missing from resolved build paths.", tool);
             }
         }
    }
    // --- End Build Tool Check ---


    let download_path = build::formula::download_formula(&formula, config).await.map_err(|e| {
        SapphireError::Generic(format!("Failed to download formula '{}': {}", name, e))
    })?;

    // Determine build strategy (bottle or source)
    let use_bottle = !force_build && build::formula::has_bottle_for_current_platform(&formula);

    if use_bottle {
        println!("==> Pouring bottle: {}", download_path.display());
        build::formula::bottle::install_bottle(&download_path, &formula)?;
    } else {
        if force_build {
            println!("==> Building from source (forced): {}", name);
        } else {
            println!("==> Building from source (no bottle available): {}", name);
        }
        // Pass the resolved build dependency paths here
        build::formula::source::build_from_source(&download_path, &formula, config, build_dep_paths)?;
    }

    // Link after successful installation (bottle or source)
    println!("==> Linking formula: {}", name);
    build::formula::link::link_formula_artifacts(&formula, &install_dir)?;

    println!("🍺 Successfully installed {} ({})", formula.name(), install_dir.display());

    Ok(())
}


/// Check if the formula likely needs autotools to build
fn formula_needs_build_tools(formula: &Formula) -> bool {
    // Check if formula name matches one of the build tools themselves
    if ["autoconf", "automake", "libtool", "m4", "pkgconf", "pkg-config"].contains(&formula.name.as_str()) {
        return true;
    }

    // If building from source and has certain build/runtime dependencies, likely needs autotools
    if dependency_names_contain(&formula.dependencies, DependencyTag::BUILD, "autoconf")
        || dependency_names_contain(&formula.dependencies, DependencyTag::BUILD, "automake")
        || dependency_names_contain(&formula.dependencies, DependencyTag::BUILD, "libtool")
        || dependency_names_contain(&formula.dependencies, DependencyTag::BUILD, "m4")
        || dependency_names_contain(&formula.dependencies, DependencyTag::BUILD, "pkg-config")
        || dependency_names_contain(&formula.dependencies, DependencyTag::BUILD, "pkgconf")
    {
        return true;
    }

    // Check if we'll need autoconf (configure.ac/.in existence could be checked if we had source)
    // These are common packages that typically use autoconf
    let autotools_packages = [
        "ncurses", "readline", "bash", "coreutils", "diffutils", "pkg-config",
        "pkgconf", "findutils", "gawk", "gnu-", "gdb", "gdbm", "gettext",
        "gmp", "grep", "htop", "libffi", "libgcrypt", "libgpg-error", "libtool" // Added libtool here too
    ];

    for pkg in autotools_packages {
        if formula.name.contains(pkg) {
             // Crude check: if it doesn't seem to use CMake explicitly, assume autotools
             // A better check would involve inspecting the source or formula definition more deeply
             // For now, if name matches and it doesn't obviously depend on cmake, assume autotools needed.
             if !dependency_names_contain(&formula.dependencies, DependencyTag::BUILD, "cmake") {
                  println!("Assuming '{}' needs autotools based on name and lack of CMake dependency.", formula.name);
                 return true;
             }
        }
    }

    false
}


/// Helper to check if a dependency list contains a dependency by name and tag
fn dependency_names_contain(deps: &[sapphire_core::dependency::Dependency], tag_mask: DependencyTag, name: &str) -> bool {
    deps.iter().any(|dep| dep.name == name && dep.tags.intersects(tag_mask))
}


// --- Cask Installation --- (Remains largely the same, but pass force_build)

// Use boxing for async recursion
fn install_cask<'a>(
    name: &'a str,
    cache: &'a Cache,
    force_build: bool, // Added force_build flag (though casks don't really "build")
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        println!("==> Installing cask: {}", name);

        // Casks don't usually have explicit build deps like formulas,
        // but might depend on *other* formulas or casks.
        let cask = api::get_cask(name).await.map_err(|e| {
             SapphireError::Generic(format!("Failed to fetch cask '{}': {}", name, e))
        })?;

        if cask.is_installed() {
            if let Some(installed_version) = cask.installed_version() {
                let current_version = cask.version.clone().unwrap_or_else(|| "unknown".to_string());

                if installed_version == current_version && !force_build { // Check force_build flag
                    println!("==> Cask '{}' is already installed (version {})", name, installed_version);
                    return Ok(());
                } else if installed_version != current_version {
                    println!("==> Upgrading cask '{}' from {} to {}",
                             name, installed_version, current_version);
                     // TODO: Implement uninstall logic before reinstalling for upgrade
                     // For now, just proceed with install which might overwrite or fail depending on cask type.
                } else {
                     println!("==> Reinstalling cask '{}' (version {}) due to force flag", name, installed_version);
                     // TODO: Implement uninstall logic first?
                }
            }
        }

        // Install cask dependencies (other formulas or casks)
        install_cask_dependencies(&cask, cache, force_build).await?; // Pass force_build down

        let download_path = build::cask::download_cask(&cask, cache).await.map_err(|e| {
            SapphireError::Generic(format!("Failed to download cask '{}': {}", name, e))
        })?;

        build::cask::install_cask(&cask, &download_path)?;

        println!("==> Successfully installed cask {}", cask.display_name());
        Ok(())
    })
}

// Use boxing for async recursion (implicitly via install_cask and potentially install_formula_internal)
fn install_cask_dependencies<'a>(
    cask: &'a Cask,
    cache: &'a Cache,
    force_build: bool, // Pass force_build flag
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        if let Some(deps) = &cask.depends_on {
             // --- Handle Formula Dependencies for Cask ---
            if let Some(formula_deps) = &deps.formula {
                if !formula_deps.is_empty() {
                    println!("==> Installing formula dependencies for cask {}: {:?}", cask.token, formula_deps);
                    let config = Config::load()?; // Load config here for formula install
                    let formulary = sapphire_core::formulary::Formulary::new(config.clone());
                    let keg_registry = KegRegistry::new(config.clone());

                     // Resolve these formula dependencies
                     let context = ResolutionContext {
                         formulary: &formulary,
                         keg_registry: &keg_registry,
                         sapphire_prefix: &config.prefix,
                         include_optional: false, // Don't pull optional deps for cask deps by default
                         include_test: false,
                         skip_recommended: false,
                         force_build, // Respect force_build flag
                     };
                     let mut resolver = DependencyResolver::new(context);
                     let resolved_graph = resolver.resolve_targets(formula_deps)?;

                     // Install the resolved formula dependencies
                     for resolved_dep in &resolved_graph.install_plan {
                         match resolved_dep.status {
                             ResolutionStatus::Missing | ResolutionStatus::Requested => {
                                 println!("==> Installing formula dependency for cask: {}", resolved_dep.formula.name());
                                 install_formula_internal(
                                     resolved_dep.formula.clone(),
                                     resolved_dep,
                                     &config, // Pass loaded config
                                     &resolved_graph.build_dependency_opt_paths, // Pass relevant build paths
                                     force_build, // Pass force_build flag
                                     false // Deps resolved upfront
                                 ).await?;
                             }
                             ResolutionStatus::Installed => {
                                 println!("Formula dependency {} already installed.", resolved_dep.formula.name());
                             }
                              ResolutionStatus::SkippedOptional => {}
                         }
                     }
                }
            }

             // --- Handle Cask Dependencies for Cask ---
            if let Some(cask_deps) = &deps.cask {
                if !cask_deps.is_empty() {
                    println!("==> Installing cask dependencies for cask {}: {:?}", cask.token, cask_deps);
                    for dep in cask_deps {
                        // Recursively call install_cask, passing the force_build flag
                        install_cask(dep, cache, force_build).await?;
                    }
                }
            }
        }

        Ok(())
    })
}