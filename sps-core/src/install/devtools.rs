use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use sps_common::error::{Result, SpsError};
use tracing::debug;
use which;

pub fn find_compiler(name: &str) -> Result<PathBuf> {
    let env_var_name = match name {
        "cc" => "CC",
        "c++" | "cxx" => "CXX",
        _ => "",
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

    if cfg!(target_os = "macos") {
        debug!("Attempting to find '{name}' using xcrun");
        let output = Command::new("xcrun")
            .arg("--find")
            .arg(name)
            .stderr(Stdio::piped())
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
                let stderr = String::from_utf8_lossy(&out.stderr);
                debug!("xcrun failed to find '{}': {}", name, stderr.trim());
            }
            Err(e) => {
                debug!("Failed to execute xcrun: {e}. Falling back to PATH search.");
            }
        }
    }

    debug!("Falling back to searching PATH for '{name}'");
    which::which(name).map_err(|e| {
        SpsError::BuildEnvError(format!("Failed to find compiler '{name}' on PATH: {e}"))
    })
}

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
                    return Err(SpsError::BuildEnvError(
                        "xcrun returned empty or invalid SDK path. Is Xcode or Command Line Tools installed correctly?".to_string()
                    ));
                }
                let sdk_path = PathBuf::from(path_str);
                if !sdk_path.exists() {
                    return Err(SpsError::BuildEnvError(format!(
                        "SDK path reported by xcrun does not exist: {}",
                        sdk_path.display()
                    )));
                }
                debug!("Found SDK path: {}", sdk_path.display());
                Ok(sdk_path)
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Err(SpsError::BuildEnvError(format!(
                    "xcrun failed to find SDK path: {}",
                    stderr.trim()
                )))
            }
            Err(e) => {
                Err(SpsError::BuildEnvError(format!(
                    "Failed to execute 'xcrun --show-sdk-path': {e}. Is Xcode or Command Line Tools installed?"
                )))
            }
        }
    } else {
        debug!("Not on macOS, returning '/' as SDK path placeholder");
        Ok(PathBuf::from("/"))
    }
}

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
                let version_parts: Vec<&str> = version_full.split('.').collect();
                let version_short = if version_parts.len() >= 2 {
                    format!("{}.{}", version_parts[0], version_parts[1])
                } else {
                    version_full.clone()
                };
                debug!("Found macOS version: {version_full} (short: {version_short})");
                Ok(version_short)
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Err(SpsError::BuildEnvError(format!(
                    "sw_vers failed to get product version: {}",
                    stderr.trim()
                )))
            }
            Err(e) => Err(SpsError::BuildEnvError(format!(
                "Failed to execute 'sw_vers -productVersion': {e}"
            ))),
        }
    } else {
        debug!("Not on macOS, returning '0.0' as version placeholder");
        Ok(String::from("0.0"))
    }
}

pub fn get_arch_flag() -> String {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "x86_64") {
            debug!("Detected target arch: x86_64");
            "-arch x86_64".to_string()
        } else if cfg!(target_arch = "aarch64") {
            debug!("Detected target arch: aarch64 (arm64)");
            "-arch arm64".to_string()
        } else {
            let arch = env::consts::ARCH;
            debug!(
                "Unknown target architecture on macOS: {arch}, cannot determine -arch flag. Build might fail."
            );
            // Provide no flag in this unknown case? Or default to native?
            String::new()
        }
    } else {
        debug!("Not on macOS, returning empty arch flag.");
        String::new()
    }
}
