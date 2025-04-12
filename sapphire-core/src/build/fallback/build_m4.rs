// src/build/fallback/build_m4.rs
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use crate::build::extract_archive_strip_components;
use super::{download_fallback_source, find_build_script, run_build_command_for_script}; // Use script runner
use std::path::{Path, PathBuf};
use std::process::Command;

const M4_VERSION: &str = "1.4.19";
const M4_SOURCE_URL: &str = "https://ftp.gnu.org/gnu/m4/m4-1.4.19.tar.gz";
const M4_SCRIPT_NAME: &str = "build_m4.sh";

/// Downloads M4 source, extracts it, and executes the build script.
/// Returns the path where M4 was installed.
pub async fn build_m4_from_source(config: &Config, install_prefix: &Path) -> Result<PathBuf> {
    log::info!("[Fallback] Starting build for M4 v{} via script", M4_VERSION);

    // 1. Download source (Rust helper)
    let source_tarball = download_fallback_source(M4_SOURCE_URL, "m4", config).await?;

    // 2. Create temp build directory (Rust helper)
    let temp_dir = tempfile::Builder::new()
        .prefix("sapphire-build-m4-")
        .tempdir_in(&config.cache_dir) // Build within cache dir
        .map_err(|e| SapphireError::Io(e))?;
    let build_dir = temp_dir.path(); // This is where we extract and build
    log::info!("[Fallback] Created temp build directory: {}", build_dir.display());

    // 3. Extract source (Rust helper)
    log::info!("[Fallback] Extracting M4 source {} to {}", source_tarball.display(), build_dir.display());
    extract_archive_strip_components(&source_tarball, build_dir, 1)?;

    // 4. Find the build script (Rust helper)
    let script_path = find_build_script(M4_SCRIPT_NAME)?;

    // 5. Execute the build script
    log::info!("[Fallback] Executing build script: {}", script_path.display());
    let mut cmd = Command::new("sh"); // Standard shell
    cmd.arg(&script_path);
    // Pass necessary info to the script as arguments
    cmd.arg(build_dir);         // Arg 1: Build directory path (where source was extracted)
    cmd.arg(install_prefix);    // Arg 2: Installation prefix path

    // Run the script using the helper
    run_build_command_for_script(&mut cmd, M4_SCRIPT_NAME, "m4")?;

    // Cleanup of temp_dir happens automatically when it goes out of scope

    log::info!("[Fallback] Successfully executed script to build/install M4 to {}", install_prefix.display());
    Ok(install_prefix.to_path_buf())
}