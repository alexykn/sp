// src/build/fallback/build_autoconf.rs
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use crate::build::extract_archive_strip_components;
use super::{download_fallback_source, find_build_script, run_build_command_for_script};
use std::path::{Path, PathBuf};
use std::process::Command;

const AUTOCONF_VERSION: &str = "2.72";
const AUTOCONF_SOURCE_URL: &str = "https://ftp.gnu.org/gnu/autoconf/autoconf-2.72.tar.gz";
const AUTOCONF_SCRIPT_NAME: &str = "build_autoconf.sh";

/// Builds Autoconf using its shell script, passing the required M4 prefix.
pub async fn build_autoconf_from_source(
    config: &Config,
    install_prefix: &Path, // Where to install autoconf
    m4_prefix: &Path,      // Where prerequisite M4 is installed
) -> Result<PathBuf> {
    log::info!("[Fallback] Starting build for Autoconf v{} via script", AUTOCONF_VERSION);
    log::info!("[Fallback] Using M4 dependency from: {}", m4_prefix.display());

    // 1. Download source
    let source_tarball = download_fallback_source(AUTOCONF_SOURCE_URL, "autoconf", config).await?;

    // 2. Create temp build directory
    let temp_dir = tempfile::Builder::new()
        .prefix("sapphire-build-autoconf-")
        .tempdir_in(&config.cache_dir)
        .map_err(|e| SapphireError::Io(e))?;
    let build_dir = temp_dir.path();
    log::info!("[Fallback] Created temp build directory: {}", build_dir.display());

    // 3. Extract source
    log::info!("[Fallback] Extracting Autoconf source {} to {}", source_tarball.display(), build_dir.display());
    extract_archive_strip_components(&source_tarball, build_dir, 1)?;

    // 4. Find the build script
    let script_path = find_build_script(AUTOCONF_SCRIPT_NAME)?;

    // 5. Execute the build script
    log::info!("[Fallback] Executing build script: {}", script_path.display());
    let mut cmd = Command::new("sh");
    cmd.arg(&script_path);
    // Pass arguments to the script
    cmd.arg(build_dir);         // Arg 1: Build directory
    cmd.arg(install_prefix);    // Arg 2: Installation prefix
    cmd.arg(m4_prefix);         // Arg 3: M4 prefix (for script's PATH setup)

    run_build_command_for_script(&mut cmd, AUTOCONF_SCRIPT_NAME, "autoconf")?;

    log::info!("[Fallback] Successfully executed script to build/install Autoconf to {}", install_prefix.display());
    Ok(install_prefix.to_path_buf())
}