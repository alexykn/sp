use std::path::Path;
use std::process::Command;

use tracing::{debug, info};

use crate::build::env::BuildEnvironment;
use crate::utils::error::{Result, SapphireError};

/// Build with Cargo
pub fn cargo_build(install_dir: &Path, build_env: &BuildEnvironment) -> Result<()> {
    info!("==> Building with Cargo");
    let cargo_exe =
        which::which_in("cargo", build_env.get_path_string(), Path::new(".")).map_err(|_| {
            SapphireError::BuildEnvError(
                "cargo command not found in build environment PATH.".to_string(),
            )
        })?;

    info!(
        "==> Running {} install --path . --root {}",
        cargo_exe.display(),
        install_dir.display()
    );
    let mut cmd = Command::new(cargo_exe);
    cmd.arg("install")
        .arg("--path")
        .arg(".")
        .arg("--root")
        .arg(install_dir);
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute cargo install: {}", e))
    })?;

    if !output.status.success() {
        println!("Cargo install failed with status: {}", output.status);
        eprintln!(
            "Cargo install stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "Cargo install stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(SapphireError::Generic(format!(
            "Cargo install failed with status: {}",
            output.status
        )));
    } else {
        debug!("Cargo install completed successfully.");
    }

    Ok(())
}
