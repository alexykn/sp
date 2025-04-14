// **File:** sapphire-core/src/build/formula/source/mod.rs

use crate::utils::error::{SapphireError, Result};
use crate::model::formula::{Formula, FormulaDependencies}; // <-- Added FormulaDependencies import
use crate::utils::config::Config;
use crate::build::env::BuildEnvironment;
use crate::build;
use crate::build::extract_archive_strip_components; // Corrected import path
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::copy;
use reqwest::Client;
use std::io::Cursor;
use std::process::Command;
use log::{debug, error, info, warn}; // <-- Added error import

// Build system modules
mod make;
mod perl;
mod go;
mod python;
mod cmake;
mod meson;
mod cargo;

// Re-export build functions
pub use make::{configure_and_make, simple_make};
pub use perl::perl_build;
pub use go::go_build;
pub use python::python_build;
pub use cmake::cmake_build;
pub use meson::meson_build;
pub use cargo::cargo_build;

/// Download source code for the formula
pub async fn download_source(formula: &Formula, config: &Config) -> Result<PathBuf> {
    let url = if !formula.url.is_empty() {
        formula.url.clone()
    } else {
        // Attempt to derive URL from GitHub homepage if URL is missing
        if let Some(homepage) = &formula.homepage {
            if homepage.contains("github.com") {
                // *** FIX: Use stable_version_str for the tag ***
                format!("{}/archive/refs/tags/v{}.tar.gz", homepage.trim_end_matches('/'), formula.stable_version_str)
            } else {
                return Err(SapphireError::Generic(format!("No source URL available for {} and cannot derive from homepage: {}", formula.name, homepage)));
            }
        } else {
            return Err(SapphireError::Generic(format!("No source URL or homepage available for {}", formula.name)));
        }
    };

    info!("==> Downloading source for {}", formula.name);

    // Delegate to the actual download logic
    download_url(&url, config).await
}

/// Download a specific URL to the cache directory.
async fn download_url(url: &str, config: &Config) -> Result<PathBuf> {
    // Define source cache directory within the main cache dir
    let cache_dir = config.cache_dir.join("source");
    fs::create_dir_all(&cache_dir).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed to create source cache dir {}: {}", cache_dir.display(), e))))?;

    // Extract filename from URL
    let filename = url.split('/').last().unwrap_or("download.tmp");
    let source_path = cache_dir.join(filename);

    // Check cache first
    if source_path.exists() {
        info!("Using cached source: {}", source_path.display());
        // TODO: Add checksum verification here if source archives have SHA256 defined in formula?
        return Ok(source_path);
    }

    info!("Downloading {}", url);
    let client = Client::new(); // Consider reusing client if possible
    let response = client.get(url)
        .send()
        .await
        .map_err(|e| SapphireError::Http(e))?; // Use specific error variant

    if !response.status().is_success() {
        let status = response.status();
        // Try to read body for error details, but handle potential read errors
        let body = response.text().await.unwrap_or_else(|_| "Failed to read error response body".to_string());
        // *** FIX: Use imported error macro ***
        error!("Download failed for '{}' from {}: HTTP status {} - {}", filename, url, status, body);
        return Err(SapphireError::DownloadError(
            filename.to_string(), // Use filename being downloaded
            url.to_string(),
            format!("HTTP status {} - {}", status, body)
        ));
    }

    // Download to temporary file
    let content = response.bytes()
        .await
        .map_err(|e| SapphireError::Http(e))?; // Use specific error variant

    let temp_dl_path = source_path.with_extension("download_tmp");
    // Ensure temp file from previous run is removed
    if temp_dl_path.exists() {
        let _ = fs::remove_file(&temp_dl_path);
    }
    { // Scope for file lock
        let mut file = File::create(&temp_dl_path).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed create temp download file {}: {}", temp_dl_path.display(), e))))?;
        let mut content_cursor = Cursor::new(content);
        copy(&mut content_cursor, &mut file).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed write download stream to {}: {}", temp_dl_path.display(), e))))?;
    } // File closed here

    // Rename temp file to final path
    fs::rename(&temp_dl_path, &source_path).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed move temp download {} to {}: {}", temp_dl_path.display(), source_path.display(), e))))?;

    info!("Downloaded source to {}", source_path.display());

    Ok(source_path)
}

