use crate::utils::error::{SapphireError, Result};
use crate::model::formula::Formula;
use crate::utils::config::Config;
use crate::build::env::BuildEnvironment; // Import BuildEnvironment
use crate::build; // Import the build module itself for get_homebrew_prefix
use crate::build::extract_archive_strip_components; // Import the specific function
use crate::build::fallback; // Import the fallback module
use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::copy; // Added Write trait
use reqwest::Client;
use std::io::Cursor;
use std::process::Command;
use std::collections::HashMap; // For Go bootstrap env var
use log::{debug, info, warn}; // Use logging


/// Download source code for the formula
pub async fn download_source(formula: &Formula, config: &Config) -> Result<PathBuf> {
    // Try to get a stable URL from the formula
    let url = if !formula.url.is_empty() { // Use formula.url directly
        formula.url.clone()
    } else {
        // Fallback to creating a URL from the formula name and homepage
        if let Some(homepage) = &formula.homepage {
            if homepage.contains("github.com") {
                // Create a typical GitHub release URL as fallback
                format!("{}/archive/refs/tags/v{}.tar.gz", homepage.trim_end_matches('/'), formula.version) // Common tag format
            } else {
                return Err(SapphireError::Generic(format!("No source URL available for {} and cannot derive from homepage", formula.name)));
            }
        } else {
            return Err(SapphireError::Generic(format!("No source URL available for {}", formula.name)));
        }
    };

    info!("==> Downloading source for {}", formula.name);

    download_url(&url, config).await
}

