// sapphire-core/src/build/formula/source/mod.rs

// --- Imports ---
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use futures::future::try_join_all;
use infer;
use tracing::{debug, error, info, warn};
use walkdir::WalkDir;

use crate::build::env::BuildEnvironment;
use crate::build::extract::extract_archive;
use crate::fetch::http as http_fetch;
use crate::model::formula::{Formula, FormulaDependencies, ResourceSpec};
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};

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

// --- Constants ---
const SUPPORTED_ARCHIVE_EXTENSIONS: [&str; 5] = ["gz", "bz2", "xz", "tar", "zip"];
const RECOGNISED_SINGLE_FILE_EXTENSIONS: [&str; 9] =
    ["tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz", "zip"];

// --- Helper Functions ---

/// Creates a directory and all its parents, mapping IO errors with context.
fn create_dir_all_with_context(path: &Path, context: &str) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to create {} {}: {}", context, path.display(), e),
        ))
    })
}

/// Executes a command, checks status, and returns a detailed error on failure.
fn run_command(cmd: &mut Command, context: &str) -> Result<Output> {
    debug!("Running command ({}): {:?}", context, cmd);
    let output = cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute command for {}: {}", context, e))
    })?;

    if !output.status.success() {
        error!("Command failed for {}. Status: {}", context, output.status);
        error!("Stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        error!("Stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        Err(SapphireError::CommandExecError(format!(
            "Command failed during {} stage. Status: {}",
            context, output.status
        )))
    } else {
        debug!("Command successful for {}", context);
        debug!("Stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        debug!("Stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        Ok(output)
    }
}

/// Tries to infer archive type, falls back to extension, and validates.
fn determine_archive_type(archive_path: &Path, context: &str) -> Result<&'static str> {
    match infer::get_from_path(archive_path)? {
        // Handles infer's IO error
        Some(kind) => {
            let ext = kind.extension();
            debug!("Inferred archive type for {}: {}", context, ext);
            if SUPPORTED_ARCHIVE_EXTENSIONS.contains(&ext) {
                Ok(ext)
            } else {
                error!(
                    "Unsupported inferred archive type '{}' for {}: {}",
                    ext,
                    context,
                    archive_path.display()
                );
                Err(SapphireError::Generic(format!(
                    "Unsupported inferred archive type '{}' for {}: {}",
                    ext,
                    context,
                    archive_path.display()
                )))
            }
        }
        None => {
            // Fallback to extension if infer fails
            let fallback_ext = archive_path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            warn!(
                "Could not infer archive type for {}, falling back to extension '{}'.",
                archive_path.display(),
                fallback_ext
            );
            if fallback_ext.is_empty() {
                error!(
                    "Cannot determine archive type for {}: {}",
                    context,
                    archive_path.display()
                );
                return Err(SapphireError::Generic(format!(
                    "Cannot determine archive type for {}: {}",
                    context,
                    archive_path.display()
                )));
            }
            // Check if fallback is supported
            if SUPPORTED_ARCHIVE_EXTENSIONS.contains(&fallback_ext) {
                // We need to return a &'static str. Find the matching static string.
                SUPPORTED_ARCHIVE_EXTENSIONS
                    .iter()
                    .find(|&&s| s == fallback_ext)
                    .copied()
                    .ok_or_else(|| {
                        SapphireError::Generic(format!(
                            "Internal error: Matched extension '{}' not found in static list",
                            fallback_ext
                        ))
                    }) // Should not happen
            } else {
                error!(
                    "Unsupported fallback archive type '{}' for {}: {}",
                    fallback_ext,
                    context,
                    archive_path.display()
                );
                Err(SapphireError::Generic(format!(
                    "Unsupported fallback archive type '{}' for {}: {}",
                    fallback_ext,
                    context,
                    archive_path.display()
                )))
            }
        }
    }
}

// --- download_source ---
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

