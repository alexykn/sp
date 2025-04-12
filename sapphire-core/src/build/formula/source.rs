use crate::utils::error::{SapphireError, Result};
use crate::model::formula::Formula;
use crate::utils::config::Config;
use crate::build::env::BuildEnvironment;
use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::copy;
use reqwest::Client;
use std::io::Cursor;
use std::process::Command;
use std::rc::Rc; // Import Rc

/// Download source code for the formula
pub async fn download_source(formula: &Formula, config: &Config) -> Result<PathBuf> {
    // Try to get a stable URL from the formula
    let url = if let Some(stable_url) = get_formula_url(formula) {
        stable_url
    } else {
        // Fallback to creating a URL from the formula name and homepage
        if let Some(homepage) = &formula.homepage {
            if homepage.contains("github.com") {
                // Create a typical GitHub release URL as fallback
                format!("{}/archive/v{}.tar.gz", homepage.trim_end_matches('/'), formula.version)
            } else {
                return Err(SapphireError::Generic("No source URL available".to_string()));
            }
        } else {
            return Err(SapphireError::Generic("No source URL available".to_string()));
        }
    };

    println!("==> Downloading source for {}", formula.name);

    download_url(&url, config).await
}

/// Get the main URL for a formula, trying stable URL first
fn get_formula_url(formula: &Formula) -> Option<String> {
    if !formula.url.is_empty() {
        Some(formula.url.clone())
    } else {
        None
    }
}

/// Download a specific URL to the cache
async fn download_url(url: &str, config: &Config) -> Result<PathBuf> {
    // Create cache directory if it doesn't exist
    let cache_dir = PathBuf::from(&config.cache_dir).join("source");
    fs::create_dir_all(&cache_dir)?;

    // Generate a filename from the URL
    let filename = url.split('/').last().unwrap_or("download.tar.gz");
    let source_path = cache_dir.join(filename);

    // Skip download if the file already exists
    if source_path.exists() {
        println!("Using cached source: {}", source_path.display());
        return Ok(source_path);
    }

    // Download the source
    let client = Client::new();
    let response = client.get(url)
        .send()
        .await
        .map_err(|e| SapphireError::Http(e))?;

    if !response.status().is_success() {
        return Err(SapphireError::Generic(format!("Failed to download: HTTP status {}", response.status())));
    }

    let content = response.bytes()
        .await
        .map_err(|e| SapphireError::Http(e))?;

    // Write the source to disk
    let mut file = File::create(&source_path)?;
    let mut content_cursor = Cursor::new(content);
    copy(&mut content_cursor, &mut file)?;

    println!("Downloaded source to {}", source_path.display());

    Ok(source_path)
}

/// Build and install formula from source
pub fn build_from_source(
    source_path: &Path,
    formula: &Formula, // Keep as reference, BuildEnvironment needs trait object anyway
    config: &Config,
    build_dep_opt_paths: &[PathBuf], // Added parameter for build dep paths
) -> Result<PathBuf> {
    // Get the installation directory
    let install_dir = super::get_formula_cellar_path(formula);

    // Create a temporary directory for building
    // Use a subdirectory within the configured cache dir for better isolation/cleanup
    let temp_dir_base = config.cache_dir.join("build-temp");
    fs::create_dir_all(&temp_dir_base)?;
    let temp_dir = tempfile::Builder::new()
        .prefix(&format!("{}-", formula.name))
        .tempdir_in(&temp_dir_base)
        .map_err(|e| SapphireError::Io(e))?
        .into_path(); // Get PathBuf and keep the directory

     // Ensure we own the temp dir for the scope of this function
     // The directory will be removed when `_temp_dir_guard` goes out of scope if using tempfile::tempdir()
     // If using into_path(), manual cleanup might be needed on error, or rely on system cleanup.
    // For simplicity, let's assume it gets cleaned up eventually or manually.


    println!("==> Extracting source to {}", temp_dir.display());

    // Extract the source
    // Extract directly into the temp dir, stripping the top-level component
    super::extract_archive_strip_components(source_path, &temp_dir, 1)?;


    // The build directory is now the temp directory itself after stripping
    let build_dir = temp_dir; // Use the temp dir as the build dir


    println!("==> Building {} from source in {}", formula.name, build_dir.display());

    // --- Setup Build Environment ---
    println!("==> Setting up build environment");
    let sapphire_prefix = build::get_homebrew_prefix(); // Use helper from build mod
    // Create BuildEnvironment, passing the build dependency paths
    let build_env = BuildEnvironment::new(
        formula,
        &sapphire_prefix,
        &config.cellar,
        build_dep_opt_paths, // Pass the resolved build dep paths here
    )?;


    // Detect and build using the appropriate build system
    detect_and_build(&build_dir, &install_dir, &build_env)?;

    // Write the receipt
    super::write_receipt(formula, &install_dir)?;

    // Explicitly clean up the temp build directory (optional, but good practice)
    if let Err(e) = fs::remove_dir_all(&build_dir) {
        eprintln!("Warning: Failed to clean up temporary build directory {}: {}", build_dir.display(), e);
    }


    Ok(install_dir)
}


