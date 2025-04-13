use crate::utils::error::{SapphireError, Result};
use crate::model::formula::Formula;
use crate::utils::config::Config;
use crate::build::env::BuildEnvironment; // Import BuildEnvironment
use crate::build; // Import the build module itself for get_homebrew_prefix
use crate::build::extract_archive_strip_components; // Import the specific function
// Removed: use crate::build::fallback;
use std::ffi::OsStr; // Import OsStr for default filename
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
                // Adjust tag format if needed (e.g., some might not have 'v')
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
        // Optionally verify checksum here
        // if verify_source_checksum(&source_path, formula.source_sha256()).is_ok() {
             return Ok(source_path);
        // } else {
        //     warn!("Cached source checksum mismatch for {}. Redownloading.", source_path.display());
        //     fs::remove_file(&source_path)?;
        // }
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
        // Return a standard error, no need for the specific DownloadError now
        return Err(SapphireError::Generic(format!(
            "Download failed for '{}' from {}: HTTP status {} - {}",
            filename, url, status, body
        )));
    }

    let content = response.bytes()
        .await
        .map_err(|e| SapphireError::Http(e))?;

    // Write the source to disk
    // Consider writing to temp file then renaming to avoid partial downloads
    let temp_dl_path = source_path.with_extension("download_tmp");
    {
        let mut file = File::create(&temp_dl_path)?;
        let mut content_cursor = Cursor::new(content);
        copy(&mut content_cursor, &mut file)?;
    }
    fs::rename(&temp_dl_path, &source_path)?;


    info!("Downloaded source to {}", source_path.display());

    // Optionally verify checksum after download
    // verify_source_checksum(&source_path, formula.source_sha256())?;

    Ok(source_path)
}

/// Build and install formula from downloaded source archive or file.
pub async fn build_from_source(
    source_path: &Path, // Reverted signature: Take Path reference directly
    formula: &Formula,
    config: &Config,
    all_installed_paths: &[PathBuf],
) -> Result<PathBuf> {
    let install_dir = super::get_formula_cellar_path(formula);
    let formula_name = formula.name();

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
    // Use tempfile::Builder correctly for creating the temp directory
    let temp_dir = tempfile::Builder::new()
        .prefix(&format!("{}-", formula_name))
        .tempdir_in(&temp_dir_base)
        .map_err(|e| SapphireError::Io(e))?
        .into_path(); // Get PathBuf and keep the directory

    info!("==> Extracting source {} to {}", source_path.display(), temp_dir.display());
    extract_archive_strip_components(&source_path, &temp_dir, 1)?;
    let build_dir = temp_dir; // Use the temp dir as the build dir

    info!("==> Building {} from source in {}", formula_name, build_dir.display());

    info!("==> Setting up build environment");
    let sapphire_prefix = build::get_homebrew_prefix();
    let build_env = BuildEnvironment::new(formula, &sapphire_prefix, &config.cellar, all_installed_paths)?;

    detect_and_build(formula, &build_dir, &install_dir, &build_env, all_installed_paths)?;
    super::write_receipt(formula, &install_dir)?;

    // Cleanup temp build directory
    if build_dir.exists() {
        if let Err(e) = fs::remove_dir_all(&build_dir) {
            warn!("Warning: Failed to clean up temporary build directory {}: {}", build_dir.display(), e);
        }
    }
    Ok(install_dir) // Return standard cellar path

    // --- Fallback Logic Removed ---
}


// --- Removed get_dependency_prefix function ---


