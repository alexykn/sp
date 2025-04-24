// sapphire-core/src/build/formula/source/mod.rs

use std::collections::HashMap;
use std::fs::{self};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use futures::future::try_join_all;
use infer;
use tracing::{debug, error};

use crate::build::env::BuildEnvironment;
use crate::build::extract;
use crate::fetch::http as http_fetch;
use crate::model::formula::{Formula, FormulaDependencies, ResourceSpec};
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};

mod cargo;
mod cmake;
mod go;
mod make;
mod meson;
mod perl;
mod python;

pub use cargo::cargo_build;
pub use cmake::cmake_build;
pub use go::go_build;
pub use make::{configure_and_make, simple_make};
pub use meson::meson_build;
pub use perl::perl_build;
pub use python::python_build;

const SUPPORTED_ARCHIVE_EXTENSIONS: [&str; 5] = ["gz", "bz2", "xz", "tar", "zip"];
const RECOGNISED_SINGLE_FILE_EXTENSIONS: [&str; 9] =
    ["tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz", "zip"];

pub async fn download_source(formula: &Formula, config: &Config) -> Result<PathBuf> {
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

    debug!("Downloading main source for {}", formula.name);
    http_fetch::fetch_formula_source_or_bottle(
        &formula.name,
        &url,
        &formula.sha256,
        &formula.mirrors,
        config,
    )
    .await
}

fn create_dir_all_with_context(path: &Path, context: &str) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to create {} {}: {}", context, path.display(), e),
        ))
    })
}

fn determine_source_root(build_dir: &Path) -> Result<PathBuf> {
    let mut subdirs = Vec::new();
    let mut has_files = false;
    match fs::read_dir(build_dir) {
        Ok(entries) => {
            // Corrected lines: Use .flatten() and remove the inner `if let`
            for entry in entries.flatten() {
                let path = entry.path();
                let file_name_string = entry.file_name().to_string_lossy().to_string();

                if file_name_string.starts_with('.') || file_name_string == ".sapphire-resources" {
                    continue;
                }

                if path.is_dir() {
                    subdirs.push(entry.file_name().into());
                } else if path.is_file() {
                    has_files = true;
                }
            }
        }
        Err(e) => {
            return Err(SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed read build dir {}: {}", build_dir.display(), e),
            )));
        }
    }

    if subdirs.len() == 1 && !has_files {
        debug!(
            "Source root appears to be single subdirectory: {:?}",
            subdirs[0]
        );
        Ok(subdirs.remove(0))
    } else {
        debug!(
            "Source root appears to be the main build directory: {}",
            build_dir.display()
        );
        Ok(PathBuf::from("."))
    }
}

fn check_and_run_autoreconf(source_dir: &Path, build_env: &BuildEnvironment) -> Result<()> {
    if (source_dir.join("configure.ac").exists() || source_dir.join("configure.in").exists())
        && !source_dir.join("configure").exists()
    {
        match which::which_in("autoreconf", build_env.get_path_string(), source_dir) {
            Ok(autoreconf_path) => {
                debug!("Running autoreconf -fvi in {}", source_dir.display());
                let mut cmd = Command::new(autoreconf_path);
                cmd.args(["-fvi"]);
                match run_command_in_dir(&mut cmd, source_dir, build_env, "autoreconf") {
                    Ok(_) => debug!("Autoreconf completed successfully."),
                    Err(e) => debug!("Autoreconf failed ({}). Continuing build detection...", e),
                }
            }
            Err(_) => {
                debug!("configure.ac/in found but configure script and autoreconf command are missing.");
            }
        }
    }
    Ok(())
}