/// Download a specific URL to the cache
async fn download_url(url: &str, config: &Config) -> Result<PathBuf> {
    // Create cache directory if it doesn't exist
    let cache_dir = PathBuf::from(&config.cache_dir).join("source");
    fs::create_dir_all(&cache_dir)?;

    // Generate a filename from the URL
    let filename = url.split('/').last().unwrap_or("download.tmp"); // Basic fallback filename
    let source_path = cache_dir.join(filename);

    // Skip download if the file already exists and is valid (optional: add SHA check later)
    if source_path.exists() {
        info!("Using cached source: {}", source_path.display());
        return Ok(source_path);
    }

    info!("Downloading {}", url);
    // Download the source
    let client = Client::new();
    let response = client.get(url)
        .send()
        .await
        .map_err(|e| SapphireError::Http(e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_else(|_| "Failed to read body".to_string());
        // Return a specific error type or structure indicating download failure
        // This allows build_from_source to catch it.
        return Err(SapphireError::DownloadError(
            filename.to_string(), // Use filename as identifier
            url.to_string(),
            format!("HTTP status {} - {}", status, body)
        ));
    }

    let content = response.bytes()
        .await
        .map_err(|e| SapphireError::Http(e))?;

    // Write the source to disk
    let mut file = File::create(&source_path)?;
    let mut content_cursor = Cursor::new(content);
    copy(&mut content_cursor, &mut file)?;

    info!("Downloaded source to {}", source_path.display());

    Ok(source_path)
}

/// Build and install formula from source, potentially using fallback build scripts.
pub async fn build_from_source(
    source_path_option: Option<PathBuf>, // Changed to Option<PathBuf>
    formula: &Formula,
    config: &Config,
    all_installed_paths: &[PathBuf],
) -> Result<PathBuf> {
    let install_dir = super::get_formula_cellar_path(formula);
    let formula_name = formula.name();

    // If source_path is provided, proceed with standard build/install
    if let Some(source_path) = source_path_option {
        // --- Handle Single-File Formulas (e.g., ca-certificates) ---
        let recognised_archive_extensions = ["tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz", "zip"];
        let source_extension = source_path.extension().and_then(|s| s.to_str()).unwrap_or("");

        if !recognised_archive_extensions.contains(&source_extension) {
            info!("==> Installing single file formula: {}", formula_name);
            fs::create_dir_all(&install_dir)?; // Ensure install dir exists
            install_single_file(&source_path, formula, &install_dir)?;
            // Write receipt for single file installs too
            super::write_receipt(formula, &install_dir)?;
            return Ok(install_dir);
        }

        // --- Proceed with Archive Extraction and Build ---
        let temp_dir_base = config.cache_dir.join("build-temp");
        fs::create_dir_all(&temp_dir_base)?;
        let temp_dir = tempfile::Builder::new()
            .prefix(&format!("{}-", formula_name))
            .tempdir_in(&temp_dir_base)
            .map_err(|e| SapphireError::Io(e))?
            .into_path(); // Get PathBuf and keep the directory

        info!("==> Extracting source to {}", temp_dir.display());
        extract_archive_strip_components(&source_path, &temp_dir, 1)?;
        let build_dir = temp_dir; // Use the temp dir as the build dir

        info!("==> Building {} from source in {}", formula_name, build_dir.display());

        info!("==> Setting up build environment");
        let sapphire_prefix = build::get_homebrew_prefix();
        let build_env = BuildEnvironment::new(formula, &sapphire_prefix, &config.cellar, all_installed_paths)?;

        detect_and_build(formula, &build_dir, &install_dir, &build_env, all_installed_paths)?;
        super::write_receipt(formula, &install_dir)?;

        if build_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&build_dir) {
                warn!("Warning: Failed to clean up temporary build directory {}: {}", build_dir.display(), e);
            }
        }
        return Ok(install_dir);
    }

    // --- Fallback Logic: source_path_option is None (Download likely failed) ---
    warn!("No source path provided for {}, attempting fallback build.", formula_name);

    // Determine fallback installation prefix (e.g., within cache)
    let fallback_install_prefix = fallback::get_fallback_install_prefix(formula_name, config)?;
    fs::create_dir_all(&fallback_install_prefix)?; // Ensure prefix exists

    let fallback_result = match formula_name {
        "m4" => {
            fallback::build_m4_from_source(config, &fallback_install_prefix).await
        }
        "autoconf" => {
            // Autoconf needs M4. Get M4's install path (either standard or fallback)
            let m4_prefix = get_dependency_prefix("m4", all_installed_paths, config)?;
            fallback::build_autoconf_from_source(config, &fallback_install_prefix, &m4_prefix).await
        }
        "libtool" => {
            // Libtool needs M4 and Autoconf. Get their prefixes.
            let m4_prefix = get_dependency_prefix("m4", all_installed_paths, config)?;
            let autoconf_prefix = get_dependency_prefix("autoconf", all_installed_paths, config)?;
            fallback::build_libtool_from_source(config, &fallback_install_prefix, &m4_prefix, &autoconf_prefix).await
        }
        _ => {
            // If it's not one of the fallback-supported tools, return the original error implicitly
            // (or explicitly if download_source returned a non-DownloadError)
            return Err(SapphireError::Generic(format!("No fallback build available for {}", formula_name)));
        }
    };

    match fallback_result {
        Ok(installed_path) => {
            info!("Fallback build for {} successful. Installed to: {}", formula_name, installed_path.display());
            // IMPORTANT: Link the fallback build into the main prefix
            // This requires simulating `brew link` or having a mechanism to register
            // these fallback builds as if they were normally installed.
            // For now, we'll just return the path where it was built.
            // A more robust solution would involve linking artifacts from `installed_path`
            // into the main prefix or making the main `install_formula_internal` aware
            // of these fallback locations.
            warn!("Fallback build for {} complete. Manual linking or path adjustment might be needed.", formula_name);
            // We should ideally return the *standard* cellar path here, even if the build
            // happened elsewhere, assuming post-build steps would link it.
            // However, the fallback scripts install directly to the given prefix.
            // Let's return the *standard* cellar path to signal expected location,
            // but the actual files are in `fallback_install_prefix`.
             super::write_receipt(formula, &install_dir)?; // Still write receipt to standard location
             Ok(install_dir) // Return standard cellar path
        }
        Err(e) => {
            log::error!("Fallback build for {} failed: {}", formula_name, e);
            Err(SapphireError::InstallError(format!("Fallback build failed for {}: {}", formula_name, e)))
        }
    }
}


