use crate::build::env::BuildEnvironment;
use crate::utils::error::{Result, SapphireError};
use log::{debug, info};
use std::path::Path;
use std::process::Command;

/// Build with Python setup.py
pub fn python_build(install_dir: &Path, build_env: &BuildEnvironment) -> Result<()> {
    info!("==> Building with Python setup.py");
    let python_exe = which::which_in("python3", build_env.get_path_string(), Path::new("."))
        .or_else(|_| which::which_in("python", build_env.get_path_string(), Path::new(".")))
        .map_err(|_| {
            SapphireError::BuildEnvError(
                "python3 or python command not found in build environment PATH.".to_string(),
            )
        })?;

    info!(
        "==> Running {} setup.py install --prefix={}",
        python_exe.display(),
        install_dir.display()
    );
    let mut cmd = Command::new(python_exe);
    cmd.arg("setup.py")
        .arg("install")
        .arg(format!("--prefix={}", install_dir.display()));
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute python setup.py install: {}", e))
    })?;

    if !output.status.success() {
        println!(
            "Python setup.py install failed with status: {}",
            output.status
        );
        eprintln!(
            "Python install stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "Python install stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(SapphireError::Generic(format!(
            "Python setup.py install failed with status: {}",
            output.status
        )));
    } else {
        debug!("Python install completed successfully.");
    }

    Ok(())
}
