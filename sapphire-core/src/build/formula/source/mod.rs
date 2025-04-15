// sapphire-core/src/build/formula/source/mod.rs

// --- Imports ---
use crate::build;
use crate::build::env::BuildEnvironment;
use crate::fetch::http as http_fetch;
use crate::model::formula::{Formula, FormulaDependencies, ResourceSpec};
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use futures::future::try_join_all;
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self};
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

// --- Build system submodules ---
mod cargo;
mod cmake;
mod go;
mod make;
mod meson;
mod perl;
mod python;

// --- Re-export build functions ---
pub use cargo::cargo_build;
pub use cmake::cmake_build;
pub use go::go_build;
pub use make::{configure_and_make, simple_make};
pub use meson::meson_build;
pub use perl::perl_build;
pub use python::python_build;

// --- download_source (unchanged) ---
pub async fn download_source(formula: &Formula, config: &Config) -> Result<PathBuf> {
    // (Implementation remains the same)
    let url = if !formula.url.is_empty() {
        formula.url.clone()
    } else if let Some(homepage) = &formula.homepage {
        if homepage.contains("github.com") {
            format!(
                "{}/archive/refs/tags/v{}.tar.gz",
                homepage.trim_end_matches('/'),
                formula.stable_version_str
            )
        } else {
            return Err(SapphireError::Generic(format!(
                "No source URL available for {} and cannot derive from homepage: {}",
                formula.name, homepage
            )));
        }
    } else {
        return Err(SapphireError::Generic(format!(
            "No source URL or homepage available for {}",
            formula.name
        )));
    };

    info!("==> Downloading main source for {}", formula.name);
    http_fetch::fetch_formula_source_or_bottle(
        &formula.name,
        &url,
        &formula.sha256,
        &formula.mirrors,
        config,
    )
    .await
}

