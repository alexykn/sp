// sapphire-core/src/build/formula/source/mod.rs

// --- Imports ---
use std::collections::HashMap;
use std::fs::{self};
use std::path::{Path, PathBuf};
use std::process::Command;

use futures::future::try_join_all;
use infer;
use tracing::{debug, error, info, warn};

use crate::build::env::BuildEnvironment;
use crate::build::extract;
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

// --- Helper Functions (ensure these are present or imported) ---
fn create_dir_all_with_context(path: &Path, context: &str) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to create {} {}: {}", context, path.display(), e),
        ))
    })
}

/// Helper function to temporarily change CWD for executing a build function.
fn with_cwd<F>(target_cwd: &Path, build_func: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    let original_cwd = std::env::current_dir().map_err(SapphireError::Io)?;
    let mut must_restore = false;

    // Only change CWD if target_cwd is not the current CWD (".")
    if target_cwd != Path::new(".") {
        debug!("Temporarily changing CWD to {}", target_cwd.display());
        if let Err(e) = std::env::set_current_dir(target_cwd) {
            error!("Failed to change CWD to {}: {}", target_cwd.display(), e);
            return Err(SapphireError::Io(e));
        }
        must_restore = true;
    } else {
        debug!("Executing build function in current CWD.");
    }

    // Execute the build function
    let result = build_func();

    // Restore CWD if we changed it
    if must_restore {
        if let Err(e) = std::env::set_current_dir(&original_cwd) {
            error!(
                "CRITICAL: Failed to restore CWD from {} back to {}: {}. State may be invalid.",
                target_cwd.display(),
                original_cwd.display(),
                e
            );
            // If restoration fails, we should probably return the CWD error,
            // potentially masking the original build result if it was Ok.
            return Err(SapphireError::Io(e));
        } else {
            debug!("Restored CWD to {}", original_cwd.display());
        }
    }

    // Return the result of the build function
    result
}

