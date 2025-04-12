// src/build/fallback/build_autoconf.rs
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use crate::build::extract_archive_strip_components;
use super::{download_fallback_source, run_build_script_from_content}; // Use content runner
use std::path::{Path, PathBuf};
use log; // Import log crate

// Embed the script content at compile time
const AUTOCONF_SCRIPT_CONTENT: &str = include_str!("../../../../sapphire-cli/scripts/build_autoconf.sh");

const AUTOCONF_VERSION: &str = "2.72";
const AUTOCONF_SOURCE_URL: &str = "https://ftp.gnu.org/gnu/autoconf/autoconf-2.72.tar.gz";
const AUTOCONF_SCRIPT_NAME_HINT: &str = "build_autoconf.sh";

/// Builds Autoconf using its embedded shell script, passing the required M4 prefix.
pub async fn build_autoconf_from_source(
    config: &Config,
    install_prefix: &Path, // Where to install autoconf
    m4_prefix: &Path,      // Where prerequisite M4 is installed
) -> Result<PathBuf> {
    log::info!("[Fallback] Starting build for Autoconf v{} via embedded script", AUTOCONF_VERSION);
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

    // 4. Prepare arguments for the script
    let args = vec![
        build_dir.to_string_lossy().to_string(),       // Arg 1: Build directory
        install_prefix.to_string_lossy().to_string(),  // Arg 2: Installation prefix
        m4_prefix.to_string_lossy().to_string(),       // Arg 3: M4 prefix
    ];

    // 5. Execute the embedded build script
    log::info!("[Fallback] Executing embedded Autoconf build script...");
     run_build_script_from_content(
        AUTOCONF_SCRIPT_CONTENT,
        AUTOCONF_SCRIPT_NAME_HINT,
        "autoconf",
        config,
        &args,
    )?;


    log::info!("[Fallback] Successfully executed script to build/install Autoconf to {}", install_prefix.display());
    Ok(install_prefix.to_path_buf())
}