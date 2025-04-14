use crate::utils::error::{SapphireError, Result};
use crate::build::env::BuildEnvironment;
use std::path::Path;
use std::fs;
use std::process::Command;
use log::{debug, info, warn};

/// Configure and build with autotools (./configure && make && make install)
pub fn configure_and_make(
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Running ./configure --prefix={}", install_dir.display());

    let mut cmd = Command::new("./configure");
    cmd.arg(format!("--prefix={}", install_dir.display()));
    cmd.args(&["--disable-dependency-tracking", "--disable-silent-rules"]);
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute configure: {}", e)))?;

    if !output.status.success() {
        println!("Configure failed with status: {}", output.status);
        eprintln!("Configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        let config_log_path = std::path::PathBuf::from("config.log");
        if config_log_path.exists() {
            eprintln!("--- Last 50 lines of config.log ---");
            if let Ok(content) = fs::read_to_string(&config_log_path) {
                for line in content.lines().rev().take(50).collect::<Vec<_>>().iter().rev() {
                    eprintln!("{}", line);
                }
            }
            eprintln!("--- End config.log ---");
        }
        return Err(SapphireError::Generic(format!(
            "Configure failed with status: {}", output.status
        )));
    } else {
        debug!("Configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        debug!("Configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    }

    // Run make
    info!("==> Running make");
    let make_exe = which::which_in("make", build_env.get_path_string(), Path::new("."))
        .map_err(|_| SapphireError::BuildEnvError("make command not found in build environment PATH.".to_string()))?;
    let mut cmd = Command::new(make_exe.clone());
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make: {}", e)))?;

    if !output.status.success() {
        println!("Make failed with status: {}", output.status);
        eprintln!("Make stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Make stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(format!(
            "Make failed with status: {}", output.status
        )));
    } else {
        debug!("Make completed successfully.");
    }

    // Run make install
    info!("==> Running make install");
    let mut cmd = Command::new(make_exe);
    cmd.arg("install");
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make install: {}", e)))?;

    if !output.status.success() {
        println!("Make install failed with status: {}", output.status);
        eprintln!("Make install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Make install stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(format!(
            "Make install failed with status: {}", output.status
        )));
    } else {
        debug!("Make install completed successfully.");
    }

    Ok(())
}

/// Build with a simple Makefile
pub fn simple_make(
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Building with simple Makefile");
    let make_exe = which::which_in("make", build_env.get_path_string(), Path::new("."))
        .map_err(|_| SapphireError::BuildEnvError("make command not found in build environment PATH.".to_string()))?;

    info!("==> Running make");
    let mut cmd_make = Command::new(make_exe.clone());
    build_env.apply_to_command(&mut cmd_make);
    let output_make = cmd_make.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make (simple): {}", e)))?;

    if !output_make.status.success() {
        println!("Make failed with status: {}", output_make.status);
        eprintln!("Make stdout:\n{}", String::from_utf8_lossy(&output_make.stdout));
        eprintln!("Make stderr:\n{}", String::from_utf8_lossy(&output_make.stderr));
        return Err(SapphireError::Generic(format!(
            "Make failed with status: {}", output_make.status
        )));
    } else {
        info!("Make completed successfully.");
    }

    info!("==> Running make install PREFIX={}", install_dir.display());
    let mut cmd_install = Command::new(make_exe);
    cmd_install.arg("install");
    cmd_install.arg(format!("PREFIX={}", install_dir.display()));
    build_env.apply_to_command(&mut cmd_install);
    let output_install = cmd_install.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make install (simple): {}", e)))?;

    if !output_install.status.success() {
        warn!("'make install' failed with status {}. Formula might be installed correctly if it doesn't use a standard install target or PREFIX variable.", output_install.status);
        let bin_dir = install_dir.join("bin");
        let lib_dir = install_dir.join("lib");
        let bin_exists = bin_dir.exists() && fs::read_dir(&bin_dir).map(|mut d| d.next().is_some()).unwrap_or(false);
        let lib_exists = lib_dir.exists() && fs::read_dir(&lib_dir).map(|mut d| d.next().is_some()).unwrap_or(false);
        if !bin_exists && !lib_exists {
            println!("Make install failed with status: {} and no files found in {}/bin or {}/lib",
                output_install.status, install_dir.display(), install_dir.display());
            eprintln!("Make install stdout:\n{}", String::from_utf8_lossy(&output_install.stdout));
            eprintln!("Make install stderr:\n{}", String::from_utf8_lossy(&output_install.stderr));
            return Err(SapphireError::Generic(format!(
                "Make install failed with status: {} and no files found in relevant install directories",
                output_install.status
            )));
        } else {
            info!("Proceeding despite 'make install' error as installation directory seems populated.");
        }
    } else {
        info!("Make install completed successfully.");
    }

    Ok(())
}