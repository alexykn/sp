// src/build/fallback/mod.rs
use crate::utils::config::Config;
use crate::utils::error::{Result, SapphireError};
// Removed unused: use crate::build::extract_archive_strip_components;
use std::path::PathBuf; // Removed unused Path
// Removed unused: use std::collections::HashMap;
// Removed unused: use std::fs::File;
use std::io::Write; // For writing to temp file
use tempfile::Builder as TempFileBuilder; // Import Builder and rename for clarity
use std::process::Command;
use log;

// Declare the sub-modules
pub mod build_m4;
pub mod build_autoconf;
pub mod build_libtool;

// Re-export the main build functions
pub use build_m4::build_m4_from_source;
pub use build_autoconf::build_autoconf_from_source;
pub use build_libtool::build_libtool_from_source;

/// Determines the installation prefix for fallback builds.
pub fn get_fallback_install_prefix(tool_name: &str, config: &Config) -> Result<PathBuf> {
    let fallback_dir = config.cache_dir.join("fallback_builds").join(tool_name);
    log::info!("[Fallback] Determined install prefix for {}: {}", tool_name, fallback_dir.display());
    if let Some(parent) = fallback_dir.parent() {
         if !parent.exists() {
             log::debug!("[Fallback] Creating parent install directory: {}", parent.display());
             std::fs::create_dir_all(parent).map_err(SapphireError::Io)?;
         }
    } else {
         return Err(SapphireError::Generic(format!("Invalid fallback install prefix configuration for {}", tool_name)));
    }
    Ok(fallback_dir)
}

/// Shared function to download source tarball for fallback builds.
pub(crate) async fn download_fallback_source(url: &str, tool_name: &str, config: &Config) -> Result<PathBuf> {
    log::info!("[Fallback] Attempting download for {} from {}", tool_name, url);
    let cache_dir = config.cache_dir.join("fallback_source");
    if !cache_dir.exists() {
        log::debug!("[Fallback] Creating fallback source cache: {}", cache_dir.display());
        std::fs::create_dir_all(&cache_dir).map_err(SapphireError::Io)?;
    }

    let filename = url.split('/').last()
        .ok_or_else(|| SapphireError::Generic(format!("Could not extract filename from URL: {}", url)))?;
    let target_path = cache_dir.join(format!("{}-{}", tool_name, filename));

    if target_path.exists() {
        log::info!("[Fallback] Using cached source: {}", target_path.display());
        return Ok(target_path);
    }

    log::info!("[Fallback] Downloading {} to {}", url, target_path.display());
    let client = reqwest::Client::builder()
         .user_agent(format!("sapphire-package-manager/fallback-downloader-0.1"))
         .build()
         .map_err(|e| SapphireError::Http(e))?;

    let response = client.get(url).send().await.map_err(SapphireError::Http)?;

    if !response.status().is_success() {
        let status = response.status();
        let body_text = response.text().await.unwrap_or_else(|e| format!("(Failed to read response body: {})", e));
        log::error!("[Fallback] Download failed for {} ({}) from {}: {}", tool_name, status, url, body_text);
        return Err(SapphireError::DownloadError( // Use specific error
             tool_name.to_string(), url.to_string(), format!("HTTP Status {}", status)
        ));
    }

    let content = response.bytes().await.map_err(SapphireError::Http)?;
    let temp_path = target_path.with_extension("download_tmp");
    {
        let mut temp_file = std::fs::File::create(&temp_path).map_err(SapphireError::Io)?;
        std::io::copy(&mut std::io::Cursor::new(content), &mut temp_file).map_err(SapphireError::Io)?;
    }
    std::fs::rename(&temp_path, &target_path).map_err(SapphireError::Io)?;

    log::info!("[Fallback] Downloaded fallback source for {} successfully.", tool_name);
    Ok(target_path)
}

/// Shared function to execute a build script provided as content string.
/// Writes the content to a temporary file, executes it, and cleans up.
pub(crate) fn run_build_script_from_content(
    script_content: &str,
    script_name_hint: &str, // For logging and temp file naming (e.g., "build_m4.sh")
    tool_name: &str,        // For logging (e.g., "m4")
    config: &Config,        // Needed for temp file location
    args: &[String],        // Arguments to pass to the script
) -> Result<()> {
    // 1. Create a temporary executable file in the cache dir
    let temp_scripts_dir = config.cache_dir.join("temp_scripts");
    std::fs::create_dir_all(&temp_scripts_dir).map_err(SapphireError::Io)?;

    // *** Corrected temp file creation using Builder ***
    let mut temp_script_file = TempFileBuilder::new()
        .prefix(script_name_hint) // Use script_name_hint for prefix
        .suffix(".sh")
        .rand_bytes(5) // Add random bytes to name
        .tempfile_in(&temp_scripts_dir) // Create in specified directory
        .map_err(|e| SapphireError::Io(e))?;

    // 2. Write the script content to the temp file
    temp_script_file.write_all(script_content.as_bytes()).map_err(SapphireError::Io)?;
    temp_script_file.flush().map_err(SapphireError::Io)?; // Ensure content is written

    let temp_script_path = temp_script_file.path().to_path_buf(); // Get path before closing

    // 3. Make the temporary script executable (Unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&temp_script_path)?; // Re-fetch metadata after writing
        let mut perms = metadata.permissions();
        perms.set_mode(0o755); // rwxr-xr-x
        std::fs::set_permissions(&temp_script_path, perms)?;
        log::debug!("[Fallback] Set temp script {} executable.", temp_script_path.display());
    }

    // The NamedTempFile will be automatically deleted when `temp_script_file` goes out of scope.
    // We keep it alive until after the command execution finishes.

    // 4. Prepare the command
    let mut cmd = Command::new("sh"); // Assume scripts are sh compatible
    cmd.arg(&temp_script_path); // Arg 0: the script itself
    cmd.args(args); // Add the other arguments passed in

    log::info!("[Fallback] Running temp script {} for {}: {:?}", temp_script_path.display(), tool_name, cmd);

    // 5. Execute the command
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute temp build script {} for {}: {}", temp_script_path.display(), tool_name, e)))?;

    // Log output
    if !output.stdout.is_empty() {
        log::debug!("[Fallback] Temp script {} stdout:\n{}", script_name_hint, String::from_utf8_lossy(&output.stdout));
    }
     if !output.stderr.is_empty() {
        log::debug!("[Fallback] Temp script {} stderr:\n{}", script_name_hint, String::from_utf8_lossy(&output.stderr));
     }

    // 6. Check status
    if !output.status.success() {
        log::error!("[Fallback] Temp build script {} failed for {} with status: {}", script_name_hint, tool_name, output.status);
        let stderr_msg = String::from_utf8_lossy(&output.stderr);
        let error_detail = if stderr_msg.trim().is_empty() {
            format!("Script exited with status {}", output.status)
        } else {
            format!("Script exited with status {}. Stderr:\n{}", output.status, stderr_msg)
        };
        // Explicitly drop the temp file before returning the error, ensuring cleanup
        drop(temp_script_file);
        return Err(SapphireError::InstallError(format!(
            "Build script {} failed for {}. {}", script_name_hint, tool_name, error_detail
        )));
    }

    // 7. Explicitly drop the temp file after successful execution
    // (although it would be dropped automatically at scope end anyway)
    drop(temp_script_file);

    log::info!("[Fallback] Temp script {} for {} completed successfully.", script_name_hint, tool_name);
    Ok(())
}


