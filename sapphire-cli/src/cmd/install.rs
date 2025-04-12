// src/cmd/install.rs
// Contains the logic for the `install` command.

use sapphire_core::utils::error::{SapphireError, Result};
use sapphire_core::model::formula::Formula;
use sapphire_core::utils::config::Config;
use sapphire_core::build;
use std::collections::HashSet;
use futures::future::BoxFuture;
use sapphire_core::fetch::api;
use sapphire_core::utils::cache::Cache;
use clap::Args;
use sapphire_core::model::cask::Cask;


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
}

pub async fn execute(args: &InstallArgs, config: &Config) -> Result<()> {
    let cache = Cache::new(&config.cache_dir).map_err(|e| {
        SapphireError::Generic(format!("Failed to initialize cache: {}", e))
    })?;

    for name in &args.names {
        if args.cask {
            install_cask(name, &cache).await?;
        } else {
            install_as_formula(name, config, &cache, !args.skip_deps).await?;
        }
    }

    Ok(())
}

// Use boxing for async recursion
fn install_as_formula<'a>(
    name: &'a str,
    config: &'a Config,
    cache: &'a Cache,
    include_deps: bool
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        println!("==> Installing formula: {}", name);

        let formula = api::get_formula(name).await.map_err(|e| {
            SapphireError::Generic(format!("Failed to fetch formula '{}': {}", name, e))
        })?;

        // Determine install path *before* dependency check to avoid duplicate installs
        let install_dir = build::formula::get_formula_cellar_path(&formula);
        let opt_path = build::get_formula_opt_path(&formula);

        // Check if already installed (using opt path is slightly better than cellar path)
        if opt_path.exists() {
            println!("Formula {} is already installed and linked", name);
            // Return info about the existing installation
            println!("Already installed: {}", name);
            return Ok(());
        }

        // Check if we need to ensure autoconf, automake and libtool are installed
        // before attempting to build other formulas
        let build_tools_needed = formula_needs_build_tools(&formula);

        if build_tools_needed && include_deps {
            // Ensure autotools are available first
            println!("Formula {} requires build tools (autoconf, automake, libtool, m4)", name);

            // Make sure m4 is installed first as it's required by autoconf
            let m4_tool = "m4";
            if m4_tool != name {  // Avoid circular dependency
                let m4_exists = std::process::Command::new("which")
                    .arg(m4_tool)
                    .output()
                    .map(|output| output.status.success())
                    .unwrap_or(false);

                if !m4_exists {
                    println!("==> Installing required build tool: {}", m4_tool);
                    // Recursively install m4 first
                    install_as_formula(m4_tool, config, cache, true).await?;
                } else {
                    println!("==> Build tool m4 is already installed");
                }
            }

            // Now install other build tools
            for tool in ["autoconf", "automake", "libtool"].iter() {
                // Skip if we're trying to install the tool itself to avoid circular deps
                if *tool == name {
                    continue;
                }

                // Check if the tool exists in PATH
                let tool_exists = std::process::Command::new("which")
                    .arg(tool)
                    .output()
                    .map(|output| output.status.success())
                    .unwrap_or(false);

                if !tool_exists {
                    println!("==> Installing required build tool: {}", tool);
                    // Recursively install the required tool
                    install_as_formula(tool, config, cache, true).await?;
                } else {
                    println!("==> Build tool {} is already installed", tool);
                }
            }
        }

        if include_deps {
            // Process regular dependencies and get their resolved info
            process_formula_deps(&formula, config, cache).await?;
        }

        let download_path = build::formula::download_formula(&formula, config).await.map_err(|e| {
            SapphireError::Generic(format!("Failed to download formula '{}': {}", name, e))
        })?;

        if build::formula::has_bottle_for_current_platform(&formula) {
            build::formula::bottle::install_bottle(&download_path, &formula)?;
        } else {
            // Pass resolved dependencies to build_from_source
            build::formula::source::build_from_source(&download_path, &formula)?;
        }

        build::formula::link::link_formula_binaries(&formula, &install_dir)?;

        println!("==> Successfully installed {}", formula.name);

        // Return info about the newly installed formula
        Ok(())
    })
}

