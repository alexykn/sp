use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{debug, info};

use crate::build::env::BuildEnvironment;
use crate::utils::error::{Result, SapphireError};

/// Build Perl using its Configure script or Makefile.PL
pub fn perl_build(
    _source_dot: &Path, // <--- Renamed and prefixed with underscore
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    let configure_script = PathBuf::from("Configure");
    let makefile_pl = PathBuf::from("Makefile.PL");

    // Determine which script to use
    if configure_script.exists() {
        info!("==> Building with Perl Configure script...");
        let sh_exe =
            which::which_in("sh", build_env.get_path_string(), Path::new(".")).map_err(|_| {
                SapphireError::BuildEnvError(
                    "sh command not found in build environment PATH (needed for Perl Configure)."
                        .to_string(),
                )
            })?;

        let mut cmd = Command::new(sh_exe);
        cmd.arg(configure_script); // Runs ./Configure
        cmd.arg("-des");
        cmd.arg(format!("-Dprefix={}", install_dir.display()));

        build_env.apply_to_command(&mut cmd);
        info!("Running Perl Configure: {:?}", cmd);
        let output = cmd.output().map_err(|e| {
            SapphireError::CommandExecError(format!("Failed to execute Perl Configure: {}", e))
        })?;

        if !output.status.success() {
            // (Error handling remains the same)
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
    } else if makefile_pl.exists() {
        info!("==> Building with Perl Makefile.PL...");
        let perl_exe =
            which::which_in("perl", build_env.get_path_string(), Path::new(".")).map_err(|_| {
                SapphireError::BuildEnvError(
                    "perl command not found in build environment PATH.".to_string(),
                )
            })?;

        let mut cmd = Command::new(perl_exe);
        cmd.arg("Makefile.PL"); // Runs perl Makefile.PL
                                // Add common args if needed, e.g., INSTALL_BASE
                                // cmd.arg(format!("INSTALL_BASE={}", install_dir.display()));
        cmd.arg(format!("PREFIX={}", install_dir.display())); // Often uses PREFIX

        build_env.apply_to_command(&mut cmd);
        info!("Running perl Makefile.PL: {:?}", cmd);
        let output = cmd.output().map_err(|e| {
            SapphireError::CommandExecError(format!("Failed to execute perl Makefile.PL: {}", e))
        })?;

        if !output.status.success() {
            // (Error handling)
            println!("perl Makefile.PL failed with status: {}", output.status);
            eprintln!(
                "perl Makefile.PL stdout:\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            eprintln!(
                "perl Makefile.PL stderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return Err(SapphireError::Generic(format!(
                "perl Makefile.PL failed with status: {}",
                output.status
            )));
        } else {
            debug!(
                "perl Makefile.PL stdout:\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            debug!(
                "perl Makefile.PL stderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    } else {
        return Err(SapphireError::BuildEnvError(
            "Neither Perl Configure nor Makefile.PL script found in CWD.".to_string(),
        ));
    }

    // Run make (common step for both Configure and Makefile.PL)
    info!("==> Running make for Perl");
    let make_exe =
        which::which_in("make", build_env.get_path_string(), Path::new(".")).map_err(|_| {
            SapphireError::BuildEnvError(
                "make command not found in build environment PATH.".to_string(),
            )
        })?;
    let mut make_cmd = Command::new(make_exe.clone());
    build_env.apply_to_command(&mut make_cmd);
    let output_make = make_cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute make for Perl: {}", e))
    })?;

    if !output_make.status.success() {
        // (Error handling remains the same)
        println!("Perl make failed with status: {}", output_make.status);
        eprintln!(
            "Perl make stdout:\n{}",
            String::from_utf8_lossy(&output_make.stdout)
        );
        eprintln!(
            "Perl make stderr:\n{}",
            String::from_utf8_lossy(&output_make.stderr)
        );
        return Err(SapphireError::Generic(format!(
            "Perl make failed with status: {}",
            output_make.status
        )));
    } else {
        info!("Perl make completed successfully.");
    }

    // Run make install
    info!("==> Running make install for Perl");
    let mut install_cmd = Command::new(make_exe);
    install_cmd.arg("install");
    build_env.apply_to_command(&mut install_cmd);
    let output_install = install_cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute make install for Perl: {}", e))
    })?;

    if !output_install.status.success() {
        // (Error handling remains the same)
        println!("Perl make install failed with status: {}", output_install.status);
        eprintln!(
            "Perl make install stdout:\n{}",
            String::from_utf8_lossy(&output_install.stdout)
        );
        eprintln!(
            "Perl make install stderr:\n{}",
            String::from_utf8_lossy(&output_install.stderr)
        );
        return Err(SapphireError::Generic(format!(
            "Perl make install failed with status: {}",
            output_install.status
        )));
    } else {
        info!("Perl make install completed successfully.");
    }

    Ok(())
}
