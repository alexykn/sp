// src/build/fallback/mod.rs
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
use crate::build::extract_archive_strip_components; // Use existing extractor
use std::path::{Path, PathBuf};
use std::process::Command;
use std::collections::HashMap; // Keep for future env var needs

// Declare the sub-modules
pub mod build_m4;
pub mod build_autoconf;
pub mod build_libtool;

// Re-export the main build functions
pub use build_m4::build_m4_from_source;
pub use build_autoconf::build_autoconf_from_source;
pub use build_libtool::build_libtool_from_source;

/// Determines the installation prefix for fallback builds.
/// Example: Install into a subdir within the cache to keep it isolated.
/// IMPORTANT: Ensure this chosen location is appropriate for your needs
/// (persistence vs. temporary, permissions).
pub fn get_fallback_install_prefix(tool_name: &str, config: &Config) -> Result<PathBuf> {
    let fallback_dir = config.cache_dir.join("fallback_builds").join(tool_name);
    log::info!("[Fallback] Determined install prefix for {}: {}", tool_name, fallback_dir.display());
    // Ensure the *parent* directory exists, script's `make install` will create final dir
    if let Some(parent) = fallback_dir.parent() {
         if !parent.exists() {
             log::debug!("[Fallback] Creating parent install directory: {}", parent.display());
             std::fs::create_dir_all(parent).map_err(SapphireError::Io)?;
         }
    } else {
         // This case should ideally not happen if cache_dir is valid
         return Err(SapphireError::Generic(format!("Invalid fallback install prefix configuration for {}", tool_name)));
    }
    Ok(fallback_dir)
}

/// Shared function to download source tarball for fallback builds.
/// Downloads to `cache_dir/fallback_source/`.
pub(crate) async fn download_fallback_source(url: &str, tool_name: &str, config: &Config) -> Result<PathBuf> {
    log::info!("[Fallback] Attempting download for {} from {}", tool_name, url);
    // Use a specific subdirectory within cache for fallback sources
    let cache_dir = config.cache_dir.join("fallback_source");
    if !cache_dir.exists() {
        log::debug!("[Fallback] Creating fallback source cache: {}", cache_dir.display());
        std::fs::create_dir_all(&cache_dir).map_err(SapphireError::Io)?;
    }

    let filename = url.split('/').last()
        .ok_or_else(|| SapphireError::Generic(format!("Could not extract filename from URL: {}", url)))?;
    // Include tool name in cached filename to avoid potential collisions if versions overlap
    let target_path = cache_dir.join(format!("{}-{}", tool_name, filename));

    if target_path.exists() {
        log::info!("[Fallback] Using cached source: {}", target_path.display());
        // Optional: Add SHA verification here if needed
        return Ok(target_path);
    }

    log::info!("[Fallback] Downloading {} to {}", url, target_path.display());
    let client = reqwest::Client::builder()
         .user_agent(format!("sapphire-package-manager/fallback-downloader-0.1")) // Example User-Agent
         // Add timeouts if desired: .timeout(std::time::Duration::from_secs(300))
         .build()
         .map_err(|e| SapphireError::Http(e))?;

    let response = client.get(url).send().await.map_err(SapphireError::Http)?;

    if !response.status().is_success() {
        let status = response.status();
        // Attempt to read body for more context, but don't fail if body reading fails
        let body_text = response.text().await.unwrap_or_else(|e| format!("(Failed to read response body: {})", e));
        log::error!("[Fallback] Download failed for {} ({}) from {}: {}", tool_name, status, url, body_text);
        // Create a more informative error, potentially wrapping the reqwest error
        return Err(SapphireError::Generic(format!(
             "Failed to download fallback source for {} from {}: HTTP Status {}", tool_name, url, status
        )));
    }

    let content = response.bytes().await.map_err(SapphireError::Http)?;
    // Write to a temporary file first, then rename to avoid partial downloads being seen as complete
    let temp_path = target_path.with_extension("download_tmp");
    { // Scope to ensure file is closed before rename
        let mut temp_file = std::fs::File::create(&temp_path).map_err(SapphireError::Io)?;
        std::io::copy(&mut std::io::Cursor::new(content), &mut temp_file).map_err(SapphireError::Io)?;
    }
    std::fs::rename(&temp_path, &target_path).map_err(SapphireError::Io)?;


    log::info!("[Fallback] Downloaded fallback source for {} successfully.", tool_name);
    // Optional: Add SHA verification here
    Ok(target_path)
}

