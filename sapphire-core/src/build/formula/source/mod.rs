// sapphire-core/src/build/formula/source/mod.rs
// *** Restored async for download_source and build_from_source, uses try_join_all for resources ***

use crate::build;
use crate::build::env::BuildEnvironment;
use crate::build::extract_archive_strip_components; // Ensure correct import path
use crate::fetch::http as http_fetch;
use crate::model::formula::{Formula, FormulaDependencies, ResourceSpec};
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use futures::future::try_join_all;
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{self};
use std::path::{Path, PathBuf};
use std::process::Command; // For concurrent resource downloads

// Build system modules (Unchanged)
mod cargo;
mod cmake;
mod go;
mod make;
mod meson;
mod perl;
mod python;

// Re-export build functions (Unchanged)
pub use cargo::cargo_build;
pub use cmake::cmake_build;
pub use go::go_build;
pub use make::{configure_and_make, simple_make};
pub use meson::meson_build;
pub use perl::perl_build;
pub use python::python_build;

/// Download main source code for the formula asynchronously.
pub async fn download_source(formula: &Formula, config: &Config) -> Result<PathBuf> {
    // Is async
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
    // Await the async fetch function
    http_fetch::fetch_formula_source_or_bottle(
        &formula.name,
        &url,
        &formula.sha256,
        &formula.mirrors,
        config,
    )
    .await // Await here
}