/// Check if the formula likely needs autotools to build
fn formula_needs_build_tools(formula: &Formula) -> bool {
    // Check if formula name matches one of the build tools themselves
    if formula.name == "autoconf" || formula.name == "automake" ||
       formula.name == "libtool" || formula.name == "m4" ||
       formula.name == "pkgconf" || formula.name == "pkg-config" {
        return true;
    }

    // Check if formula name suggests it's an autotools project
    if formula.name.contains("autoconf") || formula.name.contains("automake") ||
       formula.name.contains("libtool") || formula.name.contains("m4") {
        return true;
    }

    // If building from source and has certain build/runtime dependencies, likely needs autotools
    if dependency_names_contain(&formula.dependencies, "autoconf")
        || dependency_names_contain(&formula.dependencies, "automake")
        || dependency_names_contain(&formula.dependencies, "libtool")
        || dependency_names_contain(&formula.dependencies, "m4")
    {
        return true;
    }

    // No build_dependencies field in Formula; skip this block.

    // Check if we'll need autoconf (configure.ac/.in existence could be checked if we had source)
    // These are common packages that typically use autoconf
    let autotools_packages = [
        "ncurses", "readline", "bash", "coreutils", "diffutils", "pkg-config",
        "pkgconf", "findutils", "gawk", "gnu-", "gdb", "gdbm", "gettext",
        "gmp", "grep", "htop", "libffi", "libgcrypt", "libgpg-error"
    ];

    for pkg in autotools_packages {
        if formula.name.contains(pkg) {
            return true;
        }
    }

    false
}

// Use boxing for async recursion (implicitly via install_as_formula)
fn process_formula_deps<'a>(
    formula: &'a Formula,
    config: &'a Config,
    cache: &'a Cache
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        // Gather all relevant dependencies (runtime and build)
        let mut all_deps_set = HashSet::new();
        // Use DependencyExt to get all runtime and build-time dependencies
        all_deps_set.extend(formula.dependencies.iter().cloned());
        // Optionally add recommended dependencies if desired
        // all_deps_set.extend(formula.recommended_dependencies.iter().cloned());

        let all_deps: Vec<String> = all_deps_set.into_iter().map(|dep| dep.name.clone()).collect();

        let mut visited = HashSet::new(); // Keep track of visited deps during this call

        if !all_deps.is_empty() {
            println!(
                "==> Installing dependencies for {} (runtime + build): {:?}",
                formula.name,
                &all_deps
            );

            for dep_name in &all_deps {
                // Check visited *within this specific process_formula_deps call*
                // to avoid infinite loops for circular deps within this layer.
                // The install_as_formula function handles the global check (is it already installed?).
                if !visited.contains(dep_name.as_str()) {
                    install_as_formula(dep_name, config, cache, true).await?;
                    visited.insert(dep_name.clone());
                }
            }
        }

        // The resolved_deps_vec now contains ResolvedDependency info for all
        // runtime AND build dependencies that were installed/found.
        Ok(())
    })
}

/// Helper to check if a dependency list contains a dependency by name
fn dependency_names_contain(deps: &[sapphire_core::dependency::Dependency], name: &str) -> bool {
    deps.iter().any(|dep| (*dep).name == name)
}

// Use boxing for async recursion
fn install_cask<'a>(
    name: &'a str,
    cache: &'a Cache
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        println!("==> Installing cask: {}", name);

        let cask = api::get_cask(name).await.map_err(|e| {
            SapphireError::Generic(format!("Failed to fetch cask '{}': {}", name, e))
        })?;

        if cask.is_installed() {
            if let Some(installed_version) = cask.installed_version() {
                let current_version = cask.version.clone().unwrap_or_else(|| "unknown".to_string());

                if installed_version == current_version {
                    println!("==> Cask '{}' is already installed (version {})", name, installed_version);
                    return Ok(());
                } else {
                    println!("==> Upgrading cask '{}' from {} to {}",
                             name, installed_version, current_version);
                }
            }
        }

        install_cask_dependencies(&cask, cache).await?;

        let download_path = build::cask::download_cask(&cask, cache).await.map_err(|e| {
            SapphireError::Generic(format!("Failed to download cask '{}': {}", name, e))
        })?;

        build::cask::install_cask(&cask, &download_path)?;

        println!("==> Successfully installed cask {}", cask.display_name());
        Ok(())
    })
}

// Use boxing for async recursion (implicitly via install_cask and install_as_formula)
fn install_cask_dependencies<'a>(
    cask: &'a Cask,
    cache: &'a Cache
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        if let Some(deps) = &cask.depends_on {
            if let Some(formula_deps) = &deps.formula {
                if !formula_deps.is_empty() {
                    println!("==> Installing formula dependencies for cask {}: {:?}", cask.token, formula_deps);
                    let config = Config::load()?;
                    for dep in formula_deps {
                        install_as_formula(dep, &config, cache, true).await?;
                    }
                }
            }

            if let Some(cask_deps) = &deps.cask {
                if !cask_deps.is_empty() {
                    println!("==> Installing cask dependencies for cask {}: {:?}", cask.token, cask_deps);
                    for dep in cask_deps {
                        install_cask(dep, cache).await?;
                    }
                }
            }
        }

        Ok(())
    })
}