// --- build_from_source (unchanged) ---
pub async fn build_from_source(
    source_path: &Path,
    formula: &Formula,
    config: &Config,
    all_installed_paths: &[PathBuf],
) -> Result<PathBuf> {
    // (Implementation remains the same)
    let install_dir = formula.install_prefix(&config.cellar)?;
    let formula_name = formula.name();

    // Single file installation
    let recognised_archive_extensions =
        ["tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz", "zip"];
    let source_extension = source_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if !recognised_archive_extensions.contains(&source_extension) {
        info!("==> Installing single file formula: {}", formula_name);
        fs::create_dir_all(&install_dir).map_err(|e| {
            SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed create install dir {}: {}", install_dir.display(), e),
            ))
        })?;
        install_single_file(source_path, formula, &install_dir)?;
        crate::build::write_receipt(formula, &install_dir)?;
        return Ok(install_dir);
    }

    // Archive Installation
    let temp_dir_base = config.cache_dir.join("build-temp");
    fs::create_dir_all(&temp_dir_base).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "Failed create build temp base {}: {}",
                temp_dir_base.display(),
                e
            ),
        ))
    })?;
    let temp_build_dir = tempfile::Builder::new()
        .prefix(&format!("{}-", formula_name))
        .tempdir_in(&temp_dir_base)
        .map_err(|e| {
            SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed create temp build dir in {}: {}",
                    temp_dir_base.display(),
                    e
                ),
            ))
        })?;
    let build_dir = temp_build_dir.path();

    info!(
        "==> Extracting main source {} to {}",
        source_path.display(),
        build_dir.display()
    );
    crate::build::extract_archive_strip_components(source_path, build_dir, 1)?;

    // Resource Handling
    let resources = formula.resources()?;
    let mut resource_stage_paths = HashMap::new();

    if !resources.is_empty() {
        info!(
            "==> Handling {} resources for {}",
            resources.len(),
            formula_name
        );
        let resource_staging_base = build_dir.join(".sapphire-resources");
        fs::create_dir_all(&resource_staging_base)?;

        let download_futures = resources.iter().map(|resource| {
            let formula_name_clone = formula_name.to_string();
            let config_clone = config.clone();
            async move {
                info!(" --> Downloading resource: {}", resource.name);
                let path = http_fetch::fetch_resource(&formula_name_clone, resource, &config_clone)
                    .await?;
                Ok::<_, SapphireError>((resource.name.clone(), path))
            }
        });

        let download_results = try_join_all(download_futures).await?;

        for (res_name, resource_archive_path) in download_results {
            let stage_path = resource_staging_base.join(&res_name);
            fs::create_dir_all(&stage_path)?;
            info!(
                " --> Staging resource '{}' to {}",
                res_name,
                stage_path.display()
            );
            crate::build::extract_archive(&resource_archive_path, &stage_path)?;
            resource_stage_paths.insert(res_name, stage_path);
        }
    }

    info!(
        "==> Building {} from source in {}",
        formula_name,
        build_dir.display()
    );

    // Build Environment Setup
    info!("==> Setting up build environment");
    let sapphire_prefix = build::get_homebrew_prefix();
    let build_env = BuildEnvironment::new(
        formula,
        &sapphire_prefix,
        &config.cellar,
        all_installed_paths,
    )?;

    // Build Process
    let original_cwd = std::env::current_dir().map_err(SapphireError::Io)?;
    info!(
        "Changing working directory to build dir: {}",
        build_dir.display()
    );
    std::env::set_current_dir(build_dir).map_err(SapphireError::Io)?;
    let _cwd_guard = CurrentWorkingDirectoryGuard::new(original_cwd.clone());

    // Install Resources First
    if !resources.is_empty() {
        info!("==> Installing {} resources into libexec", resources.len());
        let libexec_path = install_dir.join("libexec");
        fs::create_dir_all(&libexec_path)?;

        for resource in &resources {
            if let Some(stage_path) = resource_stage_paths.get(&resource.name) {
                info!(" --> Installing resource: {}", resource.name);
                install_resource(resource, stage_path, &libexec_path, &build_env)?;
            } else {
                warn!(
                    "Could not find stage path for resource '{}'. Skipping installation.",
                    resource.name
                );
            }
        }
        info!(
            "Restoring working directory to build dir: {}",
            build_dir.display()
        );
        std::env::set_current_dir(build_dir).map_err(SapphireError::Io)?;
    }

    // Build Main Formula
    info!("==> Building main formula: {}", formula_name);
    detect_and_build(
        formula,
        build_dir,
        &install_dir,
        &build_env,
        all_installed_paths,
    )?;

    // Post-Install
    std::env::set_current_dir(original_cwd).map_err(SapphireError::Io)?;
    crate::build::write_receipt(formula, &install_dir)?;
    info!(
        "Build completed, temporary directory {} will be cleaned up.",
        build_dir.display()
    );
    Ok(install_dir)
}

