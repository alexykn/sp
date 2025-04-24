// FILE: sapphire-core/src/build/formula/source/python.rs

use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::env::BuildEnvironment;
use crate::build::formula::source::run_command_in_dir; // Ensure helper is imported if used
use crate::utils::error::{Result, SapphireError};

// Corrected signature: Added source_dir argument
pub fn python_build(
    source_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    debug!("Building with Python setup.py in {}", source_dir.display());
    let python_exe = which::which_in("python3", build_env.get_path_string(), source_dir)
        .or_else(|_| which::which_in("python", build_env.get_path_string(), source_dir))
        .map_err(|_| {
            SapphireError::BuildEnvError("python3 or python command not found.".to_string())
        })?;

    debug!(
        "Running {} setup.py install --prefix={}",
        python_exe.display(),
        install_dir.display()
    );
    let mut cmd = Command::new(python_exe);
    cmd.arg("setup.py")
        .arg("install")
        .arg(format!("--prefix={}", install_dir.display()));

    // Use the helper function to run the command in the correct directory
    run_command_in_dir(&mut cmd, source_dir, build_env, "python setup.py install")?;
    debug!("Python install completed successfully.");

    Ok(())
}
