use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{debug, info};

use crate::build::env::BuildEnvironment;
use crate::utils::error::{Result, SapphireError};

/// Build Go project using `go build`. This is intended for Go modules.
pub fn go_build(
    build_dir_dot: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
    _all_installed_paths: &[PathBuf],
) -> Result<()> {
    if build_dir_dot != Path::new(".") {
        return Err(SapphireError::BuildEnvError(format!(
            "go_build expected build path '.' but received '{}'",
            build_dir_dot.display()
        )));
    }

    info!("==> Building Go module (go.mod detected)");

    // Find the 'go' executable using the build environment's PATH
    let go_exe =
        which::which_in("go", build_env.get_path_string(), Path::new(".")).map_err(|_| {
            SapphireError::BuildEnvError(
                "go command not found in build environment PATH.".to_string(),
            )
        })?;

    // Determine the formula name for the output binary.
    // We infer this from the install_dir structure e.g., /Cellar/doggo/1.0.5 -> "doggo"
    let formula_name = install_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            SapphireError::BuildEnvError(format!(
                "Could not infer formula name from install path: {}",
                install_dir.display()
            ))
        })?;

    // Ensure the target bin directory exists inside the install_dir
    let target_bin_dir = install_dir.join("bin");
    fs::create_dir_all(&target_bin_dir).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to create target bin directory {}: {}",
                target_bin_dir.display(),
                e
            ),
        ))
    })?;

    // Define the output path for the binary
    let output_binary_path = target_bin_dir.join(formula_name);

    info!(
        "==> Running: {} build -o {} -ldflags \"-s -w\" .", // Log formatting unchanged
        go_exe.display(),
        output_binary_path.display()
    );

    let mut cmd = Command::new(go_exe);
    cmd.arg("build");
    cmd.arg("-o");
    cmd.arg(&output_binary_path); // Output directly to install bin dir
    cmd.arg("-ldflags"); // Pass the flag name as one argument
    #[allow(clippy::suspicious_command_arg_space)] 
    cmd.arg("-s -w"); // Pass the flag value as the next argument
                      // --- END CORRECTION ---

    // Might need to parse build_env for specific GOFLAGS or CGO flags here if needed
    cmd.arg("."); // Build the package in the current directory

    // Apply the sanitized build environment (PATH, GOPATH, GOBIN etc. if set)
    build_env.apply_to_command(&mut cmd);

    // Execute the build command
    let output = cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute go build: {}", e))
    })?;

    if !output.status.success() {
        println!("Go build failed with status: {}", output.status);
        eprintln!(
            "Go build stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "Go build stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Attempt to clean up the potentially partially created binary
        let _ = fs::remove_file(&output_binary_path);
        return Err(SapphireError::Generic(format!(
            "Go build failed with status: {}",
            output.status
        )));
    } else {
        // Log output even on success if tracing level is high enough
        debug!(
            "Go build stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        debug!(
            "Go build stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        info!(
            "Go build successful, binary placed at: {}",
            output_binary_path.display()
        );
    }

    // No artifact copying needed as we built directly into the target location.

    Ok(())
}