/// Shared function to run an external build script, log output, and handle errors.
pub(crate) fn run_build_command_for_script(
    command: &mut Command,
    script_name: &str, // e.g., "build_m4.sh"
    tool_name: &str, // e.g., "m4"
) -> Result<()> {
    log::info!("[Fallback] Running script {} for {}: {:?}", script_name, tool_name, command);
    let output = command.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute build script {} for {}: {}", script_name, tool_name, e)))?;

    // Log stdout/stderr regardless of success for debugging potential script issues
    if !output.stdout.is_empty() {
        log::debug!("[Fallback] Script {} stdout:\n{}", script_name, String::from_utf8_lossy(&output.stdout));
    }
     if !output.stderr.is_empty() {
        log::debug!("[Fallback] Script {} stderr:\n{}", script_name, String::from_utf8_lossy(&output.stderr));
     }

    if !output.status.success() {
        log::error!("[Fallback] Build script {} failed for {} with status: {}", script_name, tool_name, output.status);
        // Error message should include stderr if available and non-empty
        let stderr_msg = String::from_utf8_lossy(&output.stderr);
        let error_detail = if stderr_msg.trim().is_empty() {
            format!("Script exited with status {}", output.status)
        } else {
            format!("Script exited with status {}. Stderr:\n{}", output.status, stderr_msg)
        };
        return Err(SapphireError::InstallError(format!( // Use InstallError variant
            "Build script {} failed for {}. {}", script_name, tool_name, error_detail
        )));
    }

    log::info!("[Fallback] Script {} for {} completed successfully.", script_name, tool_name);
    Ok(())
}

/// Helper to find the bundled build script.
/// Adjust the search logic based on how/where you bundle the scripts.
pub(crate) fn find_build_script(script_name: &str) -> Result<PathBuf> {
     // 1. Look relative to the current executable (common for bundled apps)
     if let Ok(current_exe) = std::env::current_exe() {
         if let Some(exe_dir) = current_exe.parent() {
             let script_path = exe_dir.join("scripts").join(script_name);
             if script_path.exists() {
                 log::debug!("[Fallback] Found build script via executable path: {}", script_path.display());
                 // Ensure execute permissions (important!)
                 ensure_execute_permission(&script_path)?;
                 return Ok(script_path);
             }
             // Also check directly alongside executable
             let script_path_alt = exe_dir.join(script_name);
              if script_path_alt.exists() {
                 log::debug!("[Fallback] Found build script alongside executable: {}", script_path_alt.display());
                 ensure_execute_permission(&script_path_alt)?;
                 return Ok(script_path_alt);
             }
         }
     }

     // 2. Look relative to Current Working Directory (useful during development)
     let cwd_scripts_path = std::env::current_dir().map_err(SapphireError::Io)?.join("scripts").join(script_name);
      if cwd_scripts_path.exists() {
         log::debug!("[Fallback] Found build script via CWD: {}", cwd_scripts_path.display());
         ensure_execute_permission(&cwd_scripts_path)?;
         return Ok(cwd_scripts_path);
      }
     let cwd_path_alt = std::env::current_dir().map_err(SapphireError::Io)?.join(script_name);
       if cwd_path_alt.exists() {
         log::debug!("[Fallback] Found build script via CWD (root): {}", cwd_path_alt.display());
         ensure_execute_permission(&cwd_path_alt)?;
         return Ok(cwd_path_alt);
      }


     // 3. Add other search locations if necessary (e.g., fixed system path)

     log::error!("[Fallback] Fallback build script '{}' could not be found.", script_name);
     Err(SapphireError::BuildEnvError(format!("Fallback build script '{}' not found. Searched relative to executable and CWD.", script_name)))
}

/// Ensures a script has execute permissions (Unix only).
#[cfg(unix)]
fn ensure_execute_permission(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path).map_err(SapphireError::Io)?;
    let mut permissions = metadata.permissions();
    let current_mode = permissions.mode();
    // Check if execute bit is set for user, group, or others
    if current_mode & 0o111 == 0 {
        log::warn!("[Fallback] Build script {} is not executable. Attempting to set +x.", path.display());
        // Add execute permission for user (minimum needed)
        permissions.set_mode(current_mode | 0o100);
        std::fs::set_permissions(path, permissions).map_err(SapphireError::Io)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_execute_permission(_path: &Path) -> Result<()> {
    // No-op on non-Unix platforms
    Ok(())
}


// RAII Guard to restore Current Working Directory
// Ensures we change back even if there's a panic or early return.
pub(crate) struct CurrentWorkingDirectoryGuard {
    original_cwd: PathBuf,
}
impl CurrentWorkingDirectoryGuard {
    pub fn new(original_cwd: PathBuf) -> Self { Self { original_cwd } }
}
impl Drop for CurrentWorkingDirectoryGuard {
    fn drop(&mut self) {
        if let Err(e) = std::env::set_current_dir(&self.original_cwd) {
            log::error!("Failed to restore CWD to {}: {}", self.original_cwd.display(), e);
        } else { log::debug!("Restored CWD to: {}", self.original_cwd.display()); }
    }
}