/// Helper function to get the installation prefix of a dependency.
/// Checks the provided paths first, then tries the fallback prefix if needed.
fn get_dependency_prefix(dep_name: &str, all_installed_paths: &[PathBuf], config: &Config) -> Result<PathBuf> {
    // 1. Check the list of known installed opt paths
    if let Some(path) = all_installed_paths.iter().find(|p| p.ends_with(dep_name)) {
        debug!("Found dependency {} prefix in provided paths: {}", dep_name, path.display());
        return Ok(path.clone());
    }

    // 2. Check the standard opt path derived from config prefix
    let standard_opt_path = config.prefix.join("opt").join(dep_name);
    if standard_opt_path.exists() {
        debug!("Found dependency {} prefix at standard location: {}", dep_name, standard_opt_path.display());
        return Ok(standard_opt_path);
    }

    // 3. Check the *fallback* installation prefix for this dependency
    let fallback_prefix = fallback::get_fallback_install_prefix(dep_name, config)?;
    if fallback_prefix.exists() {
        debug!("Found dependency {} prefix at fallback location: {}", dep_name, fallback_prefix.display());
        return Ok(fallback_prefix);
    }

    // 4. If not found anywhere, return an error
    log::error!("Required dependency '{}' could not be found in standard paths or fallback location.", dep_name);
    Err(SapphireError::DependencyError(format!(
        "Required dependency '{}' for fallback build not found.", dep_name
    )))
}


/// Install a single file formula by copying it.
fn install_single_file(source_path: &Path, formula: &Formula, install_dir: &Path) -> Result<()> {
    // Determine the target directory within the installation path.
    let target_path = if formula.name == "ca-certificates" {
         // Special case for ca-certs: install to share/ca-certificates/cacert.pem (or similar)
         let share_dir = install_dir.join("share").join(formula.name());
         fs::create_dir_all(&share_dir)?;
         share_dir.join(source_path.file_name().unwrap_or_else(|| std::ffi::OsStr::new("cacert.pem")))
    } else {
        // Default: install into share/{formula_name}/
        let target_share_dir = install_dir.join("share").join(formula.name());
        fs::create_dir_all(&target_share_dir)?;
        let target_file_name = source_path.file_name().ok_or_else(|| SapphireError::Generic("Source path has no filename.".to_string()))?;
        target_share_dir.join(target_file_name)
    };


    info!("Copying {} to {}", source_path.display(), target_path.display());
    fs::copy(source_path, &target_path)?;

    Ok(())
}


