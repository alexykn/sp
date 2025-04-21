use std::path::Path;
use std::process::Command;

use tracing::{debug, info};

use crate::build::env::BuildEnvironment;
use crate::utils::error::{Result, SapphireError};

/// Build with Meson
pub fn meson_build(
    source_dot: &Path, // Parameter represents "." now, as CWD is the source root
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    // Ensure source_dot is actually "."
    if source_dot != Path::new(".") {
        return Err(SapphireError::BuildEnvError(format!(
            "meson_build expected source path '.' but received '{}'",
            source_dot.display()
        )));
    }

    info!("==> Building with Meson");
    let build_subdir_name = "sapphire-meson-build";
    // Meson prefers the build directory passed as an argument, relative to CWD
    let build_subdir = Path::new(".").join(build_subdir_name);

    let meson_exe =
        which::which_in("meson", build_env.get_path_string(), Path::new(".")).map_err(|_| {
            SapphireError::BuildEnvError(
                "meson command not found in build environment PATH.".to_string(),
            )
        })?;
    info!(
        "==> Running meson setup: {} setup --prefix={} {} {}",
        meson_exe.display(),
        install_dir.display(),
        build_subdir.display(), // Build directory
        "."                     // Source directory (CWD) <-- CHANGED from source_dir
    );

    let mut cmd_setup = Command::new(&meson_exe);
    cmd_setup
        .arg("setup")
        .arg(format!("--prefix={}", install_dir.display()))
        .arg("--buildtype=release")
        .arg("--libdir=lib")
        .arg(&build_subdir) // Specify build directory
        .arg("."); // Specify source directory (CWD) <-- CHANGED from source_dir
    build_env.apply_to_command(&mut cmd_setup);
    // Meson setup runs from the CWD (which is the source root)
    let output_setup = cmd_setup.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute meson setup: {}", e))
    })?;

    if !output_setup.status.success() {
        // (Error handling code from your original file)
        println!("Meson setup failed with status: {}", output_setup.status);
        eprintln!(
            "Meson setup stdout:\n{}",
            String::from_utf8_lossy(&output_setup.stdout)
        );
        eprintln!(
            "Meson setup stderr:\n{}",
            String::from_utf8_lossy(&output_setup.stderr)
        );
        return Err(SapphireError::Generic(format!(
            "Meson setup failed with status: {}",
            output_setup.status
        )));
    } else {
        debug!(
            "Meson setup stdout:\n{}",
            String::from_utf8_lossy(&output_setup.stdout)
        );
        debug!(
            "Meson setup stderr:\n{}",
            String::from_utf8_lossy(&output_setup.stderr)
        );
    }

    // Check for ninja before attempting install
    let _ninja_exe = which::which_in("ninja", build_env.get_path_string(), Path::new("."))
        .map_err(|_| SapphireError::BuildEnvError("ninja command not found (needed for meson install). Ensure ninja is installed and in build dependencies.".to_string()))?;

    // Meson install uses -C to specify the build directory
    info!("==> Running meson install -C {}", build_subdir.display());
    let mut cmd_install = Command::new(&meson_exe);
    cmd_install.arg("install").arg("-C").arg(&build_subdir); // Use -C flag
    build_env.apply_to_command(&mut cmd_install);
    // Install command also runs from the CWD (source root)
    let output_install = cmd_install.output().map_err(|e| {
        SapphireError::CommandExecError(format!("Failed to execute meson install: {}", e))
    })?;

    if !output_install.status.success() {
        // (Error handling code from your original file)
        println!(
            "Meson install failed with status: {}",
            output_install.status
        );
        eprintln!(
            "Meson install stdout:\n{}",
            String::from_utf8_lossy(&output_install.stdout)
        );
        eprintln!(
            "Meson install stderr:\n{}",
            String::from_utf8_lossy(&output_install.stderr)
        );
        return Err(SapphireError::Generic(format!(
            "Meson install failed with status: {}",
            output_install.status
        )));
    } else {
        debug!("Meson install completed successfully.");
    }

    Ok(())
}