/// Install a single file formula by copying it.
fn install_single_file(source_path: &Path, formula: &Formula, install_dir: &Path) -> Result<()> {
    // Determine the target directory within the installation path.
    let target_path = if formula.name == "ca-certificates" {
         // Special case for ca-certs: install to share/ca-certificates/cacert.pem (or similar)
         let share_dir = install_dir.join("share").join(formula.name());
         fs::create_dir_all(&share_dir)?;
         // Use OsStr::new for default filename if source_path doesn't have one
         share_dir.join(source_path.file_name().unwrap_or_else(|| OsStr::new("cacert.pem")))
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
    if !build_dir.exists() {
         return Err(SapphireError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Build directory does not exist: {}", build_dir.display()),
        )));
    }
    // Check read permissions
     fs::read_dir(build_dir).map_err(|e| {
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
        // Use build_dir as the relative point for which_in
        match which::which_in("autoreconf", build_env.get_path_string(), build_dir) {
            Ok(autoreconf_path) => {
                 // Only run autoreconf if configure doesn't already exist
                 if !build_dir.join("configure").exists() {
                     info!("==> Running autoreconf -fvi");
                     let mut cmd = Command::new(autoreconf_path);
                     cmd.args(["-fvi"]);
                     build_env.apply_to_command(&mut cmd); // Apply env vars
                     let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute autoreconf: {}", e)))?;

                     if !output.status.success() {
                         println!("autoreconf failed with status: {}", output.status);
                         eprintln!("autoreconf stdout:\n{}", String::from_utf8_lossy(&output.stdout));
                         eprintln!("autoreconf stderr:\n{}", String::from_utf8_lossy(&output.stderr));
                         return Err(SapphireError::Generic(format!(
                             "autoreconf failed with status: {}", output.status
                         )));
                     } else {
                          debug!("Autoreconf stdout:\n{}", String::from_utf8_lossy(&output.stdout));
                          debug!("Autoreconf stderr:\n{}", String::from_utf8_lossy(&output.stderr));
                     }
                 } else {
                     info!("Skipping autoreconf, configure script already exists.");
                 }
            }
            Err(_) => {
                 warn!("configure.ac found but autoreconf not found in PATH. Will proceed to check for 'configure' script.");
            }
        }
        // Fall through to configure script check
    }

    // 2. Autotools (configure script)
    let configure_script = build_dir.join("configure");
    if configure_script.exists() {
        // Ensure script is executable (important!)
        if cfg!(unix) {
             use std::os::unix::fs::PermissionsExt;
             match fs::metadata(&configure_script) {
                Ok(metadata) => {
                    let mut perms = metadata.permissions();
                    let original_mode = perms.mode();
                    if original_mode & 0o100 == 0 { // Check if user execute bit is NOT set
                         perms.set_mode(original_mode | 0o100); // Add user execute
                         if let Err(e) = fs::set_permissions(&configure_script, perms) {
                             warn!("Warning: Failed to set executable permission on configure script: {}", e);
                         } else {
                             debug!("Set configure script executable.");
                         }
                    }
                }
                Err(e) => warn!("Warning: Failed to read metadata for configure script: {}", e),
             }
        }
        // Pass build_dir explicitly, even though we changed CWD, some scripts might need it?
        // Or rely on CWD change only. Let's rely on CWD for now.
        return configure_and_make(install_dir, build_env);
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
        return cargo_build(install_dir, build_env);
    }

    // 7. Python setup.py
    if build_dir.join("setup.py").exists() {
        return python_build(install_dir, build_env);
    }

    // 8. Simple Makefile
    if build_dir.join("Makefile").exists() || build_dir.join("makefile").exists() {
        return simple_make(install_dir, build_env);
    }

    // If we get here, we couldn't determine the build system
    Err(SapphireError::Generic(format!(
        "Could not determine build system for {}",
        build_dir.display()
    )))
}