fn detect_and_build(
    build_dir: &Path,
    source_subdir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
    all_installed_paths: &[PathBuf],
) -> Result<()> {
    let source_root_abs = build_dir.join(source_subdir);
    debug!(
        "Attempting to detect build system in {}",
        source_root_abs.display()
    );

    check_and_run_autoreconf(&source_root_abs, build_env)?;

    if source_root_abs.join("CMakeLists.txt").exists() {
        debug!("Detected build system: CMake");
        cmake::cmake_build(source_subdir, build_dir, install_dir, build_env)?;
    } else if source_root_abs.join("meson.build").exists() {
        debug!("Detected build system: Meson");
        meson::meson_build(source_subdir, build_dir, install_dir, build_env)?;
    } else if source_root_abs.join("configure").exists() {
        debug!("Detected build system: Autotools (configure script)");
        make::configure_and_make(&source_root_abs, install_dir, build_env)?;
    } else if source_root_abs.join("go.mod").exists() {
        debug!("Detected Go module (go.mod)");
        go::go_build(
            &source_root_abs,
            install_dir,
            build_env,
            all_installed_paths,
        )?;
    } else if source_root_abs.join("Makefile.PL").exists()
        || source_root_abs.join("Configure").exists()
    {
        debug!("Detected build system: Perl (Makefile.PL or Configure)");
        perl::perl_build(&source_root_abs, install_dir, build_env)?;
    } else if source_root_abs.join("Cargo.toml").exists() {
        debug!("Detected build system: Rust/Cargo");
        cargo::cargo_build(&source_root_abs, install_dir, build_env)?;
    } else if source_root_abs.join("setup.py").exists() {
        debug!("Detected build system: Python setup.py");
        python::python_build(&source_root_abs, install_dir, build_env)?;
    } else if source_root_abs.join("Makefile").exists() || source_root_abs.join("makefile").exists()
    {
        debug!("Detected build system: Simple Makefile");
        make::simple_make(&source_root_abs, install_dir, build_env)?;
    } else {
        error!(
            "Could not determine build system in {}",
            source_root_abs.display()
        );
        return Err(SapphireError::Generic(
            "Could not determine build system in source directory.".to_string(),
        ));
    }
    Ok(())
}

fn determine_archive_type(archive_path: &Path, _context: &str) -> Result<&'static str> {
    match infer::get_from_path(archive_path)? {
        Some(kind) => {
            let ext = kind.extension();
            if SUPPORTED_ARCHIVE_EXTENSIONS.contains(&ext) {
                SUPPORTED_ARCHIVE_EXTENSIONS
                    .iter()
                    .find(|&&s| s == ext)
                    .copied()
                    .ok_or_else(|| {
                        SapphireError::Generic(format!(
                            "Internal error matching inferred ext {ext}"
                        ))
                    })
            } else {
                Err(SapphireError::Generic(format!(
                    "Unsupported inferred archive type '{}' for {}",
                    ext,
                    archive_path.display()
                )))
            }
        }
        None => {
            let ext = archive_path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if SUPPORTED_ARCHIVE_EXTENSIONS.contains(&ext) {
                SUPPORTED_ARCHIVE_EXTENSIONS
                    .iter()
                    .find(|&&s| s == ext)
                    .copied()
                    .ok_or_else(|| {
                        SapphireError::Generic(format!("Internal error matching file ext {ext}"))
                    })
            } else {
                Err(SapphireError::Generic(format!(
                    "Unsupported file extension '{}' for {}",
                    ext,
                    archive_path.display()
                )))
            }
        }
    }
}

fn install_resource(
    resource: &ResourceSpec,
    stage_path: &Path,
    libexec_path: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    debug!(" --> Installing resource: {}", resource.name);

    if stage_path.join("Makefile.PL").exists() {
        debug!(
            "   -> Detected Perl resource '{}', installing...",
            resource.name
        );
        install_perl_resource(resource, stage_path, libexec_path, build_env)?;
    } else if stage_path.join("setup.py").exists() {
        debug!(
            "   -> Detected Python resource '{}', installing...",
            resource.name
        );
        install_python_resource(resource, stage_path, libexec_path, build_env)?;
    } else {
        debug!(
            "   -> Could not detect known build system (Perl/Python) for resource '{}' in {}. Skipping install.",
            resource.name,
            stage_path.display()
        );
    }
    Ok(())
}