/// Find the directory containing the source code after extraction
/// DEPRECATED: Assuming extraction with --strip-components=1 now.
#[allow(dead_code)]
fn find_build_directory(temp_dir: &Path, formula_name: &str) -> Result<PathBuf> {
    // Try a directory with the formula name
    let name_dir = temp_dir.join(formula_name);
    if name_dir.exists() && name_dir.is_dir() {
        return Ok(name_dir);
    }

    // Try a formula-version directory
    let entries = fs::read_dir(temp_dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && path.file_name().unwrap().to_string_lossy().contains(formula_name) {
            return Ok(path);
        }
    }

    // If only one directory exists, use that
    let entries: Vec<_> = fs::read_dir(temp_dir)?.collect::<std::io::Result<Vec<_>>>()?;
    if entries.len() == 1 && entries[0].path().is_dir() {
        return Ok(entries[0].path());
    }

    // If we find multiple directories but one matches a specific pattern (e.g., m4-1.4.19)
    // we should prioritize that one
    let entries: Vec<_> = fs::read_dir(temp_dir)?.collect::<std::io::Result<Vec<_>>>()?;
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            let filename = path.file_name().unwrap().to_string_lossy().to_string();
            if filename.starts_with(formula_name) && filename.contains('-') {
                return Ok(path);
            }
        }
    }

    // Last resort - just print what directories we actually found to help diagnose
    println!("Available directories in {}", temp_dir.display());
    for entry in fs::read_dir(temp_dir)? {
        if let Ok(entry) = entry {
            println!(" - {}", entry.path().display());
        }
    }

    Err(SapphireError::Generic(format!(
        "Could not find build directory for {} in {}",
        formula_name, temp_dir.display()
    )))
}

