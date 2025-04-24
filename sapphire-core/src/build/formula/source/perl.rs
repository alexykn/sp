// FILE: sapphire-core/src/build/formula/source/perl.rs

use std::path::Path;
use std::process::Command;

use tracing::debug;

use crate::build::env::BuildEnvironment;
use crate::build::formula::source::run_command_in_dir;
use crate::utils::error::{Result, SapphireError};

pub fn perl_build(
    source_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    let configure_script = source_dir.join("Configure");
    let makefile_pl = source_dir.join("Makefile.PL");

    if configure_script.is_file() {
        debug!(
            "Building with Perl Configure script in {}",
            source_dir.display()
        );
        let sh_exe = which::which_in("sh", build_env.get_path_string(), source_dir)
            .map_err(|_| SapphireError::BuildEnvError("sh command not found.".to_string()))?;

        let mut cmd_configure = Command::new(sh_exe);
        cmd_configure
            .arg("./Configure") // Relative to CWD where command runs
            .arg("-des")
            .arg(format!("-Dprefix={}", install_dir.display()));

        let configure_output =
            run_command_in_dir(&mut cmd_configure, source_dir, build_env, "Perl Configure")?;
        debug!(
            "Perl Configure stdout:\n{}",
            String::from_utf8_lossy(&configure_output.stdout)
        );
        debug!(
            "Perl Configure stderr:\n{}",
            String::from_utf8_lossy(&configure_output.stderr)
        );
    } else if makefile_pl.is_file() {
        debug!("Building with Perl Makefile.PL in {}", source_dir.display());
        let perl_exe = which::which_in("perl", build_env.get_path_string(), source_dir)
            .map_err(|_| SapphireError::BuildEnvError("perl command not found.".to_string()))?;

        let mut cmd_makefile = Command::new(perl_exe);
        cmd_makefile
            .arg("Makefile.PL")
            .arg(format!("PREFIX={}", install_dir.display()));

        let makefile_output =
            run_command_in_dir(&mut cmd_makefile, source_dir, build_env, "perl Makefile.PL")?;
        debug!(
            "perl Makefile.PL stdout:\n{}",
            String::from_utf8_lossy(&makefile_output.stdout)
        );
        debug!(
            "perl Makefile.PL stderr:\n{}",
            String::from_utf8_lossy(&makefile_output.stderr)
        );
    } else {
        return Err(SapphireError::BuildEnvError(
            "Neither Perl Configure nor Makefile.PL script found.".to_string(),
        ));
    }

    let make_exe = which::which_in("make", build_env.get_path_string(), source_dir)
        .map_err(|_| SapphireError::BuildEnvError("make command not found.".to_string()))?;

    debug!("Running make for Perl");
    let mut cmd_make = Command::new(make_exe.clone());
    run_command_in_dir(&mut cmd_make, source_dir, build_env, "make (perl)")?;
    debug!("Perl make completed successfully.");

    debug!("Running make install for Perl");
    let mut cmd_install = Command::new(make_exe);
    cmd_install.arg("install");
    run_command_in_dir(
        &mut cmd_install,
        source_dir,
        build_env,
        "make install (perl)",
    )?;
    debug!("Perl make install completed successfully.");

    Ok(())
}