/// Returns Ok(true) if build system found and called, Ok(false) if not found, Err on build error.
fn check_markers_and_build(
    dir_to_check: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
    all_installed_paths: &[PathBuf],
) -> Result<bool> {
    // --- Autoreconf Check (specific to the directory being checked) ---
    // This should happen *before* checking for 'configure' existence
    if (dir_to_check.join("configure.ac").exists() || dir_to_check.join("configure.in").exists())
        && !dir_to_check.join("configure").exists()
    {
        // Need to run autoreconf *within* dir_to_check if it's not CWD
        let original_cwd = std::env::current_dir().map_err(SapphireError::Io)?;
        let mut must_restore_cwd = false;
        if dir_to_check != Path::new(".") {
            debug!(
                "Temporarily changing CWD to {} for autoreconf check",
                dir_to_check.display()
            );
            std::env::set_current_dir(dir_to_check).map_err(SapphireError::Io)?;
            must_restore_cwd = true;
        }

        match which::which_in("autoreconf", build_env.get_path_string(), Path::new(".")) {
            Ok(autoreconf_path) => {
                info!(
                    "==> Running autoreconf -fvi (as configure script is missing in {})",
                    dir_to_check.display()
                );
                let mut cmd = Command::new(autoreconf_path);
                cmd.args(["-fvi"]);
                build_env.apply_to_command(&mut cmd);
                match run_command(&mut cmd, "autoreconf") {
                    Ok(_) => info!("Autoreconf completed successfully."),
                    Err(e) => warn!("Autoreconf failed ({}). Continuing build detection...", e),
                }
            }
            Err(_) => {
                warn!("configure.ac/in found but configure script and autoreconf command are missing.");
            }
        }

        // Restore CWD if we changed it
        if must_restore_cwd {
            if let Err(e) = std::env::set_current_dir(&original_cwd) {
                error!(
                    "FATAL: Failed to restore CWD after autoreconf check in {}: {}",
                    dir_to_check.display(),
                    e
                );
                // This is tricky - state is potentially bad. Maybe return a fatal error?
                return Err(SapphireError::Io(e));
            } else {
                debug!("Restored CWD after autoreconf check.");
            }
        }
        // After running autoreconf, the 'configure' script might now exist, so we continue
        // detection.
    }

    // --- Marker Checks ---
    // Note: These checks use relative paths which are interpreted based on CWD
    // We need to ensure the build functions are called with the *correct* context/CWD.

    // Check for complex build systems first (CMake, Meson often contain other languages)
    if dir_to_check.join("CMakeLists.txt").exists() {
        info!("Detected build system: CMake in {}", dir_to_check.display());
        with_cwd(dir_to_check, || {
            cmake::cmake_build(Path::new("."), install_dir, build_env)
        })?;
        return Ok(true);
    }
    if dir_to_check.join("meson.build").exists() {
        info!("Detected build system: Meson in {}", dir_to_check.display());
        with_cwd(dir_to_check, || {
            meson::meson_build(Path::new("."), install_dir, build_env)
        })?;
        return Ok(true);
    }
    // Check for Autotools *after* CMake/Meson, as they might be preferred if both exist
    if dir_to_check.join("configure").exists() {
        info!(
            "Detected build system: Autotools (configure script) in {}",
            dir_to_check.display()
        );
        with_cwd(dir_to_check, || {
            make::configure_and_make(install_dir, build_env)
        })?;
        return Ok(true);
    }

    // --- NEW: Go Module Check (Before generic Makefiles, Cargo, Python, Perl) ---
    if dir_to_check.join("go.mod").exists() {
        info!("Detected Go module (go.mod) in {}", dir_to_check.display());
        // Use the *updated* go_build function
        with_cwd(dir_to_check, || {
            go::go_build(Path::new("."), install_dir, build_env, all_installed_paths)
        })?;
        return Ok(true);
    }

    // Continue with other language-specific build systems
    if dir_to_check.join("Makefile.PL").exists() || dir_to_check.join("Configure").exists() {
        info!(
            "Detected build system: Perl (Makefile.PL or Configure) in {}",
            dir_to_check.display()
        );
        with_cwd(dir_to_check, || {
            perl::perl_build(Path::new("."), install_dir, build_env)
        })?;
        return Ok(true);
    }
    if dir_to_check.join("Cargo.toml").exists() {
        info!(
            "Detected build system: Rust/Cargo in {}",
            dir_to_check.display()
        );
        with_cwd(dir_to_check, || cargo::cargo_build(install_dir, build_env))?;
        return Ok(true);
    }
    if dir_to_check.join("setup.py").exists() {
        info!(
            "Detected build system: Python setup.py in {}",
            dir_to_check.display()
        );
        with_cwd(dir_to_check, || {
            python::python_build(install_dir, build_env)
        })?;
        return Ok(true);
    }
    // Deprecate or remove the old Go check based on make.bash/all.bash?
    // For now, let's keep it *after* the go.mod check as a fallback for older Go projects
    // that might still use this structure (like Go itself).
    let go_src_dir = dir_to_check.join("src");
    if go_src_dir.is_dir()
        && (go_src_dir.join("make.bash").exists() || go_src_dir.join("all.bash").exists())
    {
        warn!(
            "Detected legacy Go build system (make.bash/all.bash) in {}. Using older build logic.",
            dir_to_check.display()
        );
        // The *original* go_build handled this. We need to decide if we keep both.
        // For simplicity now, let's assume the new go_build is for modules, and this
        // path might need a different function or be removed if make.bash is rare.
        // Let's log a warning and return false for now, falling through to simple_make
        // if a Makefile exists, or erroring out.
        // OR, call the old go_build logic if we rename it, e.g., go_build_legacy.
        // For now, let's assume make.bash projects likely *also* have a Makefile.
        warn!("Legacy Go build script detected, but specific handling is pending. Falling back...");
    }

    // --- Simple Makefile Fallback (Last Resort) ---
    if dir_to_check.join("Makefile").exists() || dir_to_check.join("makefile").exists() {
        info!(
            "Detected build system: Simple Makefile in {}", // Note: Go modules might fall here if go.mod check fails or legacy path is taken
            dir_to_check.display()
        );
        with_cwd(dir_to_check, || make::simple_make(install_dir, build_env))?;
        return Ok(true);
    }

    // No known build system found in this directory
    Ok(false)
}

fn run_command(cmd: &mut Command, context: &str) -> Result<std::process::Output> {
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
        // Optionally log stdout/stderr on success if needed at debug level
        Ok(output)
    }
}