/// Detect the build system and use the appropriate build method
fn detect_and_build(
    build_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    // First, ensure build directory exists and is accessible
    if !build_dir.exists() {
        return Err(SapphireError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Build directory does not exist: {}", build_dir.display()),
        )));
    }

    // Check if we can access it
    std::fs::read_dir(build_dir).map_err(|e| {
        SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Cannot access build directory {}: {}", build_dir.display(), e),
        ))
    })?;

    // Change to the build directory *before* running any build commands
    // Store the original CWD to restore later
    let original_cwd = std::env::current_dir()?;
    std::env::set_current_dir(build_dir)?;
    println!("Changed working directory to: {}", build_dir.display());

    // Use a guard to ensure we change back the CWD even on errors
    let _cwd_guard = CurrentWorkingDirectoryGuard::new(original_cwd);


    // 1. Check for autogen.sh (usually needs to run before configure)
    let autogen_script = build_dir.join("autogen.sh");
    if autogen_script.exists() {
        println!("==> Running ./autogen.sh");

        // Ensure the script is executable
        if cfg!(unix) {
             use std::os::unix::fs::PermissionsExt;
             let mut perms = fs::metadata(&autogen_script)?.permissions();
             perms.set_mode(perms.mode() | 0o111); // Add execute permissions u+x, g+x, o+x
             fs::set_permissions(&autogen_script, perms)?;
        }


        let mut cmd = Command::new("./autogen.sh");
        // Apply the environment AFTER creating the Command struct
        build_env.apply_to_command(&mut cmd);
        let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute autogen.sh: {}", e)))?;


        if !output.status.success() {
            eprintln!("autogen.sh stdout:\n{}", String::from_utf8_lossy(&output.stdout));
            eprintln!("autogen.sh stderr:\n{}", String::from_utf8_lossy(&output.stderr));
            return Err(SapphireError::Generic(format!(
                "autogen.sh failed with status: {}", output.status
            )));
        }
    }
    // 2. Check for configure.ac or configure.in (needs autoconf)
    else if build_dir.join("configure.ac").exists() || build_dir.join("configure.in").exists() {
        // Check if autoreconf exists in the environment's PATH
        let autoreconf_path = match which::which_in("autoreconf", build_env.get_path_string(), build_dir) {
             Ok(path) => path,
             Err(_) => return Err(SapphireError::BuildEnvError(
                 "autoreconf command not found in build environment PATH. Ensure autoconf is installed and in build dependencies.".to_string()
             ))
        };

        println!("==> Running autoreconf -fvi");
        let mut cmd = Command::new(autoreconf_path);
        cmd.args(["-fvi"]); // Force, verbose, install missing aux files
         // Apply the environment AFTER creating the Command struct
         build_env.apply_to_command(&mut cmd);
        let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute autoreconf: {}", e)))?;

        if !output.status.success() {
            eprintln!("autoreconf stdout:\n{}", String::from_utf8_lossy(&output.stdout));
            eprintln!("autoreconf stderr:\n{}", String::from_utf8_lossy(&output.stderr));
            return Err(SapphireError::Generic(format!(
                "autoreconf failed with status: {}", output.status
            )));
        }
        // After autoreconf, a 'configure' script should exist, so fall through to configure_and_make
    }


    // 3. Check for configure script
    let configure_script = build_dir.join("configure");
    if configure_script.exists() {
        // Ensure the configure script is executable
        if cfg!(unix) {
             use std::os::unix::fs::PermissionsExt;
             let mut perms = fs::metadata(&configure_script)?.permissions();
             perms.set_mode(perms.mode() | 0o111);
             fs::set_permissions(&configure_script, perms)?;
        }

        return configure_and_make(build_dir, install_dir, build_env);
    }

    // 4. Check for CMakeLists.txt
    if build_dir.join("CMakeLists.txt").exists() {
        return cmake_build(build_dir, install_dir, build_env);
    }

    // 5. Check for meson.build
    if build_dir.join("meson.build").exists() {
        return meson_build(build_dir, install_dir, build_env);
    }

    // 6. Check for Cargo.toml (Rust project)
    if build_dir.join("Cargo.toml").exists() {
        return cargo_build(build_dir, install_dir, build_env);
    }

    // 7. Check for setup.py (Python project)
    if build_dir.join("setup.py").exists() {
        return python_build(build_dir, install_dir, build_env);
    }

    // 8. Check for Makefile directly (simple make project)
    if build_dir.join("Makefile").exists() || build_dir.join("makefile").exists() {
        return simple_make(build_dir, install_dir, build_env);
    }


    // If we get here, we couldn't determine the build system
    Err(SapphireError::Generic(format!(
        "Could not determine build system for {}",
        build_dir.display()
    )))
}