/// Configure and build with autotools (./configure && make && make install)
fn configure_and_make(
    // Removed _build_dir parameter, rely on CWD
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Running ./configure --prefix={}", install_dir.display());

    let mut cmd = Command::new("./configure"); // Script should be in CWD now
    cmd.arg(format!("--prefix={}", install_dir.display()));
    // Add common flags often used by Homebrew
    cmd.args(&["--disable-dependency-tracking", "--disable-silent-rules"]);
    build_env.apply_to_command(&mut cmd);
    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute configure: {}", e)))?;

    if !output.status.success() {
        println!("Configure failed with status: {}", output.status);
        eprintln!("Configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        let config_log_path = PathBuf::from("config.log"); // configure usually creates this in CWD
        if config_log_path.exists() {
             eprintln!("--- Last 50 lines of config.log ---");
             if let Ok(content) = fs::read_to_string(&config_log_path) {
                 for line in content.lines().rev().take(50).collect::<Vec<_>>().iter().rev() {
                     eprintln!("{}", line);
                 }
             }
             eprintln!("--- End config.log ---");
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
    // Use which_in relative to CWD '.'
    let make_exe = which::which_in("make", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("make command not found in build environment PATH.".to_string()))?;
    let mut cmd = Command::new(make_exe.clone());
    build_env.apply_to_command(&mut cmd); // Applies MAKEFLAGS etc.
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
         // Optionally log make output even on success
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
    // cmd.args(&["-D", "usethreads"]); // Example flags if needed

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
    //info!("==> Running make test for Perl (may take a while)...");
    //let mut cmd = Command::new(make_exe.clone());
    //cmd.arg("test");
    //build_env.apply_to_command(&mut cmd);
    //let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make test for Perl: {}", e)))?;
    //
    //if !output.status.success() {
    //    // Don't necessarily fail the build if tests fail, but log it verbosely
    //    warn!("Perl 'make test' failed with status: {}. Continuing installation.", output.status);
    //    // Optionally print full output on test failure
    //    // eprintln!("Perl make test stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    //    // eprintln!("Perl make test stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    //} else {
    //    info!("Perl 'make test' passed.");
    //}


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
    source_dir: &Path, // Source dir (which is also CWD, but needed for build subdir path)
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Building with CMake");
    let build_subdir_name = "sapphire-cmake-build";
    let build_subdir = source_dir.join(build_subdir_name); // Build in a subdirectory
    fs::create_dir_all(&build_subdir).map_err(|e| SapphireError::Io(e))?;

     let cmake_exe = which::which_in("cmake", build_env.get_path_string(), Path::new(".")) // Check CWD/PATH
        .map_err(|_| SapphireError::BuildEnvError("cmake command not found in build environment PATH.".to_string()))?;
     info!("==> Running cmake configuration in {}", build_subdir.display());

    let mut cmd = Command::new(cmake_exe);
    cmd.arg("..") // Point CMake to the parent directory (source)
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", install_dir.display()))
        .args(&[
            "-DCMAKE_FIND_FRAMEWORK=LAST", // Standard Homebrew flags
            "-DCMAKE_VERBOSE_MAKEFILE=ON",
            "-Wno-dev",
            // Often useful for release builds
            // "-DCMAKE_BUILD_TYPE=Release",
            // You might need to add formula-specific CMake args here
            // e.g., -DBUILD_SHARED_LIBS=ON
        ])
        .current_dir(&build_subdir); // Run cmake *from* the build subdirectory
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
        .current_dir(&build_subdir); // Run make install *from* the build subdirectory
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
     let build_subdir = source_dir.join(build_subdir_name); // Build in subdirectory

     let meson_exe = which::which_in("meson", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("meson command not found in build environment PATH.".to_string()))?;
     // Meson setup command structure: meson setup [options] <build directory> <source directory>
     info!("==> Running meson setup: {} setup --prefix={} {} {}",
           meson_exe.display(), install_dir.display(), build_subdir.display(), source_dir.display());

     let mut cmd_setup = Command::new(&meson_exe);
     cmd_setup.arg("setup")
         .arg(format!("--prefix={}", install_dir.display()))
         // Common options
         .arg("--buildtype=release")
         .arg("--libdir=lib") // Ensure libraries go into prefix/lib
         // Add formula-specific options here if needed: .arg("-Doption=value")
         .arg(&build_subdir) // Build directory argument
         .arg(".");          // Source directory argument (current dir)
     // Apply env vars AFTER setting args
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


     // Meson install (implicitly uses ninja found via PATH)
     let _ninja_exe = which::which_in("ninja", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("ninja command not found (needed for meson install). Ensure ninja is installed and in build dependencies.".to_string()))?;
     info!("==> Running meson install -C {}", build_subdir.display());

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
    all_installed_paths: &[PathBuf],
) -> Result<()> {
    info!("==> Building with Go build script");

    // Determine which script to use and where it should be run from
    let (script_to_run, run_in_dir) = if build_dir.join("src/make.bash").exists() {
        (build_dir.join("src/make.bash"), build_dir.join("src"))
    } else if build_dir.join("src/all.bash").exists() {
        (build_dir.join("src/all.bash"), build_dir.join("src"))
    } else {
        // Some Go projects might just have build instructions in root
        // This is a guess, adjust if needed
        return Err(SapphireError::Generic(
            "Go build script (src/make.bash or src/all.bash) not found.".to_string()
        ));
    };

     // Find bash (needed to run the script)
     let bash_exe = which::which_in("bash", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("bash command not found in build environment PATH (needed for Go build script).".to_string()))?;
     info!("==> Running {} {} in {}", bash_exe.display(), script_to_run.display(), run_in_dir.display());

    // Ensure script is executable
    if cfg!(unix) {
         use std::os::unix::fs::PermissionsExt;
         match fs::metadata(&script_to_run) {
            Ok(metadata) => {
                let mut perms = metadata.permissions();
                let original_mode = perms.mode();
                 if original_mode & 0o100 == 0 { // Check if user execute bit is NOT set
                     perms.set_mode(original_mode | 0o100); // Add user execute
                     if let Err(e) = fs::set_permissions(&script_to_run, perms) {
                         warn!("Warning: Failed to set executable permission on Go build script: {}", e);
                     } else {
                         debug!("Set Go build script executable.");
                     }
                 }
            }
            Err(e) => warn!("Warning: Failed to read metadata for Go build script: {}", e),
         }
    }

    // --- Go Bootstrap Handling ---
    let mut go_build_specific_env = HashMap::new();
    // Find a dependency path ending in 'go' or 'go@x.y'
    let bootstrap_go_path = all_installed_paths.iter()
        .find(|p| p.file_name().map_or(false, |n| n == "go" || n.to_string_lossy().starts_with("go@")));

    if let Some(path) = bootstrap_go_path {
         // We need the GOROOT, which is usually the opt path itself for Go installs
         info!("Found bootstrap Go path: {}", path.display());
         go_build_specific_env.insert("GOROOT_BOOTSTRAP".to_string(), path.to_string_lossy().to_string());
    } else if build_env.get_var("GOROOT_BOOTSTRAP").is_none() {
         warn!("GOROOT_BOOTSTRAP not set and no Go dependency path found. Go build might fail if required.");
    }
    // --- End Go Bootstrap Handling ---


    // Run the Go build script
    let mut cmd = Command::new(bash_exe);
    cmd.arg(&script_to_run); // Pass the script path as argument to bash
    cmd.current_dir(&run_in_dir); // Run script from the correct directory
    // Apply the main build env first, then overwrite/add specific vars like GOROOT_BOOTSTRAP
    build_env.apply_to_command(&mut cmd); // Applies vars from BuildEnvironment
    cmd.envs(&go_build_specific_env);      // Adds/overwrites GOROOT_BOOTSTRAP if found

    let output = cmd.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute Go build script {}: {}", script_to_run.display(), e)))?;


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

    // --- Install Go Artifacts ---
    // Go build scripts usually place outputs in the main source directory's `bin` and `pkg`
    info!("==> Installing Go build artifacts to {}", install_dir.display());
    fs::create_dir_all(install_dir).map_err(SapphireError::Io)?;

    // Source 'bin' and 'pkg' are relative to the original build_dir (parent of run_in_dir)
    let go_output_bin_dir = build_dir.join("bin");
    let go_output_pkg_dir = build_dir.join("pkg");

    // Target directories in the Cellar
    let target_bin_dir = install_dir.join("bin");
    let target_pkg_dir = install_dir.join("pkg"); // Often contains platform-specific subdirs

    if go_output_bin_dir.is_dir() {
        info!("Copying contents from {} to {}", go_output_bin_dir.display(), target_bin_dir.display());
        fs::create_dir_all(&target_bin_dir).map_err(SapphireError::Io)?;
        copy_directory_contents(&go_output_bin_dir, &target_bin_dir)?;
    } else {
        warn!("Go output bin directory not found: {}", go_output_bin_dir.display());
    }

    // Go's pkg dir might contain architecture-specific subdirs, handle that
    if go_output_pkg_dir.is_dir() {
         info!("Copying contents from {} to {}", go_output_pkg_dir.display(), target_pkg_dir.display());
         fs::create_dir_all(&target_pkg_dir).map_err(SapphireError::Io)?;
         copy_directory_contents(&go_output_pkg_dir, &target_pkg_dir)?;
    } else {
         debug!("Go output pkg directory not found: {}", go_output_pkg_dir.display());
    }

    // Go might also install source files into the prefix under 'src'
    let go_output_src_dir = build_dir.join("src");
    if go_output_src_dir.is_dir() {
        let target_src_dir = install_dir.join("src");
         info!("Copying contents from {} to {}", go_output_src_dir.display(), target_src_dir.display());
         fs::create_dir_all(&target_src_dir).map_err(SapphireError::Io)?;
         copy_directory_contents(&go_output_src_dir, &target_src_dir)?;
    } else {
         debug!("Go output src directory not found: {}", go_output_src_dir.display());
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
             // Ensure parent directory exists before copying file
             if let Some(parent) = dest_path.parent() {
                 fs::create_dir_all(parent).map_err(SapphireError::Io)?;
             }
             fs::copy(&src_path, &dest_path).map_err(SapphireError::Io)?;
        }
        // Ignore symlinks or other file types for now
    }
    Ok(())
}


/// Build with Cargo
fn cargo_build(
    // Removed _source_dir, rely on CWD
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
     info!("==> Building with Cargo");
     let cargo_exe = which::which_in("cargo", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("cargo command not found in build environment PATH.".to_string()))?;

     // Use `cargo install` which builds and copies binaries to the specified --root
     // Ensure --path . to specify the current directory as the crate to install
     info!("==> Running {} install --path . --root {}", cargo_exe.display(), install_dir.display());
     let mut cmd = Command::new(cargo_exe);
     cmd.arg("install")
         .arg("--path")
         .arg(".") // Install the crate in the current directory (CWD)
         .arg("--root")
         .arg(install_dir); // Install into the formula's cellar directory
     // Add --locked if needed for reproducibility? Maybe not for general builds.
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
    // Removed _source_dir, rely on CWD
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
    info!("==> Building with Python setup.py");
    // Prefer python3 if available
     let python_exe = which::which_in("python3", build_env.get_path_string(), Path::new("."))
         .or_else(|_| which::which_in("python", build_env.get_path_string(), Path::new(".")))
         .map_err(|_| SapphireError::BuildEnvError("python3 or python command not found in build environment PATH.".to_string()))?;

    // Standard Python installation command
    info!("==> Running {} setup.py install --prefix={}", python_exe.display(), install_dir.display());
    let mut cmd = Command::new(python_exe);
    cmd.arg("setup.py")
        .arg("install")
        .arg(format!("--prefix={}", install_dir.display()));
    // Apply build env (might include PYTHONUSERBASE or other relevant vars)
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
    // Removed _build_dir, rely on CWD
    install_dir: &Path,
    build_env: &BuildEnvironment,
) -> Result<()> {
     info!("==> Building with simple Makefile");
      let make_exe = which::which_in("make", build_env.get_path_string(), Path::new("."))
         .map_err(|_| SapphireError::BuildEnvError("make command not found in build environment PATH.".to_string()))?;


     info!("==> Running make");
     let mut cmd_make = Command::new(make_exe.clone()); // Clone for install command
     build_env.apply_to_command(&mut cmd_make); // Includes MAKEFLAGS
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
    cmd_install.arg(format!("PREFIX={}", install_dir.display())); // Pass as make variable
    // Sometimes DESTDIR is used instead or in combination
    // cmd_install.arg(format!("DESTDIR={}", install_dir.display()));
    build_env.apply_to_command(&mut cmd_install); // Apply env vars
    let output_install = cmd_install.output().map_err(|e| SapphireError::CommandExecError(format!("Failed to execute make install (simple): {}", e)))?;

     if !output_install.status.success() {
         // Some Makefiles might not have an install target, this might be okay
         warn!("'make install' failed with status {}. Formula might be installed correctly if it doesn't use a standard install target or PREFIX variable.", output_install.status);
         // Check if install_dir/bin or install_dir/lib has files as a basic heuristic
         let bin_dir = install_dir.join("bin");
         let lib_dir = install_dir.join("lib");
         let bin_exists = bin_dir.exists() && fs::read_dir(&bin_dir).map(|mut d| d.next().is_some()).unwrap_or(false);
         let lib_exists = lib_dir.exists() && fs::read_dir(&lib_dir).map(|mut d| d.next().is_some()).unwrap_or(false);
         if !bin_exists && !lib_exists {
             // If install failed AND nothing seems to be installed, it's likely a real error
              println!("Make install failed with status: {} and no files found in {}/bin or {}/lib",
                  output_install.status, install_dir.display(), install_dir.display());
             eprintln!("Make install stdout:\n{}", String::from_utf8_lossy(&output_install.stdout));
             eprintln!("Make install stderr:\n{}", String::from_utf8_lossy(&output_install.stderr));
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
// Ensures we change back even if there's a panic or early return.
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
            // Use log::error for consistency
            log::error!(
                "Failed to restore original working directory to {}: {}",
                self.original_cwd.display(),
                e
            );
        } else {
             debug!("Restored working directory to: {}", self.original_cwd.display());
        }
    }
}