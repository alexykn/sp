// src/build/fallback/build_m4.rs
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use crate::build::extract_archive_strip_components;
use super::{download_fallback_source, run_build_script_from_content}; // Use content runner
use std::path::{Path, PathBuf};
use log; // Import log crate

// Embed the script content at compile time
const M4_SCRIPT_CONTENT: &str = include_str!("../../../../sapphire-cli/scripts/build_m4.sh");

const M4_VERSION: &str = "1.4.19";
const M4_SOURCE_URL: &str = "https://ftp.gnu.org/gnu/m4/m4-1.4.19.tar.gz";
const M4_SCRIPT_NAME_HINT: &str = "build_m4.sh"; // For logging/temp filename

/// Downloads M4 source, extracts it, and executes the embedded build script.
/// Returns the path where M4 was installed.
pub async fn build_m4_from_source(config: &Config, install_prefix: &Path) -> Result<PathBuf> {
    log::info!("[Fallback] Starting build for M4 v{} via embedded script", M4_VERSION);

    // 1. Download source
    let source_tarball = download_fallback_source(M4_SOURCE_URL, "m4", config).await?;

    // 2. Create temp build directory
    let temp_dir = tempfile::Builder::new()
        .prefix("sapphire-build-m4-")
        .tempdir_in(&config.cache_dir)
        .map_err(|e| SapphireError::Io(e))?;
    let build_dir = temp_dir.path();
    log::info!("[Fallback] Created temp build directory: {}", build_dir.display());

    // 3. Extract source
    log::info!("[Fallback] Extracting M4 source {} to {}", source_tarball.display(), build_dir.display());
    extract_archive_strip_components(&source_tarball, build_dir, 1)?;

    // 4. Prepare arguments for the script
    let args = vec![
        build_dir.to_string_lossy().to_string(),       // Arg 1: Build directory path
        install_prefix.to_string_lossy().to_string(),  // Arg 2: Installation prefix path
    ];

    // 5. Execute the embedded build script
    log::info!("[Fallback] Executing embedded M4 build script...");
    run_build_script_from_content(
        M4_SCRIPT_CONTENT,
        M4_SCRIPT_NAME_HINT,
        "m4",
        config, // Pass config for temp file location
        &args,
    )?;

    log::info!("[Fallback] Successfully executed script to build/install M4 to {}", install_prefix.display());
    Ok(install_prefix.to_path_buf())
}