use crate::utils::error::{SapphireError, Result};
use crate::build::env::BuildEnvironment;
use std::path::{Path, PathBuf};
use std::fs;
use std::process::Command;
use log::{debug, info, warn};

/// Build with Meson
pub fn meson_build(
    source_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Building with Meson");
    let build_subdir_name = "sapphire-meson-build";
    let build_subdir = source_dir.join(build_subdir_name);

    let meson_exe = which::which_in("meson", build_env.get_path_string(), Path::new("."))
        .map_err(|_| SapphireError::BuildEnvError("meson command not found in build environment PATH.".to_string()))?;
    info!("==> Running meson setup: {} setup --prefix={} {} {}",
        meson_exe.display(), install_dir.display(), build_subdir.display(), source_dir.display());

    let mut cmd_setup = Command::new(&meson_exe);
    cmd_setup.arg("setup")
        .arg(format!("--prefix={}", install_dir.display()))
        .arg("--buildtype=release")
        .arg("--libdir=lib")
        .arg(&build_subdir)
        .arg(".");
    build_env.apply_to_command(&mut cmd_setup);
    let output_setup = cmd_setup.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute meson setup: {}", e)))?;

    if !output_setup.status.success() {
        println!("Meson setup failed with status: {}", output_setup.status);
        eprintln!("Meson setup stdout:\n{}", String::from_utf8_lossy(&output_setup.stdout));
        eprintln!("Meson setup stderr:\n{}", String::from_utf8_lossy(&output_setup.stderr));
        return Err(SapphireError::Generic(format!(
            "Meson setup failed with status: {}", output_setup.status
        )));
    } else {
        debug!("Meson setup stdout:\n{}", String::from_utf8_lossy(&output_setup.stdout));
        debug!("Meson setup stderr:\n{}", String::from_utf8_lossy(&output_setup.stderr));
    }

    let _ninja_exe = which::which_in("ninja", build_env.get_path_string(), Path::new("."))
        .map_err(|_| SapphireError::BuildEnvError("ninja command not found (needed for meson install). Ensure ninja is installed and in build dependencies.".to_string()))?;
    info!("==> Running meson install -C {}", build_subdir.display());

    let mut cmd_install = Command::new(&meson_exe);
    cmd_install.arg("install")
        .arg("-C")
        .arg(&build_subdir);
    build_env.apply_to_command(&mut cmd_install);
    let output_install = cmd_install.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute meson install: {}", e)))?;

    if !output_install.status.success() {
        println!("Meson install failed with status: {}", output_install.status);
        eprintln!("Meson install stdout:\n{}", String::from_utf8_lossy(&output_install.stdout));
        eprintln!("Meson install stderr:\n{}", String::from_utf8_lossy(&output_install.stderr));
        return Err(SapphireError::Generic(format!(
            "Meson install failed with status: {}", output_install.status
        )));
    } else {
        debug!("Meson install completed successfully.");
    }

    Ok(())
}