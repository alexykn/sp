use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{debug, info};

use crate::build::env::BuildEnvironment;
use crate::utils::error::{Result, SapphireError};

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

    let go_exe = which::which_in("go", build_env.get_path_string(), Path::new("."))
        .map_err(|_| SapphireError::BuildEnvError("go command not found in build environment PATH.".to_string()))?;

    let formula_name = install_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .ok_or_else(|| SapphireError::BuildEnvError(format!("Could not infer formula name from install path: {}", install_dir.display())))?;

    let cmd_pkg_path = Path::new("cmd").join(formula_name);
    let package_to_build = if cmd_pkg_path.is_dir() {
        debug!("Found potential command package path: {}", cmd_pkg_path.display());
        format!("./{}", cmd_pkg_path.to_string_lossy())
    } else {
        debug!("Command package path {} not found, building '.'", cmd_pkg_path.display());
        ".".to_string()
    };

    let target_bin_dir = install_dir.join("bin");
    fs::create_dir_all(&target_bin_dir).map_err(|e| {
        SapphireError::Io(std::io::Error::new(e.kind(),
            format!("Failed to create target bin directory {}: {}", target_bin_dir.display(), e)))
    })?;
    let output_binary_path = target_bin_dir.join(formula_name);


    info!(
        "==> Running: {} build -o {} -ldflags \"-s -w\" {}",
        go_exe.display(),
        output_binary_path.display(),
        package_to_build
    );

    let mut cmd = Command::new(go_exe);
    cmd.arg("build");
    cmd.arg("-o");
    cmd.arg(&output_binary_path);
    cmd.arg("-ldflags");
    #[allow(clippy::suspicious_command_arg_space)]
    cmd.arg("-s -w");
    cmd.arg(&package_to_build);

    build_env.apply_to_command(&mut cmd);

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
        let _ = fs::remove_file(&output_binary_path);
        return Err(SapphireError::Generic(format!(
            "Go build failed with status: {}",
            output.status
        )));
    } else {
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

    Ok(())
}
