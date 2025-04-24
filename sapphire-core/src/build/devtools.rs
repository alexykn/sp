// **File:** sapphire-core/src/build/devtools.rs (New file)
use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use tracing::debug;
use which;

use crate::utils::error::{Result, SapphireError};
/// Finds the path to the specified compiler executable (e.g., "cc", "c++").
///
/// Tries environment variables (e.g., `CC`, `CXX`) first, then `xcrun` on macOS,
/// then falls back to searching the system `PATH`.
pub fn find_compiler(name: &str) -> Result<PathBuf> {
    // 1. Check environment variables (CC for "cc", CXX for "c++")
    let env_var_name = match name {
        "cc" => "CC",
        "c++" | "cxx" => "CXX",
        _ => "", // Only handle common cases for now
    };
    if !env_var_name.is_empty() {
        if let Ok(compiler_path) = env::var(env_var_name) {
            let path = PathBuf::from(compiler_path);
            if path.is_file() {
                debug!(
                    "Using compiler from env var {}: {}",
                    env_var_name,
                    path.display()
                );
                return Ok(path);
            } else {
                debug!(
                    "Env var {} points to non-existent file: {}",
                    env_var_name,
                    path.display()
                );
            }
        }
    }

    // 2. Use xcrun on macOS (if available)
    if cfg!(target_os = "macos") {
        debug!("Attempting to find '{name}' using xcrun");
        let output = Command::new("xcrun")
            .arg("--find")
            .arg(name)
            .stderr(Stdio::piped()) // Capture stderr for better error messages
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let path_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !path_str.is_empty() {
                    let path = PathBuf::from(path_str);
                    if path.is_file() {
                        debug!("Found compiler via xcrun: {}", path.display());
                        return Ok(path);
                    } else {
                        debug!(
                            "xcrun found '{}' but path doesn't exist or isn't a file: {}",
                            name,
                            path.display()
                        );
                    }
                } else {
                    debug!("xcrun found '{name}' but returned empty path.");
                }
            }
            Ok(out) => {
                // xcrun ran but failed
                let stderr = String::from_utf8_lossy(&out.stderr);
                // Don't treat xcrun failure as fatal, just means it couldn't find it this way
                debug!("xcrun failed to find '{}': {}", name, stderr.trim());
            }
            Err(e) => {
                // xcrun command itself failed to execute (likely not installed or not in PATH)
                debug!("Failed to execute xcrun: {e}. Falling back to PATH search.");
            }
        }
    }

    // 3. Fallback to searching PATH
    debug!("Falling back to searching PATH for '{name}'");
    which::which(name).map_err(|e| {
        SapphireError::BuildEnvError(format!("Failed to find compiler '{name}' on PATH: {e}"))
    })
}

/// Finds the path to the active macOS SDK.
/// Returns "/" on non-macOS platforms or if detection fails.
pub fn find_sdk_path() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        debug!("Attempting to find macOS SDK path using xcrun");
        let output = Command::new("xcrun")
            .arg("--show-sdk-path")
            .stderr(Stdio::piped())
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let path_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if path_str.is_empty() || path_str == "/" {
                    debug!("xcrun returned empty or invalid SDK path ('{path_str}'). Check Xcode/CLT installation.");
                    // Fallback or error? Homebrew errors here. Let's error.
                    return Err(SapphireError::BuildEnvError(
                        "xcrun returned empty or invalid SDK path. Is Xcode or Command Line Tools installed correctly?".to_string()
                    ));
                }
                let sdk_path = PathBuf::from(path_str);
                if !sdk_path.exists() {
                    return Err(SapphireError::BuildEnvError(format!(
                        "SDK path reported by xcrun does not exist: {}",
                        sdk_path.display()
                    )));
                }
                debug!("Found SDK path: {}", sdk_path.display());
                Ok(sdk_path)
            }
            Ok(out) => {
                // xcrun ran but failed
                let stderr = String::from_utf8_lossy(&out.stderr);
                Err(SapphireError::BuildEnvError(format!(
                    "xcrun failed to find SDK path: {}",
                    stderr.trim()
                )))
            }
            Err(e) => {
                // xcrun command itself failed to execute
                Err(SapphireError::BuildEnvError(format!(
                    "Failed to execute 'xcrun --show-sdk-path': {e}. Is Xcode or Command Line Tools installed?"
                )))
            }
        }
    } else {
        // No SDK concept in this way on Linux/other platforms usually
        debug!("Not on macOS, returning '/' as SDK path placeholder");
        Ok(PathBuf::from("/"))
    }
}

/// Gets the macOS product version string (e.g., "14.4").
/// Returns "0.0" on non-macOS platforms.
pub fn get_macos_version() -> Result<String> {
    if cfg!(target_os = "macos") {
        debug!("Attempting to get macOS version using sw_vers");
        let output = Command::new("sw_vers")
            .arg("-productVersion")
            .stderr(Stdio::piped())
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let version_full = String::from_utf8_lossy(&out.stdout).trim().to_string();
                // Homebrew often uses major.minor, let's try to replicate that
                let version_parts: Vec<&str> = version_full.split('.').collect();
                let version_short = if version_parts.len() >= 2 {
                    format!("{}.{}", version_parts[0], version_parts[1])
                } else {
                    version_full.clone() // Fallback if format is unexpected
                };
                debug!("Found macOS version: {version_full} (short: {version_short})");
                Ok(version_short)
            }
            Ok(out) => {
                // sw_vers ran but failed
                let stderr = String::from_utf8_lossy(&out.stderr);
                Err(SapphireError::BuildEnvError(format!(
                    "sw_vers failed to get product version: {}",
                    stderr.trim()
                )))
            }
            Err(e) => {
                // sw_vers command itself failed to execute
                Err(SapphireError::BuildEnvError(format!(
                    "Failed to execute 'sw_vers -productVersion': {e}"
                )))
            }
        }
    } else {
        debug!("Not on macOS, returning '0.0' as version placeholder");
        Ok(String::from("0.0")) // Not applicable
    }
}

/// Gets the appropriate architecture flag (e.g., "-arch arm64") for the current build target.
pub fn get_arch_flag() -> String {
    if cfg!(target_os = "macos") {
        // On macOS, we explicitly use -arch flags
        if cfg!(target_arch = "x86_64") {
            debug!("Detected target arch: x86_64");
            "-arch x86_64".to_string()
        } else if cfg!(target_arch = "aarch64") {
            debug!("Detected target arch: aarch64 (arm64)");
            "-arch arm64".to_string()
        } else {
            let arch = env::consts::ARCH;
            debug!("Unknown target architecture on macOS: {arch}, cannot determine -arch flag. Build might fail.");
            // Provide no flag in this unknown case? Or default to native?
            // Homebrew might error or try native. Let's return empty for safety.
            String::new()
        }
    } else {
        // On Linux/other, -march=native is common but less portable for distribution.
        // Compilers usually target the host architecture by default without specific flags.
        // Let's return an empty string for non-macOS for now. Flags can be added later if needed.
        debug!("Not on macOS, returning empty arch flag.");
        String::new()
    }
}
