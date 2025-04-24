// FILE: sapphire-core/src/build/formula/source/go.rs

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::debug;

use crate::build::env::BuildEnvironment;
use crate::build::formula::source::run_command_in_dir;
use crate::utils::error::{Result, SapphireError};

pub fn go_build(
    source_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
    _all_installed_paths: &[PathBuf], // Keep if needed later, currently unused
) -> Result<()> {
    debug!("Building Go module in {}", source_dir.display());

    let go_exe = which::which_in("go", build_env.get_path_string(), source_dir).map_err(|_| {
        SapphireError::BuildEnvError("go command not found in build environment PATH.".to_string())
    })?;

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

    let cmd_pkg_path = source_dir.join("cmd").join(formula_name);
    let package_to_build = if cmd_pkg_path.is_dir() {
        debug!(
            "Found potential command package path: {}",
            cmd_pkg_path.display()
        );
        format!("./cmd/{formula_name}") // Relative to source_dir
    } else {
        debug!("Command package path not found, building '.'");
        ".".to_string()
    };

    let target_bin_dir = install_dir.join("bin");
    fs::create_dir_all(&target_bin_dir).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed create target bin dir: {e}"),
        ))
    })?;
    let output_binary_path = target_bin_dir.join(formula_name);

    debug!(
        "Running: go build -o {} -ldflags \"-s -w\" {}",
        output_binary_path.display(),
        package_to_build
    );

    let mut cmd = Command::new(go_exe);
    #[allow(clippy::suspicious_command_arg_space)]
    cmd.arg("build")
        .arg("-o")
        .arg(&output_binary_path)
        .arg("-ldflags")
        .arg("-s -w")
        .arg(&package_to_build);

    let build_output = run_command_in_dir(&mut cmd, source_dir, build_env, "go build")?;

    debug!(
        "Go build stdout:\n{}",
        String::from_utf8_lossy(&build_output.stdout)
    );
    debug!(
        "Go build stderr:\n{}",
        String::from_utf8_lossy(&build_output.stderr)
    );
    debug!(
        "Go build successful, binary placed at: {}",
        output_binary_path.display()
    );

    Ok(())
}