/// Configure and build with autotools (./configure && make && make install)
fn configure_and_make(
    build_dir: &Path, // Should be CWD already
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    println!("==> Running ./configure --prefix={}", install_dir.display());

    let mut cmd = Command::new("./configure");
    cmd.arg(format!("--prefix={}", install_dir.display()));
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute configure: {}", e)))?;


    // Log output for debugging
    println!("Configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        // Attempt to find config.log for more details
        let config_log_path = build_dir.join("config.log");
        if config_log_path.exists() {
             eprintln!("--- config.log ---");
             if let Ok(content) = fs::read_to_string(&config_log_path) {
                  // Print only last N lines for brevity?
                 let lines: Vec<&str> = content.lines().rev().take(50).collect();
                 for line in lines.iter().rev() {
                     eprintln!("{}", line);
                 }
             }
             eprintln!("--- end config.log ---");
        }
        return Err(SapphireError::Generic(format!(
            "Configure failed with status: {}", output.status
        )));
    }

    // Run make
    println!("==> Running make");
    let mut cmd = Command::new("make");
    build_env.apply_to_command(&mut cmd); // MAKEFLAGS should be in build_env
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make: {}", e)))?;


    // Log output for debugging
    println!("Make stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Make stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Make failed with status: {}", output.status
        )));
    }

    // Run make install
    println!("==> Running make install");
    let mut cmd = Command::new("make");
    cmd.arg("install");
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make install: {}", e)))?;


    // Log output for debugging
    println!("Make install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Make install stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Make install failed with status: {}", output.status
        )));
    }

    Ok(())
}

/// Build with CMake
fn cmake_build(
    source_dir: &Path, // Source dir (which is also CWD)
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    println!("==> Building with CMake");
    let build_subdir_name = "sapphire-cmake-build";
    let build_subdir = source_dir.join(build_subdir_name);
    fs::create_dir_all(&build_subdir).map_err(|e| SapphireError::Io(e))?;

    println!("==> Running cmake .. -DCMAKE_INSTALL_PREFIX={}", install_dir.display());
    let cmake_exe = which::which_in("cmake", build_env.get_path_string(), source_dir)
        .map_err(|_| SapphireError::BuildEnvError("cmake command not found in build environment PATH.".to_string()))?;


    let mut cmd = Command::new(cmake_exe);
    cmd.arg("..") // Configure from parent directory (source_dir)
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", install_dir.display()))
        .args(&[
            "-DCMAKE_FIND_FRAMEWORK=LAST", // Standard Homebrew flags
            "-DCMAKE_VERBOSE_MAKEFILE=ON",
            "-Wno-dev", // Suppress -Wdev warnings
            // Add CMAKE_BUILD_TYPE ? e.g., Release
            // "-DCMAKE_BUILD_TYPE=Release", // Uncomment if needed
        ])
        .current_dir(&build_subdir); // Run cmake *in* the build subdir
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute cmake: {}", e)))?;


    println!("CMake configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("CMake configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "CMake configure failed with status: {}", output.status
        )));
    }

    println!("==> Running make install in {}", build_subdir.display());
    let make_exe = which::which_in("make", build_env.get_path_string(), source_dir)
        .map_err(|_| SapphireError::BuildEnvError("make command not found in build environment PATH.".to_string()))?;


    let mut cmd = Command::new(make_exe);
    cmd.arg("install")
        .current_dir(&build_subdir); // Run make install *in* the build subdir
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make install (CMake): {}", e)))?;


    println!("CMake make install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("CMake make install stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "CMake make install failed with status: {}", output.status
        )));
    }

    Ok(())
}