/// Detect the build system and use the appropriate build method
fn detect_and_build(
    formula: &Formula, // Pass formula to check name
    build_dir: &Path,
    install_dir: &Path,
    build_env: &BuildEnvironment,
    all_installed_paths: &[PathBuf], // Pass build dep paths for Go bootstrap check
) -> Result<()> {
    // Ensure build directory exists and is accessible
    if !build_dir.exists() { /* ... error handling ... */
         return Err(SapphireError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Build directory does not exist: {}", build_dir.display()),
        )));
    }
    std::fs::read_dir(build_dir).map_err(|e| { /* ... error handling ... */
         SapphireError::Io(std::io::Error::new(
            e.kind(),
            format!("Cannot access build directory {}: {}", build_dir.display(), e),
        ))
    })?;


    // Change to the build directory *before* running any build commands
    let original_cwd = std::env::current_dir()?;
    std::env::set_current_dir(build_dir)?;
    info!("Changed working directory to: {}", build_dir.display());
    let _cwd_guard = CurrentWorkingDirectoryGuard::new(original_cwd); // RAII guard


    // --- Build System Detection Order ---

    // 0. Perl Specific Check (Configure script has different args)
    if formula.name == "perl" && build_dir.join("Configure").exists() {
        return perl_build(build_dir, install_dir, build_env);
    }

    // 1. Autotools (configure.ac needs autoreconf first)
    if build_dir.join("configure.ac").exists() || build_dir.join("configure.in").exists() {
        // Check if autoreconf exists *before* trying to run it
        match which::which_in("autoreconf", build_env.get_path_string(), build_dir) {
            Ok(autoreconf_path) => {
                 // Only run autoreconf if configure doesn't already exist
                 if !build_dir.join("configure").exists() {
                     info!("==> Running autoreconf -fvi");
                     let mut cmd = Command::new(autoreconf_path);
                     cmd.args(["-fvi"]);
                     build_env.apply_to_command(&mut cmd);
                     let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute autoreconf: {}", e)))?;

                     if !output.status.success() { /* ... error handling ... */
                         println!("autoreconf failed with status: {}", output.status);
                         eprintln!("autoreconf stdout:\n{}", String::from_utf8_lossy(&output.stdout));
                         eprintln!("autoreconf stderr:\n{}", String::from_utf8_lossy(&output.stderr));
                         return Err(SapphireError::Generic(format!(
                             "autoreconf failed with status: {}", output.status
                         )));
                     }
                 } else {
                     info!("Skipping autoreconf, configure script already exists.");
                 }
            }
            Err(_) => {
                 // Autoreconf not found, but configure.ac exists. This might be an error,
                 // but some projects ship a pre-generated configure script. Proceed to check for configure.
                 warn!("configure.ac found but autoreconf not found in PATH. Will proceed to check for 'configure' script.");
            }
        }
        // Fall through to configure script check
    }

    // 2. Autotools (configure script)
    let configure_script = build_dir.join("configure");
    if configure_script.exists() {
        if cfg!(unix) { /* ... set executable ... */
             use std::os::unix::fs::PermissionsExt;
             match fs::metadata(&configure_script) {
                Ok(metadata) => {
                    let mut perms = metadata.permissions();
                    perms.set_mode(perms.mode() | 0o111); // Add execute
                    if let Err(e) = fs::set_permissions(&configure_script, perms) {
                        warn!("Warning: Failed to set executable permission on configure script: {}", e);
                    }
                }
                Err(e) => warn!("Warning: Failed to read metadata for configure script: {}", e),
             }
        }
        return configure_and_make(build_dir, install_dir, build_env);
    }

    // 3. CMake
    if build_dir.join("CMakeLists.txt").exists() {
        return cmake_build(build_dir, install_dir, build_env);
    }

    // 4. Meson
    if build_dir.join("meson.build").exists() {
        return meson_build(build_dir, install_dir, build_env);
    }

    // 5. Go
    let go_make_script = build_dir.join("src/make.bash");
    let go_all_script = build_dir.join("src/all.bash");
    if go_make_script.exists() || go_all_script.exists() {
         // Pass all installed paths to handle bootstrap
         return go_build(build_dir, install_dir, build_env, all_installed_paths);
    }

    // 6. Cargo (Rust)
    if build_dir.join("Cargo.toml").exists() {
        return cargo_build(build_dir, install_dir, build_env);
    }

    // 7. Python setup.py
    if build_dir.join("setup.py").exists() {
        return python_build(build_dir, install_dir, build_env);
    }

    // 8. Simple Makefile
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
    _build_dir: &Path, // Is CWD
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Running ./configure --prefix={}", install_dir.display());

    let mut cmd = Command::new("./configure");
    cmd.arg(format!("--prefix={}", install_dir.display()));
    // cmd.args(&["--disable-dependency-tracking", "--disable-silent-rules"]); // Optional common flags
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute configure: {}", e)))?;

    if !output.status.success() { /* ... error handling ... */
        println!("Configure failed with status: {}", output.status);
        eprintln!("Configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        let config_log_path = PathBuf::from("config.log"); // configure usually creates this in CWD
        if config_log_path.exists() {
             eprintln!("--- config.log ---");
             if let Ok(content) = fs::read_to_string(&config_log_path) {
                 let lines: Vec<&str> = content.lines().rev().take(50).collect();
                 for line in lines.iter().rev() { eprintln!("{}", line); }
             }
             eprintln!("--- end config.log ---");
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
    let make_exe = which::which_in("make", build_env.get_path_string(), Path::new(".")) // Check in CWD/PATH
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
          // Optionally log stdout/stderr even on success for debugging
          // debug!("Make stdout:\n{}", String::from_utf8_lossy(&output.stdout));
          // debug!("Make stderr:\n{}", String::from_utf8_lossy(&output.stderr));
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
         // debug!("Make install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
         // debug!("Make install stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(())
}

/// Build Perl using its Configure script
fn perl_build(
    _build_dir: &Path, // Should be CWD already
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Building with Perl Configure script...");

    let configure_script = PathBuf::from("Configure"); // Relative to CWD
    if !configure_script.exists() {
        return Err(SapphireError::BuildEnvError(format!("Perl Configure script not found at {}", configure_script.display())));
    }

    // Find sh (needed to run Configure)
    let sh_exe = which::which_in("sh", build_env.get_path_string(), Path::new("."))
        .map_err(|_| SapphireError::BuildEnvError("sh command not found in build environment PATH (needed for Perl Configure).".to_string()))?;


    let mut cmd = Command::new(sh_exe);
    cmd.arg(configure_script); // Pass Configure script path to sh
    cmd.arg("-des"); // Defaults, non-interactive, silent
    cmd.arg(format!("-Dprefix={}", install_dir.display()));
    // cmd.args(&["-D", "usethreads"]); // Example

    build_env.apply_to_command(&mut cmd);
    info!("Running Perl Configure: {:?}", cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute Perl Configure: {}", e)))?;


    if !output.status.success() {
        println!("Perl Configure failed with status: {}", output.status);
        eprintln!("Perl Configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Perl Configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(format!(
            "Perl Configure failed with status: {}", output.status
        )));
    } else {
         debug!("Perl Configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
         debug!("Perl Configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    }

    // Run make
    info!("==> Running make for Perl");
    let make_exe = which::which_in("make", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("make command not found in build environment PATH.".to_string()))?;
    let mut cmd = Command::new(make_exe.clone());
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make for Perl: {}", e)))?;

    if !output.status.success() {
        println!("Perl make failed with status: {}", output.status);
        eprintln!("Perl make stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Perl make stderr:\n{}", String::from_utf8_lossy(&output.stderr));
         return Err(SapphireError::Generic(format!(
            "Perl make failed with status: {}", output.status
        )));
    } else {
         info!("Perl make completed successfully.");
    }


    // Run make test (Recommended for Perl builds)
    info!("==> Running make test for Perl (may take a while)...");
    let mut cmd = Command::new(make_exe.clone());
    cmd.arg("test");
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make test for Perl: {}", e)))?;

    if !output.status.success() {
        // Don't necessarily fail the build if tests fail, but log it verbosely
        println!("Perl 'make test' failed with status: {}. Continuing installation.", output.status);
        eprintln!("Perl make test stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Perl make test stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    } else {
        info!("Perl 'make test' passed.");
    }


    // Run make install
    info!("==> Running make install for Perl");
    let mut cmd = Command::new(make_exe);
    cmd.arg("install");
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make install for Perl: {}", e)))?;

    if !output.status.success() {
        println!("Perl make install failed with status: {}", output.status);
        eprintln!("Perl make install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Perl make install stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(format!(
            "Perl make install failed with status: {}", output.status
        )));
    } else {
         info!("Perl make install completed successfully.");
    }

    Ok(())
}


/// Build with CMake
fn cmake_build(
    source_dir: &Path, // Source dir (which is also CWD)
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Building with CMake");
    let build_subdir_name = "sapphire-cmake-build";
    let build_subdir = source_dir.join(build_subdir_name);
    fs::create_dir_all(&build_subdir).map_err(|e| SapphireError::Io(e))?;

     let cmake_exe = which::which_in("cmake", build_env.get_path_string(), Path::new("."))
        .map_err(|_| SapphireError::BuildEnvError("cmake command not found in build environment PATH.".to_string()))?;
     info!("==> Running {} .. -DCMAKE_INSTALL_PREFIX={}", cmake_exe.display(), install_dir.display());

    let mut cmd = Command::new(cmake_exe);
    cmd.arg("..")
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", install_dir.display()))
        .args(&[
            "-DCMAKE_FIND_FRAMEWORK=LAST",
            "-DCMAKE_VERBOSE_MAKEFILE=ON",
            "-Wno-dev",
            // "-DCMAKE_BUILD_TYPE=Release",
        ])
        .current_dir(&build_subdir);
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute cmake: {}", e)))?;

    if !output.status.success() {
        println!("CMake configure failed with status: {}", output.status);
        eprintln!("CMake configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("CMake configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(format!(
            "CMake configure failed with status: {}", output.status
        )));
    } else {
         debug!("CMake configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
         debug!("CMake configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    }

    info!("==> Running make install in {}", build_subdir.display());
     let make_exe = which::which_in("make", build_env.get_path_string(), Path::new("."))
        .map_err(|_| SapphireError::BuildEnvError("make command not found in build environment PATH.".to_string()))?;

    let mut cmd = Command::new(make_exe);
    cmd.arg("install")
        .current_dir(&build_subdir);
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make install (CMake): {}", e)))?;

    if !output.status.success() {
        println!("CMake make install failed with status: {}", output.status);
        eprintln!("CMake make install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("CMake make install stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(format!(
            "CMake make install failed with status: {}", output.status
        )));
    } else {
         debug!("CMake install completed successfully.");
    }

    Ok(())
}

/// Build with Meson
fn meson_build(
    source_dir: &Path, // Source dir (which is also CWD)
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
     info!("==> Building with Meson");
     let build_subdir_name = "sapphire-meson-build";
     let build_subdir = source_dir.join(build_subdir_name);

     let meson_exe = which::which_in("meson", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("meson command not found in build environment PATH.".to_string()))?;
     info!("==> Running {} setup --prefix={} {}", meson_exe.display(), install_dir.display(), build_subdir.display());

     let mut cmd_setup = Command::new(&meson_exe);
     cmd_setup.arg("setup")
         .arg(format!("--prefix={}", install_dir.display()))
         // .arg("--buildtype=release")
         .arg(&build_subdir) // Build dir
         .arg(".");          // Source dir
     build_env.apply_to_command(&mut cmd_setup);
     let output_setup = cmd_setup.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute meson setup: {}", e)))?;

    if !output_setup.status.success() {
        println!("Meson setup failed with status: {}", output_setup.status);
        eprintln!("Meson setup stdout:\n{}", String::from_utf8_lossy(&output_setup.stdout));
        eprintln!("Meson setup stderr:\n{}", String::from_utf8_lossy(&output_setup.stderr));
        return Err(SapphireError::Generic(format!(
             "Meson setup failed with status: {}", output_setup.status
         )));
     } else {
          debug!("Meson setup stdout:\n{}", String::from_utf8_lossy(&output_setup.stdout));
          debug!("Meson setup stderr:\n{}", String::from_utf8_lossy(&output_setup.stderr));
     }


     // Meson install (implicitly uses ninja)
     let _ninja_exe = which::which_in("ninja", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("ninja command not found (needed for meson install). Ensure ninja is installed and in build dependencies.".to_string()))?;
     info!("==> Running {} install -C {}", meson_exe.display(), build_subdir.display());

     let mut cmd_install = Command::new(&meson_exe);
     cmd_install.arg("install")
         .arg("-C") // Specify build directory
         .arg(&build_subdir);
     build_env.apply_to_command(&mut cmd_install);
     let output_install = cmd_install.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute meson install: {}", e)))?;


     if !output_install.status.success() {
        println!("Meson install failed with status: {}", output_install.status);
        eprintln!("Meson install stdout:\n{}", String::from_utf8_lossy(&output_install.stdout));
        eprintln!("Meson install stderr:\n{}", String::from_utf8_lossy(&output_install.stderr));
         return Err(SapphireError::Generic(format!(
             "Meson install failed with status: {}", output_install.status
         )));
     } else {
          debug!("Meson install completed successfully.");
     }

    Ok(())
}

/// Build Go project
fn go_build(
    build_dir: &Path, // Is CWD
    install_dir: &Path,
    build_env: &BuildEnvironment,
    all_installed_paths: &[PathBuf], // Changed parameter name
) -> Result<()> {
    info!("==> Building with Go build script");

    // Determine which script to use
    let script_path = if build_dir.join("src/make.bash").exists() {
        build_dir.join("src/make.bash")
    } else if build_dir.join("src/all.bash").exists() {
        build_dir.join("src/all.bash")
    } else {
        return Err(SapphireError::Generic(
            "Go build script (src/make.bash or src/all.bash) not found.".to_string()
        ));
    };

     // Find bash (needed to run the script)
     let bash_exe = which::which_in("bash", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("bash command not found in build environment PATH (needed for Go build script).".to_string()))?;
     info!("==> Running {} {}", bash_exe.display(), script_path.display());

    // Ensure script is executable
    if cfg!(unix) { /* ... set executable ... */
         use std::os::unix::fs::PermissionsExt;
         match fs::metadata(&script_path) {
            Ok(metadata) => {
                let mut perms = metadata.permissions();
                perms.set_mode(perms.mode() | 0o111); // Add execute
                if let Err(e) = fs::set_permissions(&script_path, perms) {
                    warn!("Warning: Failed to set executable permission on Go build script: {}", e);
                }
            }
            Err(e) => warn!("Warning: Failed to read metadata for Go build script: {}", e),
         }
    }

    // --- Go Bootstrap Handling ---
    let mut go_build_specific_env = HashMap::new(); // Start with empty map for cmd specific vars
    let bootstrap_go_path = all_installed_paths.iter() // Check ALL installed paths
        .find(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("go@")).unwrap_or(false));

    if let Some(path) = bootstrap_go_path {
         info!("Found bootstrap Go path: {}", path.display());
         go_build_specific_env.insert("GOROOT_BOOTSTRAP".to_string(), path.to_string_lossy().to_string());
    } else if build_env.get_var("GOROOT_BOOTSTRAP").is_none() {
         // If bootstrap path not found in deps AND not already set in env, build might fail.
         warn!("GOROOT_BOOTSTRAP not set and no bootstrap Go dependency path provided/found. Go build might fail if required.");
    }
    // --- End Go Bootstrap Handling ---


    // Run the Go build script
    let mut cmd = Command::new(bash_exe);
    cmd.arg(&script_path); // Pass the script path as argument to bash
    cmd.current_dir(build_dir.join("src")); // Run script from within src/
    // Apply the main build env first, then overwrite/add specific vars like GOROOT_BOOTSTRAP
    build_env.apply_to_command(&mut cmd); // Applies vars from BuildEnvironment
    cmd.envs(&go_build_specific_env);      // Adds/overwrites GOROOT_BOOTSTRAP if found

    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute Go build script {}: {}", script_path.display(), e)))?;


    if !output.status.success() {
        println!("Go build script failed with status: {}", output.status);
        eprintln!("Go build stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Go build stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(format!(
            "Go build script failed with status: {}", output.status
        )));
    } else {
         debug!("Go build stdout:\n{}", String::from_utf8_lossy(&output.stdout));
         debug!("Go build stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    }

    // Install artifacts
    info!("==> Installing Go build artifacts to {}", install_dir.display());
    fs::create_dir_all(install_dir).map_err(SapphireError::Io)?;

    let go_output_bin_dir = build_dir.join("bin");
    let go_output_pkg_dir = build_dir.join("pkg");

    if go_output_bin_dir.is_dir() {
        info!("Copying contents from {}/bin", build_dir.display());
        let target_bin = install_dir.join("bin");
        fs::create_dir_all(&target_bin).map_err(SapphireError::Io)?;
        copy_directory_contents(&go_output_bin_dir, &target_bin)?;
    } else {
        warn!("Go output bin directory not found: {}", go_output_bin_dir.display());
    }

    if go_output_pkg_dir.is_dir() {
        info!("Copying contents from {}/pkg", build_dir.display());
        let target_pkg = install_dir.join("pkg");
        fs::create_dir_all(&target_pkg).map_err(SapphireError::Io)?;
        copy_directory_contents(&go_output_pkg_dir, &target_pkg)?;
    } else {
         debug!("Go output pkg directory not found: {}", go_output_pkg_dir.display());
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
            fs::copy(&src_path, &dest_path).map_err(SapphireError::Io)?;
        }
    }
    Ok(())
}


/// Build with Cargo
fn cargo_build(
    _source_dir: &Path, // Is CWD
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
     info!("==> Building with Cargo");
     let cargo_exe = which::which_in("cargo", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("cargo command not found in build environment PATH.".to_string()))?;

     info!("==> Running {} install --path . --root {}", cargo_exe.display(), install_dir.display());
     let mut cmd = Command::new(cargo_exe);
     cmd.arg("install")
         .arg("--path")
         .arg(".") // Install the crate in the current directory
         .arg("--root")
         .arg(install_dir); // Install into the formula's cellar directory
     build_env.apply_to_command(&mut cmd);
     let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute cargo install: {}", e)))?;


     if !output.status.success() {
        println!("Cargo install failed with status: {}", output.status);
        eprintln!("Cargo install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Cargo install stderr:\n{}", String::from_utf8_lossy(&output.stderr));
         return Err(SapphireError::Generic(format!(
             "Cargo install failed with status: {}", output.status
         )));
     } else {
          debug!("Cargo install completed successfully.");
     }

     Ok(())
}

/// Build with Python setup.py
fn python_build(
    _source_dir: &Path, // Is CWD
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Building with Python setup.py");
     let python_exe = which::which_in("python3", build_env.get_path_string(), Path::new("."))
         .or_else(|_| which::which_in("python", build_env.get_path_string(), Path::new(".")))
         .map_err(|_| SapphireError::BuildEnvError("python3 or python command not found in build environment PATH.".to_string()))?;

    info!("==> Running {} setup.py install --prefix={}", python_exe.display(), install_dir.display());
    let mut cmd = Command::new(python_exe);
    cmd.arg("setup.py")
        .arg("install")
        .arg(format!("--prefix={}", install_dir.display()));
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute python setup.py install: {}", e)))?;

    if !output.status.success() {
        println!("Python setup.py install failed with status: {}", output.status);
        eprintln!("Python install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Python install stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(SapphireError::Generic(format!(
            "Python setup.py install failed with status: {}", output.status
        )));
    } else {
         debug!("Python install completed successfully.");
    }

    Ok(())
}

/// Build with a simple Makefile
fn simple_make(
    _build_dir: &Path, // Is CWD
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
     info!("==> Building with simple Makefile");
      let make_exe = which::which_in("make", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("make command not found in build environment PATH.".to_string()))?;


     info!("==> Running make");
     let mut cmd_make = Command::new(make_exe.clone()); // Clone for install command
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
    // Common convention for simple Makefiles: pass PREFIX variable
    cmd_install.env("PREFIX", install_dir.as_os_str()); // Set PREFIX env var as well/instead?
    // Or pass as argument: cmd_install.arg(format!("PREFIX={}", install_dir.display()));
    build_env.apply_to_command(&mut cmd_install);
    let output_install = cmd_install.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make install (simple): {}", e)))?;

     if !output_install.status.success() {
         // Some Makefiles might not have an install target, this might be okay
         warn!("'make install' failed with status {}. Formula might be installed correctly if it doesn't use a standard install target or PREFIX variable.", output_install.status);
         // Check if install_dir/bin or install_dir/lib has files?
         let bin_dir = install_dir.join("bin");
         let lib_dir = install_dir.join("lib");
         let bin_exists = bin_dir.exists() && fs::read_dir(&bin_dir).map(|mut d| d.next().is_some()).unwrap_or(false);
         let lib_exists = lib_dir.exists() && fs::read_dir(&lib_dir).map(|mut d| d.next().is_some()).unwrap_or(false);
         if !bin_exists && !lib_exists {
              println!("Make install failed with status: {} and no files found in {}/bin or {}/lib",
                  output_install.status, install_dir.display(), install_dir.display());
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
            println!(
                "Error: Failed to restore original working directory to {}: {}",
                self.original_cwd.display(),
                e
            );
        } else {
             debug!("Restored working directory to: {}", self.original_cwd.display());
        }
    }
}