/// Build and install formula from downloaded source archive or file.
pub async fn build_from_source(
    source_path: &Path,
    formula: &Formula,
    config: &Config,
    all_installed_paths: &[PathBuf], // Expecting Vec<PathBuf> or similar slice
) -> Result<PathBuf> {
    // *** FIX: Use the FormulaDependencies trait ***
    let install_dir = formula.install_prefix(&config.cellar)?;
    let formula_name = formula.name(); // Use accessor method

    // Check if it's a single file or an archive
    let recognised_archive_extensions = ["tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz", "zip"];
    let source_extension = source_path.extension().and_then(|s| s.to_str()).unwrap_or("");

    // Handle single file installation (e.g., ca-certificates)
    if !recognised_archive_extensions.contains(&source_extension) {
        info!("==> Installing single file formula: {}", formula_name);
        // Ensure target directory exists
        fs::create_dir_all(&install_dir).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed create install dir {}: {}", install_dir.display(), e))))?;
        install_single_file(source_path, formula, &install_dir)?;
        // Write receipt after successful installation
        crate::build::write_receipt(formula, &install_dir)?; // Use crate::build path
        return Ok(install_dir);
    }

    // --- Archive Installation ---

    // Create a unique temporary directory for extraction and building
    let temp_dir_base = config.cache_dir.join("build-temp");
    fs::create_dir_all(&temp_dir_base).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed create build temp base {}: {}", temp_dir_base.display(), e))))?;
    // Use tempfile crate for robust temporary directory creation
    let temp_dir = tempfile::Builder::new()
        .prefix(&format!("{}-", formula_name))
        .tempdir_in(&temp_dir_base)
        .map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed create temp build dir in {}: {}", temp_dir_base.display(), e))))?
        .into_path(); // Get owned PathBuf

    info!("==> Extracting source {} to {}", source_path.display(), temp_dir.display());
    // Use the strip_components version of extraction
    extract_archive_strip_components(source_path, &temp_dir, 1)?;
    let build_dir = temp_dir; // Use the temp dir path directly

    info!("==> Building {} from source in {}", formula_name, build_dir.display());

    // --- Build Environment Setup ---
    info!("==> Setting up build environment");
    let sapphire_prefix = build::get_homebrew_prefix(); // Use crate::build path
    // Pass dependencies correctly. The caller (e.g., install command) should provide these.
    let build_env = BuildEnvironment::new(formula, &sapphire_prefix, &config.cellar, all_installed_paths)?;

    // --- Build Process ---
    // Detect build system and execute build within the extracted directory
    detect_and_build(formula, &build_dir, &install_dir, &build_env, all_installed_paths)?;

    // Write receipt after successful build and installation
    crate::build::write_receipt(formula, &install_dir)?; // Use crate::build path

    // --- Cleanup ---
    // Temporary directory `build_dir` will be automatically removed when `_temp_dir_guard` goes out of scope (RAII).
    // We can still manually try to remove if needed, but tempfile crate handles it.
    info!("Build completed, temporary directory {} will be cleaned up.", build_dir.display());

    Ok(install_dir) // Return the final installation directory
}

/// Install a single file formula by copying it to the appropriate location.
fn install_single_file(source_path: &Path, formula: &Formula, install_dir: &Path) -> Result<()> {
    // Determine target path based on formula or conventions
    let target_path = if formula.name == "ca-certificates" {
        // Special case for ca-certificates, install into share/formula_name/
        let share_dir = install_dir.join("share").join(formula.name());
        fs::create_dir_all(&share_dir).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed create share dir {}: {}", share_dir.display(), e))))?;
        // Use a standard name or the source filename
        share_dir.join(source_path.file_name().unwrap_or_else(|| OsStr::new("cacert.pem")))
    } else {
        // Default: install into share/formula_name/
        let target_share_dir = install_dir.join("share").join(formula.name());
        fs::create_dir_all(&target_share_dir).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed create share dir {}: {}", target_share_dir.display(), e))))?;
        let target_file_name = source_path.file_name().ok_or_else(|| SapphireError::Generic("Source path has no filename.".to_string()))?;
        target_share_dir.join(target_file_name)
    };

    info!("Copying {} to {}", source_path.display(), target_path.display());
    fs::copy(source_path, &target_path).map_err(|e| SapphireError::Io(std::io::Error::new(e.kind(), format!("Failed copy {} to {}: {}", source_path.display(), target_path.display(), e))))?;

    // Ensure correct permissions? Usually single files don't need execution perm.
    // If needed, add chmod logic here.

    Ok(())
}