/// Detects the build system based on marker files *in the current working directory*
/// or a single subdirectory, and dispatches to the appropriate build function.
fn detect_and_build_in_cwd(
    _formula: &Formula, // Prefixed as it's still not used directly here
    install_dir: &Path,
    build_env: &BuildEnvironment,
    all_installed_paths: &[PathBuf],
) -> Result<()> {
    info!("Attempting to detect build system in current directory (CWD)");

    let cwd = Path::new("."); // Represents the current working directory

    // --- Check for markers directly in CWD first ---
    if check_markers_and_build(cwd, install_dir, build_env, all_installed_paths)? {
        return Ok(()); // Build system found and handled in CWD
    }

    // --- If not found in CWD, check for a single subdirectory ---
    let mut subdirs = Vec::new();
    match fs::read_dir(cwd) {
        Ok(entries) => {
            for entry_res in entries {
                if let Ok(entry) = entry_res {
                    let path = entry.path();
                    // Ignore hidden files/dirs and resource staging dir
                    if path.is_dir()
                        && !entry.file_name().to_string_lossy().starts_with('.')
                        && entry.file_name() != ".sapphire-resources"
                    {
                        subdirs.push(path);
                    }
                } else {
                    warn!(
                        "Failed to read directory entry in CWD: {:?}",
                        entry_res.err()
                    );
                }
            }
        }
        Err(e) => {
            return Err(SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to read CWD to check for subdirectories: {}", e),
            )));
        }
    }

    if subdirs.len() == 1 {
        let subdir_path = &subdirs[0];
        info!(
            "No build system found in CWD, checking single subdirectory: {}",
            subdir_path.display()
        );

        // --- Check for markers inside the subdirectory ---
        if check_markers_and_build(subdir_path, install_dir, build_env, all_installed_paths)? {
            return Ok(()); // Build system found and handled in subdirectory
        }
    } else if subdirs.len() > 1 {
        info!("Multiple subdirectories found, cannot automatically determine build root.");
    } else {
        info!("No subdirectories found to check.");
    }

    // If no known build system is detected in CWD or single subdirectory
    error!("Could not determine build system in CWD or its immediate subdirectory.");
    Err(SapphireError::Generic(
        "Could not determine build system in source directory.".to_string(),
    ))
}

fn determine_archive_type(archive_path: &Path, _context: &str) -> Result<&'static str> {
    // <-- Prefixed
    match infer::get_from_path(archive_path)? {
        Some(kind) => {
            let ext = kind.extension();
            // Use the constant defined earlier
            if SUPPORTED_ARCHIVE_EXTENSIONS.contains(&ext) {
                SUPPORTED_ARCHIVE_EXTENSIONS
                    .iter()
                    .find(|&&s| s == ext)
                    .copied()
                    .ok_or_else(|| {
                        SapphireError::Generic(format!(
                            "Internal error matching inferred extension {}",
                            ext
                        ))
                    })
            } else {
                Err(SapphireError::Generic(format!(
                    "Unsupported inferred archive type '{}' for {}",
                    ext,
                    archive_path.display() // Add path for context
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
                        SapphireError::Generic(format!(
                            "Internal error matching file extension {}",
                            ext
                        ))
                    })
            } else {
                Err(SapphireError::Generic(format!(
                    "Unsupported file extension '{}' for {}",
                    ext,
                    archive_path.display() // Add path for context
                )))
            }
        }
    }
}

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
    // Prioritize Perl check if both Makefile.PL and setup.py might exist
    if stage_path.join("Makefile.PL").exists() {
        // Check for Perl first
        info!(
            "   -> Detected Perl resource '{}', installing...",
            resource.name
        );
        // Call the function to handle Perl resource installation
        install_perl_resource(resource, libexec_path, build_env)?;
    } else if stage_path.join("setup.py").exists() {
        // Check for Python next
        info!(
            "   -> Detected Python resource '{}', installing...",
            resource.name
        );
        // Call the function to handle Python resource installation
        install_python_resource(resource, libexec_path, build_env)?;
    } else {
        // We could potentially add more resource build system detections here
        // (e.g., simple make install)
        warn!(
            "   -> Could not detect known build system (Perl/Python) for resource '{}' in {}. Skipping install.",
            resource.name,
            stage_path.display()
        );
    }
    Ok(()) // CWD is restored when _cwd_guard goes out of scope
}

