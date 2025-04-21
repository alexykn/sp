use crate::build::env::BuildEnvironment;
use crate::utils::error::{Result, SapphireError};
use tracing::{debug, info};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Build Perl using its Configure script
pub fn perl_build(
    _build_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Building with Perl Configure script...");

    let configure_script = PathBuf::from("Configure");
    if !configure_script.exists() {
        return Err(SapphireError::BuildEnvError(format!(
            "Perl Configure script not found at {}",
            configure_script.display()
        )));
    }

    let sh_exe =
        which::which_in("sh", build_env.get_path_string(), Path::new(".")).map_err(|_| {
            SapphireError::BuildEnvError(
                "sh command not found in build environment PATH (needed for Perl Configure)."
                    .to_string(),
            )
        })?;

    let mut cmd = Command::new(sh_exe);
    cmd.arg(configure_script);
    cmd.arg("-des");
    cmd.arg(format!("-Dprefix={}", install_dir.display()));

    build_env.apply_to_command(&mut cmd);
    info!("Running Perl Configure: {:?}", cmd);
    let output = cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute Perl Configure: {}", e))
    })?;

    if !output.status.success() {
        println!("Perl Configure failed with status: {}", output.status);
        eprintln!(
            "Perl Configure stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "Perl Configure stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(SapphireError::Generic(format!(
            "Perl Configure failed with status: {}",
            output.status
        )));
    } else {
        debug!(
            "Perl Configure stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        debug!(
            "Perl Configure stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Run make
    info!("==> Running make for Perl");
    let make_exe =
        which::which_in("make", build_env.get_path_string(), Path::new(".")).map_err(|_| {
            SapphireError::BuildEnvError(
                "make command not found in build environment PATH.".to_string(),
            )
        })?;
    let mut cmd = Command::new(make_exe.clone());
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute make for Perl: {}", e))
    })?;

    if !output.status.success() {
        println!("Perl make failed with status: {}", output.status);
        eprintln!(
            "Perl make stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "Perl make stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(SapphireError::Generic(format!(
            "Perl make failed with status: {}",
            output.status
        )));
    } else {
        info!("Perl make completed successfully.");
    }

    // Run make install
    info!("==> Running make install for Perl");
    let mut cmd = Command::new(make_exe);
    cmd.arg("install");
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute make install for Perl: {}", e))
    })?;

    if !output.status.success() {
        println!("Perl make install failed with status: {}", output.status);
        eprintln!(
            "Perl make install stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "Perl make install stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(SapphireError::Generic(format!(
            "Perl make install failed with status: {}",
            output.status
        )));
    } else {
        info!("Perl make install completed successfully.");
    }

    Ok(())
}