// --- Updated detect_and_build function ---
fn detect_and_build(
    formula: &Formula,
    build_dir: &Path, // Root of extracted source (e.g., .../llvm@19-Vrwnv2/)
    install_dir: &Path,
    build_env: &BuildEnvironment,
    all_installed_paths: &[PathBuf],
) -> Result<()> {
    info!(
        "Attempting to detect build system in: {}",
        build_dir.display()
    );

    let markers: &[(&str, &str, bool)] = &[
        ("configure", "Autotools (configure script)", true),
        ("CMakeLists.txt", "CMake", false),
        ("meson.build", "Meson", false),
        ("Makefile.PL", "Perl (Makefile.PL)", true),
        ("Configure", "Perl (Configure)", true),
        ("Cargo.toml", "Rust/Cargo", true),
        ("setup.py", "Python setup.py", true),
        ("Makefile", "Makefile", true),
        ("makefile", "Makefile", true),
    ];

    // --- Special case checks first ---
    // Handle autoreconf possibility
    if build_dir.join("configure.ac").exists() || build_dir.join("configure.in").exists() {
        if !build_dir.join("configure").exists() {
            match which::which_in("autoreconf", build_env.get_path_string(), build_dir) {
                Ok(autoreconf_path) => {
                    info!("==> Running autoreconf -fvi (as configure script is missing)");
                    let mut cmd = Command::new(autoreconf_path);
                    cmd.args(["-fvi"]);
                    build_env.apply_to_command(&mut cmd);
                    let output = cmd.output().map_err(|e| {
                        SapphireError::CommandExecError(format!(
                            "Failed to execute autoreconf: {}",
                            e
                        ))
                    })?;
                    if !output.status.success() {
                        error!("autoreconf failed with status: {}", output.status);
                        eprintln!(
                            "autoreconf stdout:\n{}",
                            String::from_utf8_lossy(&output.stdout)
                        );
                        eprintln!(
                            "autoreconf stderr:\n{}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                        warn!("Autoreconf failed, continuing detection...");
                    }
                }
                Err(_) => {
                    warn!("configure.ac found but autoreconf not found in PATH.");
                }
            }
        }
    }

    // Handle Go structure
    let go_src_dir = build_dir.join("src");
    if go_src_dir.is_dir()
        && (go_src_dir.join("make.bash").exists() || go_src_dir.join("all.bash").exists())
    {
        info!("Detected Go build system (make.bash or all.bash)");
        return go::go_build(build_dir, install_dir, build_env, all_installed_paths);
    }

    // --- Search for markers ---
    // Tuple: (marker_name, marker_containing_dir_path, depth, preference_score)
    let mut best_match: Option<(String, PathBuf, usize, i32)> = None;

    // Check root (depth 0)
    for (marker, _, _) in markers {
        if build_dir.join(marker).exists() {
            let score = 3;
            debug!("Found root marker '{}', score {}", marker, score);
            let current_priority = markers
                .iter()
                .position(|(m, _, _)| m == marker)
                .unwrap_or(usize::MAX);
            let existing_priority = match &best_match {
                Some((em, _, _, s)) if *s == score => markers
                    .iter()
                    .position(|(m, _, _)| m == em)
                    .unwrap_or(usize::MAX),
                _ => usize::MAX,
            };

            if score >= best_match.as_ref().map_or(0, |(_, _, _, s)| *s)
                && current_priority < existing_priority
            {
                debug!(
                    "Updating best match (root): '{}' in {} (depth 0, score {})",
                    marker,
                    build_dir.display(),
                    score
                );
                best_match = Some((marker.to_string(), build_dir.to_path_buf(), 0, score));
            } else if best_match.is_none() {
                debug!(
                    "Setting initial best match (root): '{}' in {} (depth 0, score {})",
                    marker,
                    build_dir.display(),
                    score
                );
                best_match = Some((marker.to_string(), build_dir.to_path_buf(), 0, score));
            }
        }
    }

    // Check depth 1 and 2 if no root match found or only low-preference root found
    if best_match.is_none() || best_match.as_ref().map_or(0, |(_, _, _, s)| *s) < 2 {
        info!("Checking subdirectories (depth 1 & 2) for preferred build system markers (CMake/Meson)...");
        let base_formula_name = formula
            .name()
            .split('@')
            .next()
            .unwrap_or_else(|| formula.name());
        let preferred_subdirs: Vec<OsString> = vec![
            OsString::from("src"),
            OsString::from("source"),
            OsString::from(base_formula_name),
        ];
        debug!(
            "Preferred subdirectories for nested check: {:?}",
            preferred_subdirs
        );

        for entry_result in WalkDir::new(build_dir)
            .min_depth(1)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let current_depth = entry_result.depth();
            let path = entry_result.path(); // Path to the entry (file or dir)
            let file_name = entry_result.file_name();

            // We only prioritize CMakeLists.txt or meson.build when nested
            if file_name == "CMakeLists.txt" || file_name == "meson.build" {
                let marker = file_name.to_str().unwrap_or_default();
                let parent_dir = path.parent().expect("Nested file must have a parent"); // Directory containing the marker
                let parent_dir_name = parent_dir.file_name().map(|f| f.to_os_string());

                let mut score;
                if let Some(p_name) = parent_dir_name {
                    if preferred_subdirs.contains(&p_name) {
                        score = 2; // Preferred subdirectory
                        debug!(
                            "Found preferred nested marker '{}' in {} (depth {}, score {})",
                            marker,
                            parent_dir.display(),
                            current_depth,
                            score
                        );
                    } else {
                        score = 1; // Other subdirectory
                        debug!(
                            "Found other nested marker '{}' in {} (depth {}, score {})",
                            marker,
                            parent_dir.display(),
                            current_depth,
                            score
                        );
                    }
                } else {
                    score = 1; // Fallback score if parent name unavailable
                    debug!(
                        "Found other nested marker '{}' at depth {} (no parent name?), score {}",
                        marker, current_depth, score
                    );
                }

                // Adjust score based on depth (prefer shallower)
                score -= current_depth as i32 - 1;

                let is_better = match best_match {
                    None => true,
                    Some((_, _, _, existing_score)) => score > existing_score,
                };

                if is_better {
                    debug!(
                        "Updating best match: '{}' in {} (depth {}, score {})",
                        marker,
                        parent_dir.display(),
                        current_depth,
                        score
                    );
                    // *** Store the directory containing the marker ***
                    best_match = Some((
                        marker.to_string(),
                        parent_dir.to_path_buf(),
                        current_depth,
                        score,
                    ));
                }
            }
        }
    }

    // --- Dispatch based on the best match found ---
    if let Some((marker_name, marker_dir, depth, score)) = best_match {
        let system_name = markers
            .iter()
            .find(|(m, _, _)| m == &marker_name)
            .map(|(_, sn, _)| *sn)
            .unwrap_or("Unknown");

        info!(
            "Detected build system '{}' (marker: {}, score: {}) in {} (depth: {})",
            system_name,
            marker_name,
            score,
            marker_dir.display(), // Log the directory *containing* the marker
            depth
        );

        // *** CRUCIAL CHANGE: Determine the source path to pass based on depth ***
        let source_path_for_build = if depth == 0 {
            // If marker found at root, the build system should use the root build_dir
            build_dir
        } else if marker_name == "CMakeLists.txt" || marker_name == "meson.build" {
            // If CMake/Meson marker found nested, the build system needs to be
            // pointed at the directory *containing* the marker.
            marker_dir.as_path()
        } else {
            // For other nested markers (currently shouldn't happen with root_required),
            // default to build_dir, but this might need refinement.
            warn!(
                "Unexpected nested marker '{}' found, using root build directory.",
                marker_name
            );
            build_dir
        };

        info!(
            "Using source path for build dispatch: {}",
            source_path_for_build.display()
        );

        return dispatch_build(
            &marker_name,
            source_path_for_build, // Pass the determined source path
            install_dir,
            build_env,
            all_installed_paths,
        );
    }

    // --- If no build system detected ---
    error!(
        "Could not determine build system for {}",
        build_dir.display()
    );
    Err(SapphireError::Generic(format!(
        "Could not determine build system for {}",
        build_dir.display()
    )))
}