pub async fn build_from_source(
    source_path: &Path,
    formula: &Formula,
    config: &Config,
    all_installed_paths: &[PathBuf],
) -> Result<PathBuf> {
    let install_dir = formula.install_prefix(&config.cellar)?;
    let formula_name = formula.name();

    let source_extension = source_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    if !RECOGNISED_SINGLE_FILE_EXTENSIONS.contains(&source_extension) {
        debug!("Installing single file formula: {}", formula_name);
        create_dir_all_with_context(&install_dir, "install directory")?;
        install_single_file(source_path, formula, &install_dir)?;
        crate::build::write_receipt(formula, &install_dir)?;
        return Ok(install_dir);
    }

    let source_archive_type_str = determine_archive_type(source_path, "main source archive")?;
    let inferred_root_dir = extract::infer_archive_root_dir(source_path, source_archive_type_str)?;
    let strip_components = if inferred_root_dir.is_some() { 1 } else { 0 };

    let temp_dir_base = config.cache_dir.join("build-temp");
    create_dir_all_with_context(&temp_dir_base, "build temp base")?;
    let temp_build_dir = tempfile::Builder::new()
        .prefix(&format!("{formula_name}-"))
        .tempdir_in(&temp_dir_base)
        .map_err(|e| SapphireError::IoError(format!("Failed create temp build dir: {e}")))?;
    let build_dir = temp_build_dir.path();

    debug!(
        "Extracting main source {} to {} (strip={})",
        source_path.display(),
        build_dir.display(),
        strip_components
    );
    crate::build::extract::extract_archive(
        source_path,
        build_dir,
        strip_components,
        source_archive_type_str,
    )?;
    debug!("Extracted main source to {}", build_dir.display());

    let resources = formula.resources()?;
    let mut resource_stage_paths = HashMap::new();

    if !resources.is_empty() {
        debug!(
            "Handling {} resources for {}",
            resources.len(),
            formula_name
        );
        let resource_staging_base = build_dir.join(".sapphire-resources");
        create_dir_all_with_context(&resource_staging_base, "resource staging base")?;

        let download_futures = resources.iter().map(|resource| {
            let formula_name_clone = formula_name.to_string();
            let config_clone = config.clone();
            async move {
                debug!(" --> Downloading resource: {}", resource.name);
                let path = http_fetch::fetch_resource(&formula_name_clone, resource, &config_clone)
                    .await?;
                Ok::<_, SapphireError>((resource.name.clone(), path))
            }
        });
        let download_results = try_join_all(download_futures).await?;

        for (res_name, resource_archive_path) in download_results {
            let stage_path = resource_staging_base.join(&res_name);
            create_dir_all_with_context(&stage_path, "resource stage path")?;
            debug!(
                " --> Staging resource '{}' to {}",
                res_name,
                stage_path.display()
            );
            let resource_archive_type_str =
                determine_archive_type(&resource_archive_path, &format!("resource '{res_name}'"))?;
            crate::build::extract::extract_archive(
                &resource_archive_path,
                &stage_path,
                0,
                resource_archive_type_str,
            )?;
            resource_stage_paths.insert(res_name, stage_path);
        }
    }

    debug!(
        "Building {} from source in {}",
        formula_name,
        build_dir.display()
    );

    debug!("Setting up build environment");
    let sapphire_prefix = config.prefix();
    let build_env = BuildEnvironment::new(
        formula,
        sapphire_prefix,
        &config.cellar,
        all_installed_paths,
    )?;

    if !resources.is_empty() {
        debug!("Installing {} resources into libexec", resources.len());
        let libexec_path = install_dir.join("libexec");
        create_dir_all_with_context(&libexec_path, "libexec directory")?;
        for resource in &resources {
            if let Some(stage_path) = resource_stage_paths.get(&resource.name) {
                install_resource(resource, stage_path, &libexec_path, &build_env)?;
            } else {
                debug!(
                    "Could not find stage path for resource '{}'. Skipping.",
                    resource.name
                );
            }
        }
    }

    debug!(
        "Detecting build system and building main formula: {}",
        formula_name
    );
    let source_subdir = determine_source_root(build_dir)?;
    detect_and_build(
        build_dir,
        &source_subdir,
        &install_dir,
        &build_env,
        all_installed_paths,
    )?;

    if !install_dir.exists() {
        debug!("Creating installation directory: {}", install_dir.display());
        fs::create_dir_all(&install_dir).map_err(|e| {
            SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed create install dir: {e}"),
            ))
        })?;
    } else {
        debug!(
            "Installation directory already exists: {}",
            install_dir.display()
        );
    }
    crate::build::write_receipt(formula, &install_dir)?;
    debug!(
        "Build completed, temporary directory {} will be cleaned up.",
        build_dir.display()
    );

    Ok(install_dir)
}

