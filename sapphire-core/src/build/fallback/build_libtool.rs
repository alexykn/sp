// src/build/fallback/build_libtool.rs
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use crate::build::extract_archive_strip_components;
use super::{download_fallback_source, run_build_script_from_content}; // Use content runner
use std::path::{Path, PathBuf};
use log; // Import log crate

// Embed the script content at compile time
const LIBTOOL_SCRIPT_CONTENT: &str = include_str!("../../../../sapphire-cli/scripts/build_libtool.sh");

const LIBTOOL_VERSION: &str = "2.5.4"; // Using version from report example
const LIBTOOL_SOURCE_URL: &str = "https://ftp.gnu.org/gnu/libtool/libtool-2.5.4.tar.gz"; // Adjusted to .gz based on filename
const LIBTOOL_SCRIPT_NAME_HINT: &str = "build_libtool.sh";

/// Builds Libtool using its embedded shell script, passing required M4 and Autoconf prefixes.
pub async fn build_libtool_from_source(
    config: &Config,
    install_prefix: &Path, // Where to install libtool
    m4_prefix: &Path,      // Where prerequisite M4 is
    autoconf_prefix: &Path,// Where prerequisite Autoconf is
) -> Result<PathBuf> {
    log::info!("[Fallback] Starting build for Libtool v{} via embedded script", LIBTOOL_VERSION);
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
    // Assuming .tar.gz, strip 1 component (e.g., libtool-2.5.4/)
    extract_archive_strip_components(&source_tarball, build_dir, 1)?;

    // 4. Prepare arguments for the script
    let args = vec![
        build_dir.to_string_lossy().to_string(),       // Arg 1: Build directory
        install_prefix.to_string_lossy().to_string(),  // Arg 2: Installation prefix
        m4_prefix.to_string_lossy().to_string(),       // Arg 3: M4 prefix
        autoconf_prefix.to_string_lossy().to_string(), // Arg 4: Autoconf prefix
    ];

    // 5. Execute the embedded build script
    log::info!("[Fallback] Executing embedded Libtool build script...");
    run_build_script_from_content(
        LIBTOOL_SCRIPT_CONTENT,
        LIBTOOL_SCRIPT_NAME_HINT,
        "libtool",
        config,
        &args,
    )?;

    log::info!("[Fallback] Successfully executed script to build/install Libtool to {}", install_prefix.display());
    Ok(install_prefix.to_path_buf())
}