/// Build with Meson
fn meson_build(
    source_dir: &Path, // Source dir (which is also CWD)
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
     println!("==> Building with Meson");
     let build_subdir_name = "sapphire-meson-build";
     let build_subdir = source_dir.join(build_subdir_name);

     // Meson setup command
     println!("==> Running meson setup --prefix={} {}", install_dir.display(), build_subdir.display());
     let meson_exe = which::which_in("meson", build_env.get_path_string(), source_dir)
         .map_err(|_| SapphireError::BuildEnvError("meson command not found in build environment PATH.".to_string()))?;

     let mut cmd_setup = Command::new(meson_exe);
     cmd_setup.arg("setup")
         .arg(format!("--prefix={}", install_dir.display()))
         // Add other standard meson options if needed (e.g., buildtype)
         // .arg("--buildtype=release")
         .arg(&build_subdir) // The build directory to create/use
         .arg(".");          // The source directory
     build_env.apply_to_command(&mut cmd_setup); // Apply env vars
     let output_setup = cmd_setup.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute meson setup: {}", e)))?;

     println!("Meson setup stdout:\n{}", String::from_utf8_lossy(&output_setup.stdout));
     println!("Meson setup stderr:\n{}", String::from_utf8_lossy(&output_setup.stderr));

     if !output_setup.status.success() {
         return Err(SapphireError::Generic(format!(
             "Meson setup failed with status: {}", output_setup.status
         )));
     }

     // Meson install command
     println!("==> Running meson install -C {}", build_subdir.display());
      let ninja_exe = which::which_in("ninja", build_env.get_path_string(), source_dir)
          .map_err(|_| SapphireError::BuildEnvError("ninja command not found in build environment PATH (needed for meson install).".to_string()))?;

     // Meson install uses ninja internally by default
     let mut cmd_install = Command::new(meson_exe); // Still use meson command
     cmd_install.arg("install")
         .arg("-C") // Specify build directory
         .arg(&build_subdir);
     build_env.apply_to_command(&mut cmd_install); // Apply env vars
     let output_install = cmd_install.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute meson install: {}", e)))?;

     println!("Meson install stdout:\n{}", String::from_utf8_lossy(&output_install.stdout));
     println!("Meson install stderr:\n{}", String::from_utf8_lossy(&output_install.stderr));

     if !output_install.status.success() {
         return Err(SapphireError::Generic(format!(
             "Meson install failed with status: {}", output_install.status
         )));
     }

    Ok(())
}

/// Build with Cargo
fn cargo_build(
    source_dir: &Path, // Source dir (which is also CWD)
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
     println!("==> Building with Cargo");
     let cargo_exe = which::which_in("cargo", build_env.get_path_string(), source_dir)
         .map_err(|_| SapphireError::BuildEnvError("cargo command not found in build environment PATH.".to_string()))?;

     // Cargo install directly installs binaries to $CARGO_HOME/bin by default.
     // To install into the Cellar, we usually need `cargo build --release`
     // and then manually copy artifacts, or use `cargo install --path . --root <prefix>`
     // Let's use the latter approach as it's more direct.

     println!("==> Running cargo install --path . --root {}", install_dir.display());
     let mut cmd = Command::new(cargo_exe);
     cmd.arg("install")
         .arg("--path")
         .arg(".") // Install the crate in the current directory (source_dir)
         .arg("--root")
         .arg(install_dir); // Install into the formula's cellar directory
     build_env.apply_to_command(&mut cmd);
     let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute cargo install: {}", e)))?;

     println!("Cargo install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
     println!("Cargo install stderr:\n{}", String::from_utf8_lossy(&output.stderr));

     if !output.status.success() {
         return Err(SapphireError::Generic(format!(
             "Cargo install failed with status: {}", output.status
         )));
     }

     // Cargo install --root installs into <root>/bin, <root>/lib etc.
     // which matches the desired cellar structure. No manual copying needed.

     Ok(())
}

/// Build with Python setup.py
fn python_build(
    source_dir: &Path, // Source dir (which is also CWD)
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    println!("==> Building with Python setup.py");
     // Find the python executable, preferably one managed by Homebrew if available
     let python_exe = which::which_in("python3", build_env.get_path_string(), source_dir)
         .or_else(|_| which::which_in("python", build_env.get_path_string(), source_dir))
         .map_err(|_| SapphireError::BuildEnvError("python3 or python command not found in build environment PATH.".to_string()))?;

    println!("==> Running {} setup.py install --prefix={}", python_exe.display(), install_dir.display());
    let mut cmd = Command::new(python_exe);
    cmd.arg("setup.py")
        .arg("install")
        .arg(format!("--prefix={}", install_dir.display()));
        // Add other common Python flags if needed, e.g., --optimize=1
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute python setup.py install: {}", e)))?;


    println!("Python install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Python install stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(SapphireError::Generic(format!(
            "Python setup.py install failed with status: {}", output.status
        )));
    }

    Ok(())
}