/// Detect the build system and use the appropriate build method.
/// Changes working directory temporarily.
fn detect_and_build(
    formula: &Formula,
    build_dir: &Path, // This is the temporary directory where source was extracted
    install_dir: &Path, // This is the final Cellar path (e.g., Cellar/foo/1.1_5)
    build_env: &BuildEnvironment,
    all_installed_paths: &[PathBuf],
) -> Result<()> {
    // Ensure build directory exists before proceeding
    if !build_dir.exists() {
        return Err(SapphireError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Build directory does not exist: {}", build_dir.display()),
        )));
    }
    // Check read permissions
    fs::read_dir(build_dir).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Cannot access build directory {}: {}", build_dir.display(), e),
        ))
    })?;

    // Change CWD for the build process
    let original_cwd = std::env::current_dir().map_err(SapphireError::Io)?;
    info!("Changing working directory to build dir: {}", build_dir.display());
    std::env::set_current_dir(build_dir).map_err(SapphireError::Io)?;
    // Use RAII guard to ensure CWD is restored even on errors
    let _cwd_guard = CurrentWorkingDirectoryGuard::new(original_cwd);

    // --- Build System Detection Logic ---

    // Specific formula handlers first
    if formula.name == "perl" && Path::new("Configure").exists() {
        info!("Detected Perl build system (Configure script)");
        return perl::perl_build(build_dir, install_dir, build_env);
    }

    // Autotools: Check for configure.ac/in first, try autoreconf if configure doesn't exist
    if Path::new("configure.ac").exists() || Path::new("configure.in").exists() {
        // Try to find autoreconf in the build environment's PATH
        match which::which_in("autoreconf", build_env.get_path_string(), build_dir) {
            Ok(autoreconf_path) => {
                if !Path::new("configure").exists() {
                    info!("==> Running autoreconf -fvi (as configure script is missing)");
                    let mut cmd = Command::new(autoreconf_path);
                    cmd.args(["-fvi"]); // Force, Verbose, Install missing aux files
                    build_env.apply_to_command(&mut cmd); // Apply build env vars
                    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute autoreconf: {}", e)))?;
                    if !output.status.success() {
                        // *** FIX: Use imported error macro ***
                        error!("autoreconf failed with status: {}", output.status);
                        eprintln!("autoreconf stdout:\n{}", String::from_utf8_lossy(&output.stdout));
                        eprintln!("autoreconf stderr:\n{}", String::from_utf8_lossy(&output.stderr));
                        return Err(SapphireError::Generic(format!(
                            "autoreconf failed with status: {}", output.status
                        )));
                    } else {
                        // Print output only on success if verbose/debug logging is enabled
                        debug!("Autoreconf stdout:\n{}", String::from_utf8_lossy(&output.stdout));
                        debug!("Autoreconf stderr:\n{}", String::from_utf8_lossy(&output.stderr));
                    }
                } else {
                    info!("Skipping autoreconf, configure script already exists.");
                }
            }
            Err(_) => {
                // Autoreconf not found, but configure.ac exists. Proceed to check for 'configure'.
                warn!("configure.ac found but autoreconf not found in PATH. Will proceed only if 'configure' script exists.");
            }
        }
    }

    // Check for standard build files/scripts
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
    // Go build scripts (check within src subdir)
    let go_src_dir = Path::new("src");
    if go_src_dir.is_dir() && (go_src_dir.join("make.bash").exists() || go_src_dir.join("all.bash").exists()) {
        info!("Detected Go build system (make.bash or all.bash)");
        // Pass build_dir (the temp dir containing 'src')
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
    // Makefile check is usually last
    if Path::new("Makefile").exists() || Path::new("makefile").exists() {
        info!("Detected Makefile build system (no configure script)");
        return make::simple_make(install_dir, build_env);
    }

    // If no build system is detected
    // *** FIX: Use imported error macro ***
    error!("Could not determine build system for {}", build_dir.display());
    Err(SapphireError::Generic(format!(
        "Could not determine build system for {}",
        build_dir.display()
    )))
}

// RAII Guard to restore Current Working Directory
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
            // Use log crate for consistency
            log::error!( // Use log::error instead of bare error!
                "Failed to restore original working directory to {}: {}",
                self.original_cwd.display(),
                e
            );
        } else {
            log::debug!("Restored working directory to: {}", self.original_cwd.display());
        }
    }
}