// --- build_from_source ---
pub async fn build_from_source(
    source_path: &Path,
    formula: &Formula,
    config: &Config,
    all_installed_paths: &[PathBuf],
) -> Result<PathBuf> {
    let install_dir = formula.install_prefix(&config.cellar)?;
    let formula_name = formula.name();

    // Single file installation check
    let source_extension = source_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if !RECOGNISED_SINGLE_FILE_EXTENSIONS.contains(&source_extension) {
        info!("==> Installing single file formula: {}", formula_name);
        create_dir_all_with_context(&install_dir, "install directory")?;
        install_single_file(source_path, formula, &install_dir)?;
        crate::build::write_receipt(formula, &install_dir)?;
        return Ok(install_dir);
    }

    // Archive Installation Setup
    let temp_dir_base = config.cache_dir.join("build-temp");
    create_dir_all_with_context(&temp_dir_base, "build temp base")?;
    let temp_build_dir = tempfile::Builder::new()
        .prefix(&format!("{}-", formula_name))
        .tempdir_in(&temp_dir_base)
        .map_err(|e| {
            SapphireError::IoError(format!(
                "Failed create temp build dir in {}: {}",
                temp_dir_base.display(),
                e
            ))
        })?;
    let build_dir = temp_build_dir.path();

    info!(
        "==> Extracting main source {} to {}",
        source_path.display(),
        build_dir.display()
    );
    let source_archive_type_str = determine_archive_type(source_path, "main source")?;
    extract_archive(source_path, build_dir, 1, source_archive_type_str)?; // strip_components = 1
    debug!("==> Extracted main source to {}", build_dir.display());

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
        create_dir_all_with_context(&resource_staging_base, "resource staging base")?;

        // Download all resources concurrently
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

        // Extract downloaded resources
        for (res_name, resource_archive_path) in download_results {
            let stage_path = resource_staging_base.join(&res_name);
            create_dir_all_with_context(&stage_path, "resource stage path")?;
            info!(
                " --> Staging resource '{}' from {} to {}",
                res_name,
                resource_archive_path.display(),
                stage_path.display()
            );
            let resource_archive_type_str = determine_archive_type(
                &resource_archive_path,
                &format!("resource '{}'", res_name),
            )?;
            extract_archive(
                &resource_archive_path,
                &stage_path,
                0,
                resource_archive_type_str,
            )?; // strip_components = 0
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
    let sapphire_prefix = config.prefix();
    let build_env = BuildEnvironment::new(
        formula,
        &sapphire_prefix,
        &config.cellar,
        all_installed_paths,
    )?;

    // Build Process (with CWD management)
    let original_cwd = std::env::current_dir().map_err(SapphireError::Io)?;
    info!(
        "Changing working directory to build dir: {}",
        build_dir.display()
    );
    std::env::set_current_dir(build_dir).map_err(SapphireError::Io)?;
    let _cwd_guard = CurrentWorkingDirectoryGuard::new(original_cwd.clone()); // RAII guard

    // Install Resources First (if any)
    if !resources.is_empty() {
        info!("==> Installing {} resources into libexec", resources.len());
        let libexec_path = install_dir.join("libexec");
        create_dir_all_with_context(&libexec_path, "libexec directory")?;

        for resource in &resources {
            if let Some(stage_path) = resource_stage_paths.get(&resource.name) {
                info!(" --> Installing resource: {}", resource.name);
                // install_resource changes CWD, ensure build_dir is restored afterward
                let build_dir_cwd = std::env::current_dir().map_err(SapphireError::Io)?;
                install_resource(resource, stage_path, &libexec_path, &build_env)?;
                info!(
                    "Restoring working directory after resource install: {}",
                    build_dir_cwd.display()
                );
                std::env::set_current_dir(build_dir_cwd).map_err(SapphireError::Io)?;
            } else {
                warn!(
                    "Could not find stage path for resource '{}'. Skipping installation.",
                    resource.name
                );
            }
        }
    }

    // Build Main Formula
    info!("==> Building main formula: {}", formula_name);
    detect_and_build(
        formula,
        build_dir, // Pass the root build dir
        &install_dir,
        &build_env,
        all_installed_paths,
    )?;

    // Post-Install (CWD restored by _cwd_guard dropping)
    // No need to set CWD back manually here if _cwd_guard is used correctly.
    // std::env::set_current_dir(original_cwd).map_err(SapphireError::Io)?; // This is redundant
    crate::build::write_receipt(formula, &install_dir)?;
    info!(
        "Build completed, temporary directory {} will be cleaned up.",
        build_dir.display()
    );
    Ok(install_dir)
}

// --- detect_and_build ---
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

    // Marker format: (filename, system_name, requires_root_dir_detection)
    // requires_root_dir_detection: true if we generally expect this marker ONLY at the root.
    let markers: &[(&str, &str, bool)] = &[
        ("configure", "Autotools (configure script)", true), // Must be at root
        ("CMakeLists.txt", "CMake", false),                  // Can be nested
        ("meson.build", "Meson", false),                     // Can be nested
        ("Makefile.PL", "Perl (Makefile.PL)", true),         // Must be at root
        ("Configure", "Perl (Configure)", true),             // Must be at root
        ("Cargo.toml", "Rust/Cargo", true),                  // Must be at root
        ("setup.py", "Python setup.py", true),               // Must be at root
        ("Makefile", "Makefile", true),                      // Must be at root
        ("makefile", "Makefile", true),                      // Must be at root
    ];

    // --- Special case checks first ---

    // Handle autoreconf possibility if configure script is missing but .ac/.in exists
    if (build_dir.join("configure.ac").exists() || build_dir.join("configure.in").exists())
        && !build_dir.join("configure").exists()
    {
        match which::which_in("autoreconf", build_env.get_path_string(), build_dir) {
            Ok(autoreconf_path) => {
                info!("==> Running autoreconf -fvi (as configure script is missing)");
                let mut cmd = Command::new(autoreconf_path);
                cmd.args(["-fvi"]);
                build_env.apply_to_command(&mut cmd);
                // Use helper, but only warn on failure, don't stop detection
                match run_command(&mut cmd, "autoreconf") {
                    Ok(_) => info!("Autoreconf completed successfully."),
                    Err(e) => warn!("Autoreconf failed ({}). Continuing detection...", e),
                }
            }
            Err(_) => {
                warn!("configure.ac/in found but configure script and autoreconf command are missing.");
            }
        }
    }

    // Handle Go structure (special case not based on single marker file)
    let go_src_dir = build_dir.join("src");
    if go_src_dir.is_dir()
        && (go_src_dir.join("make.bash").exists() || go_src_dir.join("all.bash").exists())
    {
        info!("Detected Go build system (make.bash or all.bash)");
        // Go build usually expects to run from the root of the extracted source
        return go::go_build(build_dir, install_dir, build_env, all_installed_paths);
    }

    // --- Search for markers ---
    // Stores best match: (marker_filename, marker_containing_dir_path, depth, score)
    let mut best_match: Option<(String, PathBuf, usize, i32)> = None;
    let base_formula_name = formula
        .name()
        .split('@')
        .next()
        .unwrap_or_else(|| formula.name());
    let preferred_subdirs: Vec<OsString> = vec![
        OsString::from("src"),
        OsString::from("source"),
        OsString::from(base_formula_name), // e.g., "llvm" for "llvm@19"
    ];

    // Iterate through directories using WalkDir, limiting depth
    for entry_result in WalkDir::new(build_dir)
        .min_depth(0) // Start from root
        .max_depth(2) // Check root, depth 1, depth 2
        .into_iter()
        .filter_map(|e| e.ok())
    // Filter out errors during walk
    {
        let current_path = entry_result.path(); // Path to the file/directory entry
        let current_depth = entry_result.depth();

        if current_path.is_file() {
            let file_name_os = entry_result.file_name();
            let file_name = file_name_os.to_str().unwrap_or("");
            let parent_dir = current_path.parent().unwrap_or(build_dir); // Dir containing the file

            // Find if this filename is a known marker
            if let Some((marker, _system_name, requires_root)) =
                markers.iter().find(|(m, _, _)| *m == file_name)
            {
                // Skip non-root markers if they require root detection
                if *requires_root && current_depth > 0 {
                    debug!(
                        "Skipping marker '{}' found at depth {}, requires root.",
                        marker, current_depth
                    );
                    continue;
                }

                // --- Scoring Logic ---
                let mut score = match current_depth {
                    0 => 3, // Root match is highest preference
                    1 | 2 => {
                        // Nested matches (only relevant for CMake/Meson now)
                        if *marker == "CMakeLists.txt" || *marker == "meson.build" {
                            let parent_dir_name = parent_dir.file_name().map(|f| f.to_os_string());
                            if parent_dir_name
                                .map_or(false, |name| preferred_subdirs.contains(&name))
                            {
                                2 // Preferred subdirectory
                            } else {
                                1 // Other subdirectory
                            }
                        } else {
                            // Other markers shouldn't be scored if nested due to requires_root
                            // check above
                            continue; // Skip scoring this nested non-CMake/Meson marker
                        }
                    }
                    _ => 0, // Deeper matches ignored by max_depth
                };

                // Adjust score based on depth (prefer shallower for same base score)
                score -= current_depth as i32;

                // Marker list priority (lower index = higher priority)
                let current_priority = markers
                    .iter()
                    .position(|(m, _, _)| m == marker)
                    .unwrap_or(usize::MAX);

                // --- Update best match ---
                let is_better = match best_match {
                    None => true,
                    Some((_, _, _, existing_score)) if score > existing_score => true,
                    Some((ref existing_marker, _, _, existing_score))
                        if score == existing_score =>
                    {
                        let existing_priority = markers
                            .iter()
                            .position(|(m, _, _)| m == existing_marker)
                            .unwrap_or(usize::MAX);
                        current_priority < existing_priority // Better priority at same score
                    }
                    _ => false, // Not better
                };

                if is_better {
                    debug!(
                        "Updating best match: '{}' in {} (depth {}, score {}, priority {})",
                        marker,
                        parent_dir.display(),
                        current_depth,
                        score,
                        current_priority
                    );
                    best_match = Some((
                        marker.to_string(),
                        parent_dir.to_path_buf(), // Store dir *containing* marker
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
            "Detected build system '{}' (marker: '{}', score: {}) in {} (depth: {})",
            system_name,
            marker_name,
            score,
            marker_dir.display(), // Log the directory *containing* the marker
            depth
        );

        // Determine the effective source directory for the build command
        // Most build systems run from the root of the extracted archive.
        // CMake and Meson are exceptions; they need the directory containing the marker file.
        let source_path_for_build =
            if depth > 0 && (marker_name == "CMakeLists.txt" || marker_name == "meson.build") {
                info!(
                    "Using nested marker directory for build: {}",
                    marker_dir.display()
                );
                marker_dir.as_path()
            } else {
                // Default to the root build directory for root markers or other (unexpected) nested
                // ones.
                info!(
                    "Using root build directory for build: {}",
                    build_dir.display()
                );
                build_dir
            };

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

// --- dispatch_build ---
fn dispatch_build(
    marker_filename: &str,
    source_dir_for_build: &Path, // Path build func should use (might be root or nested)
    install_dir: &Path,
    build_env: &BuildEnvironment,
    _all_installed_paths: &[PathBuf], /* Keep in case Go needs it, though it's handled
                                       * separately now */
) -> Result<()> {
    // Remember: The *current working directory* for dispatch_build is still the root `build_dir`.
    // The build functions themselves might change CWD or operate relative to
    // `source_dir_for_build`.
    match marker_filename {
        "configure" => {
            info!("Dispatching to Autotools (configure script)");
            // configure_and_make assumes it runs ./configure from the CWD (which is root build_dir)
            make::configure_and_make(install_dir, build_env)
        }
        "CMakeLists.txt" => {
            info!("Dispatching to CMake");
            // cmake_build needs the path containing CMakeLists.txt
            cmake::cmake_build(source_dir_for_build, install_dir, build_env)
        }
        "meson.build" => {
            info!("Dispatching to Meson");
            // meson_build needs the path containing meson.build
            meson::meson_build(source_dir_for_build, install_dir, build_env)
        }
        "Makefile.PL" | "Configure" => {
            info!("Dispatching to Perl build");
            // perl_build likely expects to run from the CWD (root build_dir)
            // It might internally use source_dir_for_build if needed, but typically runs `perl
            // Makefile.PL` in CWD
            perl::perl_build(source_dir_for_build, install_dir, build_env) // Pass root source dir
        }
        "Cargo.toml" => {
            info!("Dispatching to Rust/Cargo");
            // cargo_build runs `cargo build --release` from CWD (root build_dir)
            cargo::cargo_build(install_dir, build_env)
        }
        "setup.py" => {
            info!("Dispatching to Python setup.py");
            // python_build runs `python setup.py install` from CWD (root build_dir)
            python::python_build(install_dir, build_env)
        }
        "Makefile" | "makefile" => {
            info!("Dispatching to simple Makefile");
            // simple_make runs `make install` from CWD (root build_dir)
            make::simple_make(install_dir, build_env)
        }
        // Note: Go is handled earlier as a special case.
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

// --- Resource installation helpers ---
fn install_resource(
    resource: &ResourceSpec,
    stage_path: &Path,   // Directory where resource was extracted
    libexec_path: &Path, // Base libexec path (e.g., <prefix>/libexec)
    build_env: &BuildEnvironment,
) -> Result<()> {
    let original_cwd = std::env::current_dir().map_err(SapphireError::Io)?;
    debug!(
        "Changing CWD for resource '{}' install: {}",
        resource.name,
        stage_path.display()
    );
    std::env::set_current_dir(stage_path).map_err(SapphireError::Io)?;
    // Use RAII guard to ensure CWD restoration even on errors
    let _cwd_guard = CurrentWorkingDirectoryGuard::new(original_cwd);

    // Check for build files within the staged resource directory
    if stage_path.join("Makefile.PL").exists() {
        info!("   -> Detected Perl resource '{}'", resource.name);
        install_perl_resource(resource, libexec_path, build_env)?;
    } else if stage_path.join("setup.py").exists() {
        info!("   -> Detected Python resource '{}'", resource.name);
        install_python_resource(resource, libexec_path, build_env)?;
    } else {
        // We could potentially add more resource build system detections here (e.g., simple make
        // install)
        warn!(
            "   -> Could not detect known build system (Perl/Python) for resource '{}' in {}. Skipping install.",
            resource.name,
            stage_path.display()
        );
    }
    Ok(()) // CWD is restored when _cwd_guard goes out of scope
}

fn install_perl_resource(
    resource: &ResourceSpec,
    libexec_path: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    // CWD is expected to be the resource stage_path here
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
    create_dir_all_with_context(&perl_lib_target, "Perl resource lib target")?;

    // Environment setup for Perl
    let mut cmd_env = build_env.get_vars().clone();
    let perl5lib_path_str = perl_lib_target.to_string_lossy().to_string();
    let current_perl5lib = cmd_env.get("PERL5LIB").cloned().unwrap_or_default();
    let new_perl5lib = if current_perl5lib.is_empty() {
        perl5lib_path_str
    } else {
        format!("{}:{}", perl5lib_path_str, current_perl5lib) // Prepend target dir
    };
    cmd_env.insert("PERL5LIB".into(), new_perl5lib.clone().into()); // Use OsString/OsStr

    // Run perl Makefile.PL INSTALL_BASE=<libexec>
    let mut configure_cmd = Command::new(&perl_exe);
    configure_cmd
        .arg("Makefile.PL")
        .arg(format!("INSTALL_BASE={}", libexec_path.display()));
    configure_cmd.env_clear().envs(&cmd_env); // Apply full env
    run_command(
        &mut configure_cmd,
        &format!("Perl Makefile.PL for resource '{}'", resource.name),
    )?;

    // Run make
    let mut make_cmd = Command::new(make_exe.clone());
    make_cmd.env_clear().envs(&cmd_env); // Apply full env
    run_command(
        &mut make_cmd,
        &format!("make for Perl resource '{}'", resource.name),
    )?;

    // Run make install
    let mut install_cmd = Command::new(make_exe);
    install_cmd.arg("install");
    install_cmd.env_clear().envs(&cmd_env); // Apply full env
    run_command(
        &mut install_cmd,
        &format!("make install for Perl resource '{}'", resource.name),
    )?;

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
    // CWD is expected to be the resource stage_path here
    let python_exe = which::which_in("python3", build_env.get_path_string(), Path::new("."))
        .or_else(|_| which::which_in("python", build_env.get_path_string(), Path::new(".")))
        .map_err(|_| {
            SapphireError::BuildEnvError(
                "python not found in build env PATH for resource install".to_string(),
            )
        })?;

    // Determine Python version for site-packages path
    let mut version_cmd = Command::new(&python_exe);
    version_cmd.args([
        "-c",
        "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')",
    ]);
    build_env.apply_to_command(&mut version_cmd); // Ensure correct python is used from env path
    let python_version_output = run_command(&mut version_cmd, "get python version")?;
    let python_version_str = String::from_utf8_lossy(&python_version_output.stdout)
        .trim()
        .to_string();

    if python_version_str.is_empty() {
        return Err(SapphireError::BuildEnvError(
            "Could not determine python version for resource path.".to_string(),
        ));
    }

    // Define target vendor site-packages directory
    let python_site_packages = libexec_path
        .join("vendor") // Install into vendor dir inside libexec
        .join("lib")
        .join(format!("python{}", python_version_str))
        .join("site-packages");
    create_dir_all_with_context(&python_site_packages, "Python resource site-packages")?;

    // Environment setup for Python
    let mut cmd_env = build_env.get_vars().clone();
    let pythonpath_entry = python_site_packages.to_string_lossy().to_string();
    let current_pythonpath = cmd_env.get("PYTHONPATH").cloned().unwrap_or_default();
    let new_pythonpath = if current_pythonpath.is_empty() {
        pythonpath_entry
    } else {
        // Prepend target dir
        format!(
            "{}:{}",
            pythonpath_entry,
            std::ffi::OsStr::new(&current_pythonpath).to_string_lossy()
        )
    };
    cmd_env.insert("PYTHONPATH".into(), new_pythonpath.clone().into());

    // Run python setup.py install --prefix=<libexec>/vendor
    let mut install_cmd = Command::new(python_exe);
    install_cmd.arg("setup.py").arg("install").arg(format!(
        "--prefix={}",
        libexec_path.join("vendor").display()
    )); // Install under vendor prefix
    install_cmd.env_clear().envs(&cmd_env); // Apply full env
    run_command(
        &mut install_cmd,
        &format!("Python setup.py install for resource '{}'", resource.name),
    )?;

    info!(
        "   -> Successfully installed Python resource '{}' to {}",
        resource.name,
        python_site_packages.display()
    );
    Ok(())
}

// --- Single file install ---
fn install_single_file(source_path: &Path, formula: &Formula, install_dir: &Path) -> Result<()> {
    // Special handling for ca-certificates, otherwise install to share/<formula_name>
    let target_dir = if formula.name == "ca-certificates" {
        install_dir.join("share").join(&formula.name)
    } else {
        install_dir.join("share").join(&formula.name)
    };
    create_dir_all_with_context(&target_dir, "single file target directory")?;

    let target_filename = source_path
        .file_name()
        .ok_or_else(|| SapphireError::Generic("Source path has no filename.".to_string()))?;
    let target_path = target_dir.join(target_filename);

    info!(
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

// --- CurrentWorkingDirectoryGuard ---
/// Restores the original working directory when dropped (RAII).
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
            // Use tracing::error! for consistency if tracing is the standard logger
            error!(
                "Failed to restore original working directory to {}: {}",
                self.original_cwd.display(),
                e
            );
        } else {
            debug!(
                "Restored working directory to: {}",
                self.original_cwd.display()
            );
        }
    }
}