/// Build and install formula from downloaded source archive or file asynchronously.
pub async fn build_from_source(
    // Is async
    source_path: &Path,
    formula: &Formula,
    config: &Config,
    all_installed_paths: &[PathBuf],
) -> Result<PathBuf> {
    let install_dir = formula.install_prefix(&config.cellar)?;
    let formula_name = formula.name();

    // Single file installation (synchronous)
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
        install_single_file(source_path, formula, &install_dir)?; // sync call
        crate::build::write_receipt(formula, &install_dir)?; // sync call
        return Ok(install_dir);
    }

    // --- Archive Installation ---
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
    extract_archive_strip_components(source_path, build_dir, 1)?; // sync call

    // --- Resource Handling ---
    let resources = formula.resources()?;
    let mut resource_stage_paths = HashMap::new();

    if !resources.is_empty() {
        info!(
            "==> Handling {} resources for {}",
            resources.len(),
            formula_name
        );
        let resource_staging_base = build_dir.join(".sapphire-resources");
        fs::create_dir_all(&resource_staging_base)?; // sync call

        // Create futures for all resource downloads
        let download_futures = resources.iter().map(|resource| {
            let formula_name_clone = formula_name.to_string(); // Clone for async block
            let config_clone = config.clone(); // Clone config
            async move {
                info!(" --> Downloading resource: {}", resource.name);
                let path = http_fetch::fetch_resource(&formula_name_clone, resource, &config_clone)
                    .await?; // await async fetch
                Ok::<_, SapphireError>((resource.name.clone(), path)) // Return tuple
            }
        });

        // Execute downloads concurrently and await results
        let download_results = try_join_all(download_futures).await?; // await concurrent downloads

        // Stage resources after successful download (synchronous part)
        for (res_name, resource_archive_path) in download_results {
            let stage_path = resource_staging_base.join(&res_name);
            fs::create_dir_all(&stage_path)?;
            info!(
                " --> Staging resource '{}' to {}",
                res_name,
                stage_path.display()
            );
            crate::build::extract_archive(&resource_archive_path, &stage_path)?; // sync extraction
            resource_stage_paths.insert(res_name, stage_path);
        }
    }
    // --- End Resource Handling ---

    info!(
        "==> Building {} from source in {}",
        formula_name,
        build_dir.display()
    );

    // Build Environment Setup (synchronous)
    info!("==> Setting up build environment");
    let sapphire_prefix = build::get_homebrew_prefix();
    let build_env = BuildEnvironment::new(
        formula,
        &sapphire_prefix,
        &config.cellar,
        all_installed_paths,
    )?; // sync call

    // --- Build Process (synchronous) ---
    let original_cwd = std::env::current_dir().map_err(SapphireError::Io)?;
    info!(
        "Changing working directory to build dir: {}",
        build_dir.display()
    );
    std::env::set_current_dir(build_dir).map_err(SapphireError::Io)?;
    let _cwd_guard = CurrentWorkingDirectoryGuard::new(original_cwd.clone());

    // --- Install Resources First (synchronous) ---
    if !resources.is_empty() {
        info!("==> Installing {} resources into libexec", resources.len());
        let libexec_path = install_dir.join("libexec");
        fs::create_dir_all(&libexec_path)?; // sync call

        for resource in &resources {
            if let Some(stage_path) = resource_stage_paths.get(&resource.name) {
                info!(" --> Installing resource: {}", resource.name);
                install_resource(resource, stage_path, &libexec_path, &build_env)?;
            // sync call
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
        std::env::set_current_dir(build_dir).map_err(SapphireError::Io)?; // sync call
    }
    // --- End Resource Installation ---

    // --- Build Main Formula (synchronous) ---
    info!("==> Building main formula: {}", formula_name);
    detect_and_build(
        formula,
        build_dir,
        &install_dir,
        &build_env,
        all_installed_paths,
    )?; // sync call

    // --- Post-Install ---
    std::env::set_current_dir(original_cwd).map_err(SapphireError::Io)?; // sync call
    crate::build::write_receipt(formula, &install_dir)?; // sync call
    info!(
        "Build completed, temporary directory {} will be cleaned up.",
        build_dir.display()
    );
    Ok(install_dir)
}

// install_resource and helpers (install_perl_resource, install_python_resource) remain synchronous
fn install_resource(
    resource: &ResourceSpec,
    stage_path: &Path,
    libexec_path: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    /* Sync implementation from previous step */
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
            "   -> Could not detect build system for resource '{}'. Skipping install.",
            resource.name
        );
    }
    Ok(())
}
fn install_perl_resource(
    resource: &ResourceSpec,
    libexec_path: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    /* Sync implementation from previous step */
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
    let new_perl5lib = match cmd_env.get("PERL5LIB") {
        Some(existing) => format!("{}:{}", perl5lib_path, existing),
        None => perl5lib_path.clone(),
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
    let mut make_cmd = Command::new(&make_exe);
    build_env.apply_to_command(&mut make_cmd);
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
    let mut install_cmd = Command::new(&make_exe);
    install_cmd.arg("install");
    build_env.apply_to_command(&mut install_cmd);
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
    /* Sync implementation from previous step */
    let python_exe = which::which_in("python3", build_env.get_path_string(), Path::new("."))
        .or_else(|_| which::which_in("python", build_env.get_path_string(), Path::new(".")))
        .map_err(|_| {
            SapphireError::BuildEnvError(
                "python not found in build env PATH for resource install".to_string(),
            )
        })?;
    let python_lib_target = libexec_path.join("vendor");
    fs::create_dir_all(&python_lib_target)?;
    let mut install_cmd = Command::new(python_exe);
    install_cmd
        .arg("setup.py")
        .arg("install")
        .arg(format!("--prefix={}", python_lib_target.display()));
    let mut cmd_env = build_env.get_vars().clone();
    let pythonpath_entry = python_lib_target.to_string_lossy().to_string();
    let new_pythonpath = match cmd_env.get("PYTHONPATH") {
        Some(existing) => format!("{}:{}", pythonpath_entry, existing),
        None => pythonpath_entry.clone(),
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
        "   -> Successfully installed Python resource '{}'",
        resource.name
    );
    Ok(())
}

// install_single_file remains synchronous
fn install_single_file(source_path: &Path, formula: &Formula, install_dir: &Path) -> Result<()> {
    /* Sync implementation from previous step */
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

// detect_and_build remains synchronous
fn detect_and_build(
    formula: &Formula,
    build_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
    all_installed_paths: &[PathBuf],
) -> Result<()> {
    /* Sync implementation from previous step */
    if !build_dir.exists() {
        return Err(SapphireError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Build directory does not exist: {}", build_dir.display()),
        )));
    }
    fs::read_dir(build_dir).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "Cannot access build directory {}: {}",
                build_dir.display(),
                e
            ),
        ))
    })?;
    if formula.name == "perl" && Path::new("Configure").exists() {
        info!("Detected Perl build system (Configure script)");
        return perl::perl_build(build_dir, install_dir, build_env);
    }
    if Path::new("configure.ac").exists() || Path::new("configure.in").exists() {
        match which::which_in("autoreconf", build_env.get_path_string(), build_dir) {
            Ok(autoreconf_path) => {
                if !Path::new("configure").exists() {
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
                        return Err(SapphireError::Generic(format!(
                            "autoreconf failed with status: {}",
                            output.status
                        )));
                    } else {
                        debug!(
                            "Autoreconf stdout:\n{}",
                            String::from_utf8_lossy(&output.stdout)
                        );
                        debug!(
                            "Autoreconf stderr:\n{}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                } else {
                    info!("Skipping autoreconf, configure script already exists.");
                }
            }
            Err(_) => {
                warn!("configure.ac found but autoreconf not found in PATH. Will proceed only if 'configure' script exists.");
            }
        }
    }
    if Path::new("configure").exists() {
        info!("Detected Autotools build system (configure script)");
        return make::configure_and_make(install_dir, build_env);
    }
    if Path::new("CMakeLists.txt").exists() {
        info!("Detected CMake build system");
        return cmake::cmake_build(build_dir, install_dir, build_env);
    }
    if Path::new("meson.build").exists() {
        info!("Detected Meson build system");
        return meson::meson_build(build_dir, install_dir, build_env);
    }
    let go_src_dir = Path::new("src");
    if go_src_dir.is_dir()
        && (go_src_dir.join("make.bash").exists() || go_src_dir.join("all.bash").exists())
    {
        info!("Detected Go build system (make.bash or all.bash)");
        return go::go_build(build_dir, install_dir, build_env, all_installed_paths);
    }
    if Path::new("Cargo.toml").exists() {
        info!("Detected Rust/Cargo build system");
        return cargo::cargo_build(install_dir, build_env);
    }
    if Path::new("setup.py").exists() {
        info!("Detected Python build system (setup.py)");
        return python::python_build(install_dir, build_env);
    }
    if Path::new("Makefile").exists() || Path::new("makefile").exists() {
        info!("Detected Makefile build system (no configure script)");
        return make::simple_make(install_dir, build_env);
    }
    error!(
        "Could not determine build system for {}",
        build_dir.display()
    );
    Err(SapphireError::Generic(format!(
        "Could not determine build system for {}",
        build_dir.display()
    )))
}

// CurrentWorkingDirectoryGuard remains unchanged
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
