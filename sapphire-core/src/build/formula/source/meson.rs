// FILE: sapphire-core/src/build/formula/source/meson.rs

use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::env::BuildEnvironment;
use crate::build::formula::source::run_command_in_dir;
use crate::utils::error::{Result, SapphireError};

pub fn meson_build(
    source_subdir: &Path,
    build_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    debug!("Building with Meson in {}", build_dir.display());
    let meson_build_subdir_name = "sapphire-meson-build";
    let meson_build_dir = build_dir.join(meson_build_subdir_name);
    let source_root_abs = build_dir.join(source_subdir);

    let meson_exe =
        which::which_in("meson", build_env.get_path_string(), build_dir).map_err(|_| {
            SapphireError::BuildEnvError(
                "meson command not found in build environment PATH.".to_string(),
            )
        })?;

    debug!(
        "Running meson setup (source: {}, build: {})",
        source_root_abs.display(),
        meson_build_dir.display()
    );

    let mut cmd_setup = Command::new(&meson_exe);
    cmd_setup
        .arg("setup")
        .arg(format!("--prefix={}", install_dir.display()))
        .arg("--buildtype=release")
        .arg("--libdir=lib")
        .arg(&meson_build_dir)
        .arg("."); // Source directory is CWD for the command

    let setup_output =
        run_command_in_dir(&mut cmd_setup, &source_root_abs, build_env, "meson setup")?;
    debug!(
        "Meson setup stdout:\n{}",
        String::from_utf8_lossy(&setup_output.stdout)
    );
    debug!(
        "Meson setup stderr:\n{}",
        String::from_utf8_lossy(&setup_output.stderr)
    );

    debug!("Running meson install -C {}", meson_build_dir.display());
    let _ninja_exe = which::which_in("ninja", build_env.get_path_string(), build_dir)
        .map_err(|_| SapphireError::BuildEnvError("ninja command not found.".to_string()))?;

    let mut cmd_install = Command::new(&meson_exe);
    cmd_install.arg("install").arg("-C").arg(&meson_build_dir);

    run_command_in_dir(
        &mut cmd_install,
        &source_root_abs,
        build_env,
        "meson install",
    )?;
    debug!("Meson install completed successfully.");

    Ok(())
}
