// FILE: sapphire-core/src/build/formula/source/cmake.rs

use std::fs;
use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::env::BuildEnvironment;
use crate::build::formula::source::run_command_in_dir;
use crate::utils::error::{Result, SapphireError};

pub fn cmake_build(
    source_subdir: &Path,
    build_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    debug!("Building with CMake in {}", build_dir.display());
    let cmake_build_subdir_name = "sapphire-cmake-build";
    let cmake_build_dir = build_dir.join(cmake_build_subdir_name);
    fs::create_dir_all(&cmake_build_dir).map_err(SapphireError::Io)?;

    let cmake_exe =
        which::which_in("cmake", build_env.get_path_string(), build_dir).map_err(|_| {
            SapphireError::BuildEnvError(
                "cmake command not found in build environment PATH.".to_string(),
            )
        })?;

    debug!(
        "Running cmake configuration (source: {}, build: {})",
        build_dir.join(source_subdir).display(),
        cmake_build_dir.display()
    );

    let mut cmd_configure = Command::new(cmake_exe);
    cmd_configure
        .arg(build_dir.join(source_subdir))
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", install_dir.display()))
        .arg("-DCMAKE_POLICY_VERSION_MINIMUM=3.5")
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .args([
            "-G",
            "Ninja",
            "-DCMAKE_FIND_FRAMEWORK=LAST",
            "-DCMAKE_VERBOSE_MAKEFILE=ON",
            "-Wno-dev",
        ]);

    let configure_output = run_command_in_dir(
        &mut cmd_configure,
        &cmake_build_dir,
        build_env,
        "cmake configure",
    )?;
    debug!(
        "CMake configure stdout:\n{}",
        String::from_utf8_lossy(&configure_output.stdout)
    );
    debug!(
        "CMake configure stderr:\n{}",
        String::from_utf8_lossy(&configure_output.stderr)
    );

    debug!("Running ninja install in {}", cmake_build_dir.display());
    let ninja_exe = which::which_in("ninja", build_env.get_path_string(), &cmake_build_dir)
        .map_err(|_| {
            SapphireError::BuildEnvError(
                "ninja command not found in build environment PATH.".to_string(),
            )
        })?;

    let mut cmd_install = Command::new(ninja_exe);
    cmd_install.arg("install");

    run_command_in_dir(
        &mut cmd_install,
        &cmake_build_dir,
        build_env,
        "ninja install",
    )?;
    debug!("Ninja install completed successfully.");

    Ok(())
}