fn install_perl_resource(
    resource: &ResourceSpec,
    stage_path: &Path, // This is the CWD for the commands now
    libexec_path: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    let perl_exe = which::which_in("perl", build_env.get_path_string(), stage_path)
        .map_err(|_| SapphireError::BuildEnvError("perl not found.".to_string()))?;
    let make_exe = which::which_in("make", build_env.get_path_string(), stage_path)
        .map_err(|_| SapphireError::BuildEnvError("make not found.".to_string()))?;

    let perl_lib_target = libexec_path.join("lib").join("perl5");
    create_dir_all_with_context(&perl_lib_target, "Perl resource lib target")?;

    let mut cmd_env = build_env.get_vars().clone();
    let perl5lib_path_str = perl_lib_target.to_string_lossy().to_string();
    let current_perl5lib = cmd_env.get("PERL5LIB").cloned().unwrap_or_default();
    let new_perl5lib = if current_perl5lib.is_empty() {
        perl5lib_path_str
    } else {
        format!("{perl5lib_path_str}:{current_perl5lib}")
    };
    cmd_env.insert("PERL5LIB".into(), new_perl5lib.clone());

    let mut configure_cmd = Command::new(&perl_exe);
    configure_cmd
        .arg("Makefile.PL")
        .arg(format!("INSTALL_BASE={}", libexec_path.display()));
    configure_cmd.env_clear().envs(&cmd_env);
    run_command_in_dir(
        &mut configure_cmd,
        stage_path,
        build_env,
        &format!("Perl Makefile.PL for '{}'", resource.name),
    )?;

    let mut make_cmd = Command::new(make_exe.clone());
    make_cmd.env_clear().envs(&cmd_env);
    run_command_in_dir(
        &mut make_cmd,
        stage_path,
        build_env,
        &format!("make for Perl resource '{}'", resource.name),
    )?;

    let mut install_cmd = Command::new(make_exe);
    install_cmd.arg("install");
    install_cmd.env_clear().envs(&cmd_env);
    run_command_in_dir(
        &mut install_cmd,
        stage_path,
        build_env,
        &format!("make install for Perl resource '{}'", resource.name),
    )?;

    debug!(
        "   -> Successfully installed Perl resource '{}'",
        resource.name
    );
    Ok(())
}

