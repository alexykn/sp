use crate::utils::error::{SapphireError, Result};
use crate::build::env::BuildEnvironment;
use std::path::Path;
use std::fs;
use std::process::Command;
use log::{debug, info};

/// Build with CMake
pub fn cmake_build(
    source_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Building with CMake");
    let build_subdir_name = "sapphire-cmake-build";
    let build_subdir = source_dir.join(build_subdir_name);
    fs::create_dir_all(&build_subdir).map_err(|e| SapphireError::Io(e))?;

    let cmake_exe = which::which_in("cmake", build_env.get_path_string(), Path::new("."))
        .map_err(|_| SapphireError::BuildEnvError("cmake command not found in build environment PATH.".to_string()))?;
    info!("==> Running cmake configuration in {}", build_subdir.display());

    let mut cmd = Command::new(cmake_exe);
    cmd.arg("..")
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", install_dir.display()))
        .args(&[
            "-DCMAKE_FIND_FRAMEWORK=LAST",
            "-DCMAKE_VERBOSE_MAKEFILE=ON",
            "-Wno-dev",
        ])
        .current_dir(&build_subdir);
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute cmake: {}", e)))?;

    if !output.status.success() {
        println!("CMake configure failed with status: {}", output.status);
        eprintln!("CMake configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("CMake configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(format!(
            "CMake configure failed with status: {}", output.status
        )));
    } else {
        debug!("CMake configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        debug!("CMake configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    }

    info!("==> Running make install in {}", build_subdir.display());
    let make_exe = which::which_in("make", build_env.get_path_string(), Path::new("."))
        .map_err(|_| SapphireError::BuildEnvError("make command not found in build environment PATH.".to_string()))?;

    let mut cmd = Command::new(make_exe);
    cmd.arg("install")
        .current_dir(&build_subdir);
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make install (CMake): {}", e)))?;

    if !output.status.success() {
        println!("CMake make install failed with status: {}", output.status);
        eprintln!("CMake make install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("CMake make install stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(format!(
            "CMake make install failed with status: {}", output.status
        )));
    } else {
        debug!("CMake install completed successfully.");
    }

    Ok(())
}