// --- build_from_source ---
pub async fn build_from_source(
    source_path: &Path, // Path to the downloaded archive
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

    // Check if the extension indicates it's NOT a recognized archive type
    // If it's not a known archive, assume it's a single file to be installed directly.
    if !RECOGNISED_SINGLE_FILE_EXTENSIONS.contains(&source_extension) {
        info!("==> Installing single file formula: {}", formula_name);
        create_dir_all_with_context(&install_dir, "install directory")?;
        // Call the function that handles copying the single file
        install_single_file(source_path, formula, &install_dir)?;
        crate::build::write_receipt(formula, &install_dir)?;
        return Ok(install_dir);
    }

    // --- Determine Archive Type and Infer Root ---
    // (Assuming source_path is guaranteed to be an archive now,
    // single file check might be moved before calling build_from_source if needed)
    let source_archive_type_str = determine_archive_type(source_path, "main source archive")?; // Use existing helper

    // Infer the root directory *before* extraction
    let inferred_root_dir = extract::infer_archive_root_dir(source_path, source_archive_type_str)?;

    let strip_components = if inferred_root_dir.is_some() {
        // If a single root dir exists, strip it during extraction
        tracing::debug!("Detected single root dir in archive, will use strip_components=1.");
        1
    } else {
        // If archive is flat or has multiple roots, don't strip
        tracing::debug!("Archive is flat or has multiple roots, using strip_components=0.");
        0
    };

    // --- Staging Area Setup ---
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
    let build_dir = temp_build_dir.path(); // This is where files will land after stripping

    // --- Extract with calculated strip_components ---
    info!(
        "==> Extracting main source {} to {} (strip_components={})",
        source_path.display(),
        build_dir.display(),
        strip_components
    );
    // Call the existing extract_archive function (ensure it's in scope, maybe
    // crate::build::extract::extract_archive)
    crate::build::extract::extract_archive(
        source_path,
        build_dir,
        strip_components,
        source_archive_type_str,
    )?;
    debug!("==> Extracted main source to {}", build_dir.display());

    // --- Resource Handling (remains the same) ---
    let resources = formula.resources()?; // Assume this returns Vec<ResourceSpec>
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
            // Resources are typically extracted without stripping components
            crate::build::extract::extract_archive(
                &resource_archive_path,
                &stage_path,
                0, // strip_components = 0 for resources
                resource_archive_type_str,
            )?;
            resource_stage_paths.insert(res_name, stage_path);
        }
    }

    info!(
        "==> Building {} from source in {}",
        formula_name,
        build_dir.display() // Build happens directly in the temp dir now
    );

    // --- Build Environment Setup (remains the same) ---
    info!("==> Setting up build environment");
    let sapphire_prefix = config.prefix();
    let build_env = BuildEnvironment::new(
        formula,
        sapphire_prefix,
        &config.cellar,
        all_installed_paths,
    )?;

    // --- Build Process (with CWD management) ---
    let original_cwd = std::env::current_dir().map_err(SapphireError::Io)?;
    info!(
        "Changing working directory to build dir: {}",
        build_dir.display()
    );
    std::env::set_current_dir(build_dir).map_err(SapphireError::Io)?;
    // RAII guard ensures CWD is restored even if subsequent steps panic or return Err
    let _cwd_guard = CurrentWorkingDirectoryGuard::new(original_cwd.clone());

    // --- Autoreconf Check (remains the same) ---
    if (Path::new("configure.ac").exists() || Path::new("configure.in").exists())
        && !Path::new("configure").exists()
    {
        match which::which_in("autoreconf", build_env.get_path_string(), Path::new(".")) {
            Ok(autoreconf_path) => {
                info!("==> Running autoreconf -fvi (as configure script is missing)");
                let mut cmd = Command::new(autoreconf_path);
                cmd.args(["-fvi"]);
                build_env.apply_to_command(&mut cmd);
                match run_command(&mut cmd, "autoreconf") {
                    Ok(_) => info!("Autoreconf completed successfully."),
                    Err(e) => warn!("Autoreconf failed ({}). Continuing build detection...", e),
                }
            }
            Err(_) => {
                warn!("configure.ac/in found but configure script and autoreconf command are missing.");
            }
        }
    }

    // --- Install Resources First (remains the same) ---
    if !resources.is_empty() {
        info!("==> Installing {} resources into libexec", resources.len());
        let libexec_path = install_dir.join("libexec");
        create_dir_all_with_context(&libexec_path, "libexec directory")?;

        for resource in &resources {
            if let Some(stage_path) = resource_stage_paths.get(&resource.name) {
                info!(" --> Installing resource: {}", resource.name);
                // install_resource changes CWD, ensure build_dir is restored afterward
                // let build_dir_cwd = std::env::current_dir().map_err(SapphireError::Io)?; // Not
                // needed due to guard install_resource needs to be called within
                // the context of the _cwd_guard
                install_resource(resource, stage_path, &libexec_path, &build_env)?;
                // No need to manually restore CWD here, the guard handles it.
            } else {
                warn!(
                    "Could not find stage path for resource '{}'. Skipping installation.",
                    resource.name
                );
            }
        }
    }

    // --- Build Main Formula using simplified detection ---
    info!(
        "==> Detecting build system and building main formula: {}",
        formula_name
    );
    // CWD is now guaranteed to be the source root directory
    detect_and_build_in_cwd(
        formula,
        &install_dir,
        &build_env,
        all_installed_paths, // Keep passing this for Go build
    )?;

    if !install_dir.exists() {
        info!("Creating installation directory: {}", install_dir.display());
        fs::create_dir_all(&install_dir).map_err(|e| {
            SapphireError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed create install dir {}: {}", install_dir.display(), e),
            ))
        })?;
    } else {
        debug!(
            "Installation directory already exists: {}",
            install_dir.display()
        );
    }
    crate::build::write_receipt(formula, &install_dir)?; // Ensure write_receipt is available
    info!(
        "Build completed, temporary directory {} will be cleaned up.",
        build_dir.display()
    );
    Ok(install_dir)
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
    cmd_env.insert("PERL5LIB".into(), new_perl5lib.clone()); // Use OsString/OsStr

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
    cmd_env.insert("PYTHONPATH".into(), new_pythonpath.clone());

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