fn install_python_resource(
    resource: &ResourceSpec,
    stage_path: &Path, // CWD for commands
    libexec_path: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    let python_exe = which::which_in("python3", build_env.get_path_string(), stage_path)
        .or_else(|_| which::which_in("python", build_env.get_path_string(), stage_path))
        .map_err(|_| SapphireError::BuildEnvError("python not found.".to_string()))?;

    let mut version_cmd = Command::new(&python_exe);
    version_cmd.args([
        "-c",
        "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')",
    ]);
    let python_version_output = run_command_in_dir(
        &mut version_cmd,
        stage_path,
        build_env,
        "get python version",
    )?;
    let python_version_str = String::from_utf8_lossy(&python_version_output.stdout)
        .trim()
        .to_string();

    if python_version_str.is_empty() {
        return Err(SapphireError::BuildEnvError(
            "Could not determine python version.".to_string(),
        ));
    }

    let python_site_packages = libexec_path
        .join("vendor")
        .join("lib")
        .join(format!("python{python_version_str}"))
        .join("site-packages");
    create_dir_all_with_context(&python_site_packages, "Python resource site-packages")?;

    let mut cmd_env = build_env.get_vars().clone();
    let pythonpath_entry = python_site_packages.to_string_lossy().to_string();
    let current_pythonpath = cmd_env.get("PYTHONPATH").cloned().unwrap_or_default();
    let new_pythonpath = if current_pythonpath.is_empty() {
        pythonpath_entry
    } else {
        format!(
            "{}:{}",
            pythonpath_entry,
            std::ffi::OsStr::new(&current_pythonpath).to_string_lossy()
        )
    };
    cmd_env.insert("PYTHONPATH".into(), new_pythonpath.clone());

    let mut install_cmd = Command::new(python_exe);
    install_cmd.arg("setup.py").arg("install").arg(format!(
        "--prefix={}",
        libexec_path.join("vendor").display()
    ));
    install_cmd.env_clear().envs(&cmd_env);
    run_command_in_dir(
        &mut install_cmd,
        stage_path,
        build_env,
        &format!("Python setup.py install for '{}'", resource.name),
    )?;

    debug!(
        "   -> Successfully installed Python resource '{}' to {}",
        resource.name,
        python_site_packages.display()
    );
    Ok(())
}

fn install_single_file(source_path: &Path, formula: &Formula, install_dir: &Path) -> Result<()> {
    let target_dir = install_dir.join("share").join(&formula.name);
    create_dir_all_with_context(&target_dir, "single file target directory")?;

    let target_filename = if formula.name == "ca-certificates" {
        source_path
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("cacert.pem"))
    } else {
        source_path
            .file_name()
            .ok_or_else(|| SapphireError::Generic("Source path has no filename.".to_string()))?
    };

    let target_path = target_dir.join(target_filename);

    debug!(
        "Copying {} to {}",
        source_path.display(),
        target_path.display()
    );
    fs::copy(source_path, &target_path).map_err(|e| {
        SapphireError::IoError(format!(
            "Failed copy {} to {}: {}",
            source_path.display(),
            target_path.display(),
            e
        ))
    })?;
    Ok(())
}

fn run_command_in_dir(
    cmd: &mut Command,
    cwd: &Path,
    build_env: &BuildEnvironment,
    context: &str,
) -> Result<Output> {
    build_env.apply_to_command(cmd);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::null()); // Prevent interference

    debug!(
        "Running command ({}) in [{}]: {:?}",
        context,
        cwd.display(),
        cmd
    );

    let output = cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!(
            "Failed to execute command for {} in {}: {}",
            context,
            cwd.display(),
            e
        ))
    })?;

    if !output.status.success() {
        error!(
            "Command failed for {} in [{}]. Status: {}",
            context,
            cwd.display(),
            output.status
        );
        error!("Stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        error!("Stderr:\n{}", String::from_utf8_lossy(&output.stderr));

        if context == "cmake configure" {
            let error_log = cwd.join("CMakeFiles/CMakeError.log");
            if error_log.exists() {
                eprintln!("--- CMakeFiles/CMakeError.log ---");
                if let Ok(content) = fs::read_to_string(&error_log) {
                    eprintln!("{content}");
                }
                eprintln!("--- End CMakeFiles/CMakeError.log ---");
            }
        } else if context == "configure" {
            let config_log_path = cwd.join("config.log");
            if config_log_path.exists() {
                eprintln!("--- Last 50 lines of config.log ---");
                if let Ok(content) = fs::read_to_string(&config_log_path) {
                    let lines: Vec<&str> = content.lines().rev().take(50).collect();
                    for line in lines.iter().rev() {
                        eprintln!("{line}");
                    }
                }
                eprintln!("--- End config.log ---");
            }
        }

        Err(SapphireError::CommandExecError(format!(
            "Command failed during {} stage in [{}]. Status: {}",
            context,
            cwd.display(),
            output.status
        )))
    } else {
        debug!("Command successful for {} in [{}]", context, cwd.display());
        Ok(output)
    }
}
