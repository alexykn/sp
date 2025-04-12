// src/build/fallback/build_libtool.rs
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use crate::build::extract_archive_strip_components;
use super::{download_fallback_source, find_build_script, run_build_command_for_script};
use std::path::{Path, PathBuf};
use std::process::Command;

const LIBTOOL_VERSION: &str = "2.5.4"; // Using version from report example
const LIBTOOL_SOURCE_URL: &str = "https://ftp.gnu.org/gnu/libtool/libtool-2.5.4.tar.gz";
const LIBTOOL_SCRIPT_NAME: &str = "build_libtool.sh";

/// Builds Libtool using its shell script, passing required M4 and Autoconf prefixes.
pub async fn build_libtool_from_source(
    config: &Config,
    install_prefix: &Path, // Where to install libtool
    m4_prefix: &Path,      // Where prerequisite M4 is
    autoconf_prefix: &Path,// Where prerequisite Autoconf is
) -> Result<PathBuf> {
    log::info!("[Fallback] Starting build for Libtool v{} via script", LIBTOOL_VERSION);
    log::info!("[Fallback] Using M4 dependency from: {}", m4_prefix.display());
    log::info!("[Fallback] Using Autoconf dependency from: {}", autoconf_prefix.display());

    // 1. Download source
    let source_tarball = download_fallback_source(LIBTOOL_SOURCE_URL, "libtool", config).await?;

    // 2. Create temp build directory
    let temp_dir = tempfile::Builder::new()
        .prefix("sapphire-build-libtool-")
        .tempdir_in(&config.cache_dir)
        .map_err(|e| SapphireError::Io(e))?;
    let build_dir = temp_dir.path();
    log::info!("[Fallback] Created temp build directory: {}", build_dir.display());

    // 3. Extract source
    log::info!("[Fallback] Extracting Libtool source {} to {}", source_tarball.display(), build_dir.display());
    extract_archive_strip_components(&source_tarball, build_dir, 1)?;

    // 4. Find the build script
    let script_path = find_build_script(LIBTOOL_SCRIPT_NAME)?;

    // 5. Execute the build script
    log::info!("[Fallback] Executing build script: {}", script_path.display());
    let mut cmd = Command::new("sh");
    cmd.arg(&script_path);
    // Pass arguments
    cmd.arg(build_dir);         // Arg 1: Build directory
    cmd.arg(install_prefix);    // Arg 2: Installation prefix
    cmd.arg(m4_prefix);         // Arg 3: M4 prefix
    cmd.arg(autoconf_prefix);   // Arg 4: Autoconf prefix

    run_build_command_for_script(&mut cmd, LIBTOOL_SCRIPT_NAME, "libtool")?;

    log::info!("[Fallback] Successfully executed script to build/install Libtool to {}", install_prefix.display());
    Ok(install_prefix.to_path_buf())
}