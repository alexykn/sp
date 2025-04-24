// FILE: sapphire-core/src/build/formula/source/cargo.rs

use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::env::BuildEnvironment;
use crate::build::formula::source::run_command_in_dir;
use crate::utils::error::{Result, SapphireError};

pub fn cargo_build(
    source_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    debug!("Building with Cargo in {}", source_dir.display());
    let cargo_exe =
        which::which_in("cargo", build_env.get_path_string(), source_dir).map_err(|_| {
            SapphireError::BuildEnvError(
                "cargo command not found in build environment PATH.".to_string(),
            )
        })?;

    debug!(
        "Running cargo install --path . --root {}",
        install_dir.display()
    );
    let mut cmd = Command::new(cargo_exe);
    cmd.arg("install")
        .arg("--path")
        .arg(".") // Build path is relative to the CWD where command runs
        .arg("--root")
        .arg(install_dir);

    run_command_in_dir(&mut cmd, source_dir, build_env, "cargo install")?;
    debug!("Cargo install completed successfully.");

    Ok(())
}
