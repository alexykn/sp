use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{debug, info, warn};

use crate::build::env::BuildEnvironment;
use crate::utils::error::{Result, SapphireError};

/// Build Go project
pub fn go_build(
    build_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
    all_installed_paths: &[PathBuf],
) -> Result<()> {
    info!("==> Building with Go build script");

    let (script_to_run, run_in_dir) = if build_dir.join("src/make.bash").exists() {
        (build_dir.join("src/make.bash"), build_dir.join("src"))
    } else if build_dir.join("src/all.bash").exists() {
        (build_dir.join("src/all.bash"), build_dir.join("src"))
    } else {
        return Err(SapphireError::Generic(
            "Go build script (src/make.bash or src/all.bash) not found.".to_string(),
        ));
    };

    let bash_exe =
        which::which_in("bash", build_env.get_path_string(), Path::new(".")).map_err(|_| {
            SapphireError::BuildEnvError(
                "bash command not found in build environment PATH (needed for Go build script)."
                    .to_string(),
            )
        })?;
    info!(
        "==> Running {} {} in {}",
        bash_exe.display(),
        script_to_run.display(),
        run_in_dir.display()
    );

    if cfg!(unix) {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(&script_to_run) {
            Ok(metadata) => {
                let mut perms = metadata.permissions();
                let original_mode = perms.mode();
                if original_mode & 0o100 == 0 {
                    perms.set_mode(original_mode | 0o100);
                    if let Err(e) = fs::set_permissions(&script_to_run, perms) {
                        warn!(
                            "Warning: Failed to set executable permission on Go build script: {}",
                            e
                        );
                    } else {
                        debug!("Set Go build script executable.");
                    }
                }
            }
            Err(e) => warn!(
                "Warning: Failed to read metadata for Go build script: {}",
                e
            ),
        }
    }

    let mut go_build_specific_env = HashMap::new();
    let bootstrap_go_path = all_installed_paths.iter().find(|p| {
        p.file_name().map_or(false, |n| {
            n == "go" || n.to_string_lossy().starts_with("go@")
        })
    });

    if let Some(path) = bootstrap_go_path {
        info!("Found bootstrap Go path: {}", path.display());
        go_build_specific_env.insert(
            "GOROOT_BOOTSTRAP".to_string(),
            path.to_string_lossy().to_string(),
        );
    } else if build_env.get_var("GOROOT_BOOTSTRAP").is_none() {
        warn!("GOROOT_BOOTSTRAP not set and no Go dependency path found. Go build might fail if required.");
    }

    let mut cmd = Command::new(bash_exe);
    cmd.arg(&script_to_run);
    cmd.current_dir(&run_in_dir);
    build_env.apply_to_command(&mut cmd);
    cmd.envs(&go_build_specific_env);

    let output = cmd.output().map_err(|e| {
        SapphireError::CommandExecError(format!(
            "Failed to execute Go build script {}: {}",
            script_to_run.display(),
            e
        ))
    })?;

    if !output.status.success() {
        println!("Go build script failed with status: {}", output.status);
        eprintln!(
            "Go build stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "Go build stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(SapphireError::Generic(format!(
            "Go build script failed with status: {}",
            output.status
        )));
    } else {
        debug!(
            "Go build stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        debug!(
            "Go build stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    info!(
        "==> Installing Go build artifacts to {}",
        install_dir.display()
    );
    fs::create_dir_all(install_dir).map_err(SapphireError::Io)?;

    let go_output_bin_dir = build_dir.join("bin");
    let go_output_pkg_dir = build_dir.join("pkg");
    let target_bin_dir = install_dir.join("bin");
    let target_pkg_dir = install_dir.join("pkg");

    if go_output_bin_dir.is_dir() {
        info!(
            "Copying contents from {} to {}",
            go_output_bin_dir.display(),
            target_bin_dir.display()
        );
        fs::create_dir_all(&target_bin_dir).map_err(SapphireError::Io)?;
        copy_directory_contents(&go_output_bin_dir, &target_bin_dir)?;
    } else {
        warn!(
            "Go output bin directory not found: {}",
            go_output_bin_dir.display()
        );
    }

    if go_output_pkg_dir.is_dir() {
        info!(
            "Copying contents from {} to {}",
            go_output_pkg_dir.display(),
            target_pkg_dir.display()
        );
        fs::create_dir_all(&target_pkg_dir).map_err(SapphireError::Io)?;
        copy_directory_contents(&go_output_pkg_dir, &target_pkg_dir)?;
    } else {
        debug!(
            "Go output pkg directory not found: {}",
            go_output_pkg_dir.display()
        );
    }

    let go_output_src_dir = build_dir.join("src");
    if go_output_src_dir.is_dir() {
        let target_src_dir = install_dir.join("src");
        info!(
            "Copying contents from {} to {}",
            go_output_src_dir.display(),
            target_src_dir.display()
        );
        fs::create_dir_all(&target_src_dir).map_err(SapphireError::Io)?;
        copy_directory_contents(&go_output_src_dir, &target_src_dir)?;
    } else {
        debug!(
            "Go output src directory not found: {}",
            go_output_src_dir.display()
        );
    }

    Ok(())
}

/// Recursively copies the contents of a source directory to a target directory.
fn copy_directory_contents(from: &Path, to: &Path) -> Result<()> {
    for entry_result in fs::read_dir(from).map_err(SapphireError::Io)? {
        let entry = entry_result.map_err(SapphireError::Io)?;
        let src_path = entry.path();
        let dest_path = to.join(entry.file_name());

        if src_path.is_dir() {
            fs::create_dir_all(&dest_path).map_err(SapphireError::Io)?;
            copy_directory_contents(&src_path, &dest_path)?;
        } else if src_path.is_file() {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent).map_err(SapphireError::Io)?;
            }
            fs::copy(&src_path, &dest_path).map_err(SapphireError::Io)?;
        }
    }
    Ok(())
}
