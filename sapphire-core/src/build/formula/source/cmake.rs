// sapphire-core/src/build/formula/source/cmake.rs

use std::fs;
use std::path::Path;
use std::process::Command;

use tracing::{debug, info};

use crate::build::env::BuildEnvironment;
use crate::utils::error::{Result, SapphireError};

/// Build with CMake, using Ninja as the generator
pub fn cmake_build(
    source_dot: &Path, // Parameter represents "." now, as CWD is the source root
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    // Ensure source_dot is actually "."
    if source_dot != Path::new(".") {
        return Err(SapphireError::BuildEnvError(format!(
            "cmake_build expected source path '.' but received '{}'",
            source_dot.display()
        )));
    }

    info!("==> Building with CMake");
    let build_subdir_name = "sapphire-cmake-build";
    // Create the build directory *outside* the source, relative to CWD
    let build_subdir = Path::new(".").join(build_subdir_name); // Relative to CWD
    fs::create_dir_all(&build_subdir).map_err(SapphireError::Io)?;

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
    // Pass "." as the source directory relative to the CWD (which is the actual source root)
    cmd.arg(".") // <--- CHANGED FROM source_dir_for_build (which was previously passed here)
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", install_dir.display()))
        .arg("-DCMAKE_POLICY_VERSION_MINIMUM=3.5") // Keep this for compatibility
        .arg("-DCMAKE_BUILD_TYPE=Release") // Add recommended build type
        .args([
            "-G",
            "Ninja", // Specify Ninja generator
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
        // (Error handling code from your original file)
        println!("CMake configure failed with status: {}", output.status);
        eprintln!(
            "CMake configure stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "CMake configure stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
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
        // (Error handling code from your original file)
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