// --- Updated dispatch_build function ---
fn dispatch_build(
    marker_filename: &str,
    source_dir_for_build: &Path, // <-- Renamed for clarity, this is the path build func needs
    install_dir: &Path,
    build_env: &BuildEnvironment,
    _all_installed_paths: &[PathBuf],
) -> Result<()> {
    // Note: The current working directory for all these calls is still the root `build_dir`
    // from where `detect_and_build` was called. The build functions need to handle
    // the `source_dir_for_build` argument correctly relative to their execution context.
    match marker_filename {
        "configure" => {
            // Assumes configure was found at root
            info!("Dispatching to Autotools (configure script at root)");
            make::configure_and_make(install_dir, build_env) // Runs ./configure in current CWD
        }
        "CMakeLists.txt" => {
            info!("Dispatching to CMake");
            // Pass the potentially nested source dir containing CMakeLists.txt
            cmake::cmake_build(source_dir_for_build, install_dir, build_env)
        }
        "meson.build" => {
            info!("Dispatching to Meson");
            // Pass the potentially nested source dir containing meson.build
            meson::meson_build(source_dir_for_build, install_dir, build_env)
        }
        "Makefile.PL" | "Configure" => {
            // Assumes found at root
            info!("Dispatching to Perl build (at root)");
            perl::perl_build(source_dir_for_build, install_dir, build_env) // Pass root source dir
        }
        "Cargo.toml" => {
            // Assumes found at root
            info!("Dispatching to Rust/Cargo (at root)");
            cargo::cargo_build(install_dir, build_env) // Runs from CWD
        }
        "setup.py" => {
            // Assumes found at root
            info!("Dispatching to Python setup.py (at root)");
            python::python_build(install_dir, build_env) // Runs from CWD
        }
        "Makefile" | "makefile" => {
            // Assumes found at root
            info!("Dispatching to simple Makefile (at root)");
            make::simple_make(install_dir, build_env) // Runs from CWD
        }
        _ => {
            error!(
                "Internal error: Unknown marker file dispatched: {}",
                marker_filename
            );
            Err(SapphireError::Generic(format!(
                "Internal error: Unknown build system marker '{}'",
                marker_filename
            )))
        }
    }
}