/// Build with a simple Makefile
fn simple_make(
    _build_dir: &Path, // Is CWD
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
     println!("==> Building with simple Makefile");
      let make_exe = which::which_in("make", build_env.get_path_string(), _build_dir)
         .map_err(|_| SapphireError::BuildEnvError("make command not found in build environment PATH.".to_string()))?;


     println!("==> Running make");
     let mut cmd_make = Command::new(&make_exe);
     build_env.apply_to_command(&mut cmd_make);
     let output_make = cmd_make.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make (simple): {}", e)))?;

     println!("Make stdout:\n{}", String::from_utf8_lossy(&output_make.stdout));
     println!("Make stderr:\n{}", String::from_utf8_lossy(&output_make.stderr));

    if !output_make.status.success() {
        return Err(SapphireError::Generic(format!(
            "Make failed with status: {}", output_make.status
        )));
    }

    println!("==> Running make install PREFIX={}", install_dir.display());
    let mut cmd_install = Command::new(make_exe);
    cmd_install.arg("install");
     // Common convention for simple Makefiles: pass PREFIX variable
    cmd_install.env("PREFIX", install_dir.as_os_str()); // Set PREFIX env var as well/instead?
    // Or pass as argument: cmd_install.arg(format!("PREFIX={}", install_dir.display()));
    build_env.apply_to_command(&mut cmd_install);
    let output_install = cmd_install.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make install (simple): {}", e)))?;

    println!("Make install stdout:\n{}", String::from_utf8_lossy(&output_install.stdout));
    println!("Make install stderr:\n{}", String::from_utf8_lossy(&output_install.stderr));

    if !output_install.status.success() {
        // Some Makefiles might not have an install target, this might be okay
        // but we should probably check if files were actually installed.
        // For now, treat it as an error if the command fails.
         println!("Warning: 'make install' failed. Formula might be installed correctly if it doesn't use a standard install target.");
         // Check if install_dir/bin or install_dir/lib has files?
         let bin_dir = install_dir.join("bin");
         let lib_dir = install_dir.join("lib");
         let bin_exists = bin_dir.exists() && fs::read_dir(bin_dir).map(|mut d| d.next().is_some()).unwrap_or(false);
         let lib_exists = lib_dir.exists() && fs::read_dir(lib_dir).map(|mut d| d.next().is_some()).unwrap_or(false);
         if !bin_exists && !lib_exists {
             return Err(SapphireError::Generic(format!(
                 "Make install failed with status: {} and no files found in {}/bin or {}/lib",
                 output_install.status, install_dir.display(), install_dir.display()
             )));
         } else {
              println!("Proceeding despite 'make install' error as installation directory seems populated.");
         }
    }

    Ok(())
}

/// Check if a file is executable (on Unix)
#[cfg(unix)]
fn is_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(path)?;
    let permissions = metadata.permissions();
    // Check if user, group, or other has execute permission
    Ok(permissions.mode() & 0o111 != 0)
}

/// Check if a file is executable (fallback for non-Unix)
#[cfg(not(unix))]
fn is_executable(path: &Path) -> Result<bool> {
     // Simple check for common executable extensions on non-Unix
     Ok(path.is_file() &&
        path.extension().map_or(false, |ext| ext == "exe" || ext == "bat" || ext == "cmd" || ext == "sh"))
}


// RAII Guard to restore Current Working Directory
struct CurrentWorkingDirectoryGuard {
    original_cwd: PathBuf,
}

impl CurrentWorkingDirectoryGuard {
    fn new(original_cwd: PathBuf) -> Self {
        Self { original_cwd }
    }
}

impl Drop for CurrentWorkingDirectoryGuard {
    fn drop(&mut self) {
        if let Err(e) = std::env::set_current_dir(&self.original_cwd) {
            eprintln!(
                "Error: Failed to restore original working directory to {}: {}",
                self.original_cwd.display(),
                e
            );
        } else {
             println!("Restored working directory to: {}", self.original_cwd.display());
        }
    }
}
