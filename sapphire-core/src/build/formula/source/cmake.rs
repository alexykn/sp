// sapphire-core/src/build/formula/source/cmake.rs

use crate::build::env::BuildEnvironment;
use crate::utils::error::{Result, SapphireError};
use tracing::{debug, info};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Build with CMake, using Ninja as the generator
pub fn cmake_build(
    source_dir_for_build: &Path, // Renamed for clarity - path containing main CMakeLists.txt
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Building with CMake");
    let build_subdir_name = "sapphire-cmake-build";
    // Create the build directory *outside* the source_dir_for_build, typically in the CWD
    // which is the root build_dir set by the caller (build_from_source)
    let build_subdir = Path::new(".").join(build_subdir_name); // Relative to CWD
    fs::create_dir_all(&build_subdir).map_err(|e| SapphireError::Io(e))?;

    let cmake_exe = which::which_in("cmake", build_env.get_path_string(), Path::new(".")) // Check CWD and PATH
        .map_err(|_| {
            SapphireError::BuildEnvError(
                "cmake command not found in build environment PATH.".to_string(),
            )
        })?;
    info!(
        "==> Running cmake configuration in {}",
        build_subdir.display()
    );

    let mut cmd = Command::new(cmake_exe);
    // Pass the potentially nested source dir containing the main CMakeLists.txt
    cmd.arg(source_dir_for_build)
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", install_dir.display()))
        .arg("-DCMAKE_POLICY_VERSION_MINIMUM=3.5") // Keep this for compatibility
        .arg("-DCMAKE_BUILD_TYPE=Release") // *** Add recommended build type ***
        .args(&[
            "-G",
            "Ninja", // *** Specify Ninja generator ***
            "-DCMAKE_FIND_FRAMEWORK=LAST",
            "-DCMAKE_VERBOSE_MAKEFILE=ON", // Verbose output helpful for debugging
            "-Wno-dev",                    // Suppress developer warnings if needed
        ])
        // Run from the newly created build subdir
        .current_dir(&build_subdir);

    build_env.apply_to_command(&mut cmd);
    let output = cmd
        .output()
        .map_err(|e| SapphireError::CommandExecError(format!("Failed to execute cmake: {}", e)))?;

    if !output.status.success() {
        println!("CMake configure failed with status: {}", output.status);
        eprintln!(
            "CMake configure stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "CMake configure stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Attempt to read CMakeCache.txt for more clues on failure
        let cache_log = build_subdir.join("CMakeCache.txt");
        if cache_log.exists() {
            eprintln!("--- CMakeCache.txt ---");
            if let Ok(content) = fs::read_to_string(&cache_log) {
                eprintln!("{}", content);
            }
            eprintln!("--- End CMakeCache.txt ---");
        }
        let error_log = build_subdir.join("CMakeFiles/CMakeError.log");
        if error_log.exists() {
            eprintln!("--- CMakeFiles/CMakeError.log ---");
            if let Ok(content) = fs::read_to_string(&error_log) {
                eprintln!("{}", content);
            }
            eprintln!("--- End CMakeFiles/CMakeError.log ---");
        }
        return Err(SapphireError::Generic(format!(
            "CMake configure failed with status: {}",
            output.status
        )));
    } else {
        // Log stdout/stderr even on success if debug logging is high enough
        debug!(
            "CMake configure stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        debug!(
            "CMake configure stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // --- Use ninja for install step ---
    info!("==> Running ninja install in {}", build_subdir.display());
    let ninja_exe = which::which_in("ninja", build_env.get_path_string(), Path::new(".")) // Find ninja
        .map_err(|_| {
            SapphireError::BuildEnvError(
                "ninja command not found in build environment PATH (required for CMake build)."
                    .to_string(),
            )
        })?;

    let mut cmd_install = Command::new(ninja_exe); // Use ninja
    cmd_install.arg("install").current_dir(&build_subdir); // Run 'ninja install' from build subdir

    build_env.apply_to_command(&mut cmd_install);
    let output_install = cmd_install.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute ninja install (CMake): {}", e))
    })?;

    if !output_install.status.success() {
        println!(
            "Ninja install failed with status: {}",
            output_install.status
        );
        eprintln!(
            "Ninja install stdout:\n{}",
            String::from_utf8_lossy(&output_install.stdout)
        );
        eprintln!(
            "Ninja install stderr:\n{}",
            String::from_utf8_lossy(&output_install.stderr)
        );
        return Err(SapphireError::Generic(format!(
            "Ninja install failed with status: {}",
            output_install.status
        )));
    } else {
        debug!("Ninja install completed successfully.");
    }

    Ok(())
}