// --- Resource installation helpers (unchanged) ---
fn install_resource(
    resource: &ResourceSpec,
    stage_path: &Path,
    libexec_path: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    // (Implementation remains the same)
    let original_cwd = std::env::current_dir().map_err(SapphireError::Io)?;
    debug!(
        "Changing CWD for resource '{}' install: {}",
        resource.name,
        stage_path.display()
    );
    std::env::set_current_dir(stage_path).map_err(SapphireError::Io)?;
    let _cwd_guard = CurrentWorkingDirectoryGuard::new(original_cwd);

    if stage_path.join("Makefile.PL").exists() {
        info!("   -> Detected Perl resource '{}'", resource.name);
        install_perl_resource(resource, libexec_path, build_env)?;
    } else if stage_path.join("setup.py").exists() {
        info!("   -> Detected Python resource '{}'", resource.name);
        install_python_resource(resource, libexec_path, build_env)?;
    } else {
        warn!(
            "   -> Could not detect build system for resource '{}' in {}. Skipping install.",
            resource.name,
            stage_path.display()
        );
    }
    Ok(())
}
fn install_perl_resource(
    resource: &ResourceSpec,
    libexec_path: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    // (Implementation remains the same)
    let perl_exe =
        which::which_in("perl", build_env.get_path_string(), Path::new(".")).map_err(|_| {
            SapphireError::BuildEnvError(
                "perl not found in build env PATH for resource install".to_string(),
            )
        })?;
    let make_exe =
        which::which_in("make", build_env.get_path_string(), Path::new(".")).map_err(|_| {
            SapphireError::BuildEnvError(
                "make not found in build env PATH for resource install".to_string(),
            )
        })?;
    let perl_lib_target = libexec_path.join("lib").join("perl5");
    fs::create_dir_all(&perl_lib_target)?;
    let mut configure_cmd = Command::new(&perl_exe);
    configure_cmd
        .arg("Makefile.PL")
        .arg(format!("INSTALL_BASE={}", libexec_path.display()));
    let mut cmd_env = build_env.get_vars().clone();
    let perl5lib_path = perl_lib_target.to_string_lossy().to_string();
    let current_perl5lib = std::env::join_paths(
        std::env::var("PERL5LIB")
            .unwrap_or_default()
            .split(':')
            .map(PathBuf::from),
    )
    .ok()
    .and_then(|p| p.into_string().ok())
    .unwrap_or_default();

    let new_perl5lib = if current_perl5lib.is_empty() {
        perl5lib_path
    } else {
        format!("{}:{}", perl5lib_path, current_perl5lib)
    };
    cmd_env.insert("PERL5LIB".to_string(), new_perl5lib);
    configure_cmd.env_clear().envs(&cmd_env);
    debug!(
        "Running Perl configure for resource '{}': {:?}",
        resource.name, configure_cmd
    );
    let output_config = configure_cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!(
            "Failed to execute perl Makefile.PL for resource {}: {}",
            resource.name, e
        ))
    })?;
    if !output_config.status.success() {
        error!(
            "Perl Makefile.PL failed for resource '{}'. Status: {}",
            resource.name, output_config.status
        );
        error!(
            "Stdout:\n{}",
            String::from_utf8_lossy(&output_config.stdout)
        );
        error!(
            "Stderr:\n{}",
            String::from_utf8_lossy(&output_config.stderr)
        );
        return Err(SapphireError::InstallError(format!(
            "Perl Makefile.PL failed for resource {}",
            resource.name
        )));
    } else {
        debug!(
            "Stdout:\n{}",
            String::from_utf8_lossy(&output_config.stdout)
        );
        debug!(
            "Stderr:\n{}",
            String::from_utf8_lossy(&output_config.stderr)
        );
    }
    let mut make_cmd = Command::new(make_exe.clone());
    build_env.apply_to_command(&mut make_cmd);
    make_cmd.env("PERL5LIB", cmd_env.get("PERL5LIB").unwrap());
    debug!(
        "Running make for resource '{}': {:?}",
        resource.name, make_cmd
    );
    let output_make = make_cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!(
            "Failed to execute make for resource {}: {}",
            resource.name, e
        ))
    })?;
    if !output_make.status.success() {
        error!(
            "make failed for resource '{}'. Status: {}",
            resource.name, output_make.status
        );
        error!("Stdout:\n{}", String::from_utf8_lossy(&output_make.stdout));
        error!("Stderr:\n{}", String::from_utf8_lossy(&output_make.stderr));
        return Err(SapphireError::InstallError(format!(
            "make failed for resource {}",
            resource.name
        )));
    }
    let mut install_cmd = Command::new(make_exe);
    install_cmd.arg("install");
    build_env.apply_to_command(&mut install_cmd);
    install_cmd.env("PERL5LIB", cmd_env.get("PERL5LIB").unwrap());
    debug!(
        "Running make install for resource '{}': {:?}",
        resource.name, install_cmd
    );
    let output_install = install_cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!(
            "Failed to execute make install for resource {}: {}",
            resource.name, e
        ))
    })?;
    if !output_install.status.success() {
        error!(
            "make install failed for resource '{}'. Status: {}",
            resource.name, output_install.status
        );
        error!(
            "Stdout:\n{}",
            String::from_utf8_lossy(&output_install.stdout)
        );
        error!(
            "Stderr:\n{}",
            String::from_utf8_lossy(&output_install.stderr)
        );
        return Err(SapphireError::InstallError(format!(
            "make install failed for resource {}",
            resource.name
        )));
    }
    info!(
        "   -> Successfully installed Perl resource '{}'",
        resource.name
    );
    Ok(())
}
fn install_python_resource(
    resource: &ResourceSpec,
    libexec_path: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    // (Implementation remains the same)
    let python_exe = which::which_in("python3", build_env.get_path_string(), Path::new("."))
        .or_else(|_| which::which_in("python", build_env.get_path_string(), Path::new(".")))
        .map_err(|_| {
            SapphireError::BuildEnvError(
                "python not found in build env PATH for resource install".to_string(),
            )
        })?;
    let python_version_output = Command::new(&python_exe)
        .arg("-c")
        .arg("import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")
        .output()
        .map_err(|e| {
            SapphireError::BuildEnvError(format!("Failed to get python version: {}", e))
        })?;
    let python_version_str = String::from_utf8_lossy(&python_version_output.stdout)
        .trim()
        .to_string();
    if python_version_str.is_empty() {
        return Err(SapphireError::BuildEnvError(
            "Could not determine python minor version for resource installation path.".to_string(),
        ));
    }
    let python_site_packages = libexec_path
        .join("vendor")
        .join("lib")
        .join(format!("python{}", python_version_str))
        .join("site-packages");

    fs::create_dir_all(&python_site_packages)?;
    let mut install_cmd = Command::new(python_exe);
    install_cmd.arg("setup.py").arg("install").arg(format!(
        "--prefix={}",
        libexec_path.join("vendor").display()
    ));
    let mut cmd_env = build_env.get_vars().clone();
    let pythonpath_entry = python_site_packages.to_string_lossy().to_string();
    let current_pythonpath = std::env::var("PYTHONPATH").unwrap_or_default();
    let new_pythonpath = if current_pythonpath.is_empty() {
        pythonpath_entry
    } else {
        format!("{}:{}", pythonpath_entry, current_pythonpath)
    };
    cmd_env.insert("PYTHONPATH".to_string(), new_pythonpath);

    install_cmd.env_clear().envs(&cmd_env);

    debug!(
        "Running Python install for resource '{}': {:?}",
        resource.name, install_cmd
    );
    let output_install = install_cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!(
            "Failed to execute python setup.py install for resource {}: {}",
            resource.name, e
        ))
    })?;
    if !output_install.status.success() {
        error!(
            "Python setup.py install failed for resource '{}'. Status: {}",
            resource.name, output_install.status
        );
        error!(
            "Stdout:\n{}",
            String::from_utf8_lossy(&output_install.stdout)
        );
        error!(
            "Stderr:\n{}",
            String::from_utf8_lossy(&output_install.stderr)
        );
        return Err(SapphireError::InstallError(format!(
            "Python setup.py install failed for resource {}",
            resource.name
        )));
    }
    info!(
        "   -> Successfully installed Python resource '{}' to {}",
        resource.name,
        python_site_packages.display()
    );
    Ok(())
}

