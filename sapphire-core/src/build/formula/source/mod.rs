use crate::utils::error::{SapphireError, Result};
use crate::model::formula::Formula;
use crate::utils::config::Config;
use crate::build::env::BuildEnvironment;
use crate::build;
use crate::build::extract_archive_strip_components;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::copy;
use reqwest::Client;
use std::io::Cursor;
use std::process::Command;
use log::{debug, info, warn};

mod make;
mod perl;
mod go;
mod python;
mod cmake;
mod meson;
mod cargo;

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
        if let Some(homepage) = &formula.homepage {
            if homepage.contains("github.com") {
                format!("{}/archive/refs/tags/v{}.tar.gz", homepage.trim_end_matches('/'), formula.version)
            } else {
                return Err(SapphireError::Generic(format!("No source URL available for {} and cannot derive from homepage", formula.name)));
            }
        } else {
            return Err(SapphireError::Generic(format!("No source URL available for {}", formula.name)));
        }
    };

    info!("==> Downloading source for {}", formula.name);

    download_url(&url, config).await
}

/// Download a specific URL to the cache
async fn download_url(url: &str, config: &Config) -> Result<PathBuf> {
    let cache_dir = PathBuf::from(&config.cache_dir).join("source");
    fs::create_dir_all(&cache_dir)?;

    let filename = url.split('/').last().unwrap_or("download.tmp");
    let source_path = cache_dir.join(filename);

    if source_path.exists() {
        info!("Using cached source: {}", source_path.display());
        return Ok(source_path);
    }

    info!("Downloading {}", url);
    let client = Client::new();
    let response = client.get(url)
        .send()
        .await
        .map_err(|e| SapphireError::Http(e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_else(|_| "Failed to read body".to_string());
        return Err(SapphireError::Generic(format!(
            "Download failed for '{}' from {}: HTTP status {} - {}",
            filename, url, status, body
        )));
    }

    let content = response.bytes()
        .await
        .map_err(|e| SapphireError::Http(e))?;

    let temp_dl_path = source_path.with_extension("download_tmp");
    {
        let mut file = File::create(&temp_dl_path)?;
        let mut content_cursor = Cursor::new(content);
        copy(&mut content_cursor, &mut file)?;
    }
    fs::rename(&temp_dl_path, &source_path)?;

    info!("Downloaded source to {}", source_path.display());

    Ok(source_path)
}

/// Build and install formula from downloaded source archive or file.
pub async fn build_from_source(
    source_path: &Path,
    formula: &Formula,
    config: &Config,
    all_installed_paths: &[PathBuf],
) -> Result<PathBuf> {
    let install_dir = super::get_formula_cellar_path(formula);
    let formula_name = formula.name();

    let recognised_archive_extensions = ["tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz", "zip"];
    let source_extension = source_path.extension().and_then(|s| s.to_str()).unwrap_or("");

    if !recognised_archive_extensions.contains(&source_extension) {
        info!("==> Installing single file formula: {}", formula_name);
        fs::create_dir_all(&install_dir)?;
        install_single_file(&source_path, formula, &install_dir)?;
        super::write_receipt(formula, &install_dir)?;
        return Ok(install_dir);
    }

    let temp_dir_base = config.cache_dir.join("build-temp");
    fs::create_dir_all(&temp_dir_base)?;
    let temp_dir = tempfile::Builder::new()
        .prefix(&format!("{}-", formula_name))
        .tempdir_in(&temp_dir_base)
        .map_err(|e| SapphireError::Io(e))?
        .into_path();

    info!("==> Extracting source {} to {}", source_path.display(), temp_dir.display());
    extract_archive_strip_components(&source_path, &temp_dir, 1)?;
    let build_dir = temp_dir;

    info!("==> Building {} from source in {}", formula_name, build_dir.display());

    info!("==> Setting up build environment");
    let sapphire_prefix = build::get_homebrew_prefix();
    let build_env = BuildEnvironment::new(formula, &sapphire_prefix, &config.cellar, all_installed_paths)?;

    detect_and_build(formula, &build_dir, &install_dir, &build_env, all_installed_paths)?;
    super::write_receipt(formula, &install_dir)?;

    if build_dir.exists() {
        if let Err(e) = fs::remove_dir_all(&build_dir) {
            warn!("Warning: Failed to clean up temporary build directory {}: {}", build_dir.display(), e);
        }
    }
    Ok(install_dir)
}

/// Install a single file formula by copying it.
fn install_single_file(source_path: &Path, formula: &Formula, install_dir: &Path) -> Result<()> {
    let target_path = if formula.name == "ca-certificates" {
        let share_dir = install_dir.join("share").join(formula.name());
        fs::create_dir_all(&share_dir)?;
        share_dir.join(source_path.file_name().unwrap_or_else(|| OsStr::new("cacert.pem")))
    } else {
        let target_share_dir = install_dir.join("share").join(formula.name());
        fs::create_dir_all(&target_share_dir)?;
        let target_file_name = source_path.file_name().ok_or_else(|| SapphireError::Generic("Source path has no filename.".to_string()))?;
        target_share_dir.join(target_file_name)
    };

    info!("Copying {} to {}", source_path.display(), target_path.display());
    fs::copy(source_path, &target_path)?;

    Ok(())
}

/// Detect the build system and use the appropriate build method
fn detect_and_build(
    formula: &Formula,
    build_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
    all_installed_paths: &[PathBuf],
) -> Result<()> {
    if !build_dir.exists() {
        return Err(SapphireError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Build directory does not exist: {}", build_dir.display()),
        )));
    }
    fs::read_dir(build_dir).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Cannot access build directory {}: {}", build_dir.display(), e),
        ))
    })?;

    let original_cwd = std::env::current_dir()?;
    std::env::set_current_dir(build_dir)?;
    info!("Changed working directory to: {}", build_dir.display());
    let _cwd_guard = CurrentWorkingDirectoryGuard::new(original_cwd);

    if formula.name == "perl" && build_dir.join("Configure").exists() {
        return perl::perl_build(build_dir, install_dir, build_env);
    }
    if build_dir.join("configure.ac").exists() || build_dir.join("configure.in").exists() {
        match which::which_in("autoreconf", build_env.get_path_string(), build_dir) {
            Ok(autoreconf_path) => {
                if !build_dir.join("configure").exists() {
                    info!("==> Running autoreconf -fvi");
                    let mut cmd = Command::new(autoreconf_path);
                    cmd.args(["-fvi"]);
                    build_env.apply_to_command(&mut cmd);
                    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute autoreconf: {}", e)))?;
                    if !output.status.success() {
                        println!("autoreconf failed with status: {}", output.status);
                        eprintln!("autoreconf stdout:\n{}", String::from_utf8_lossy(&output.stdout));
                        eprintln!("autoreconf stderr:\n{}", String::from_utf8_lossy(&output.stderr));
                        return Err(SapphireError::Generic(format!(
                            "autoreconf failed with status: {}", output.status
                        )));
                    } else {
                        debug!("Autoreconf stdout:\n{}", String::from_utf8_lossy(&output.stdout));
                        debug!("Autoreconf stderr:\n{}", String::from_utf8_lossy(&output.stderr));
                    }
                } else {
                    info!("Skipping autoreconf, configure script already exists.");
                }
            }
            Err(_) => {
                warn!("configure.ac found but autoreconf not found in PATH. Will proceed to check for 'configure' script.");
            }
        }
    }
    let configure_script = build_dir.join("configure");
    if configure_script.exists() {
        return make::configure_and_make(install_dir, build_env);
    }
    if build_dir.join("CMakeLists.txt").exists() {
        return cmake::cmake_build(build_dir, install_dir, build_env);
    }
    if build_dir.join("meson.build").exists() {
        return meson::meson_build(build_dir, install_dir, build_env);
    }
    let go_make_script = build_dir.join("src/make.bash");
    let go_all_script = build_dir.join("src/all.bash");
    if go_make_script.exists() || go_all_script.exists() {
        return go::go_build(build_dir, install_dir, build_env, all_installed_paths);
    }
    if build_dir.join("Cargo.toml").exists() {
        return cargo::cargo_build(install_dir, build_env);
    }
    if build_dir.join("setup.py").exists() {
        return python::python_build(install_dir, build_env);
    }
    if build_dir.join("Makefile").exists() || build_dir.join("makefile").exists() {
        return make::simple_make(install_dir, build_env);
    }
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
            log::error!(
                "Failed to restore original working directory to {}: {}",
                self.original_cwd.display(),
                e
            );
        } else {
            debug!("Restored working directory to: {}", self.original_cwd.display());
        }
    }
}