// --- Single file install (unchanged) ---
fn install_single_file(source_path: &Path, formula: &Formula, install_dir: &Path) -> Result<()> {
    // (Implementation remains the same)
    let target_path = if formula.name == "ca-certificates" {
        let share_dir = install_dir.join("share").join(formula.name());
        fs::create_dir_all(&share_dir).map_err(|e| {
            SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed create share dir {}: {}", share_dir.display(), e),
            ))
        })?;
        share_dir.join(
            source_path
                .file_name()
                .unwrap_or_else(|| OsStr::new("cacert.pem")),
        )
    } else {
        let target_share_dir = install_dir.join("share").join(formula.name());
        fs::create_dir_all(&target_share_dir).map_err(|e| {
            SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed create share dir {}: {}",
                    target_share_dir.display(),
                    e
                ),
            ))
        })?;
        let target_file_name = source_path
            .file_name()
            .ok_or_else(|| SapphireError::Generic("Source path has no filename.".to_string()))?;
        target_share_dir.join(target_file_name)
    };
    info!(
        "Copying {} to {}",
        source_path.display(),
        target_path.display()
    );
    fs::copy(source_path, &target_path).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "Failed copy {} to {}: {}",
                source_path.display(),
                target_path.display(),
                e
            ),
        ))
    })?;
    Ok(())
}

// --- CurrentWorkingDirectoryGuard (unchanged) ---
struct CurrentWorkingDirectoryGuard {
    original_cwd: PathBuf,
}
impl CurrentWorkingDirectoryGuard {
    fn new(original_cwd: PathBuf) -> Self {
        Self { original_cwd }
    }
}
impl Drop for CurrentWorkingDirectoryGuard {
    fn drop(&mut self) {
        if let Err(e) = std::env::set_current_dir(&self.original_cwd) {
            log::error!(
                "Failed to restore original working directory to {}: {}",
                self.original_cwd.display(),
                e
            );
        } else {
            log::debug!(
                "Restored working directory to: {}",
                self.original_cwd.display()
            );
        }
    }
}
