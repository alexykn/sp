// src/build/source.rs
// Contains logic for downloading and building from source

use crate::utils::error::{BrewRsError, Result};
use crate::model::formula::Formula;
use crate::utils::config::Config;
use crate::build::env::{self, ResolvedDependency};
use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::copy;
use reqwest::Client;
use std::io::Cursor;
use std::process::Command;
use std::collections::HashMap;

/// Download source code for the formula
pub async fn download_source(formula: &Formula, config: &Config) -> Result<PathBuf> {
    // Try to get a stable URL from the formula
    let url = if let Some(stable_url) = get_formula_url(formula) {
        stable_url
    } else {
        // Fallback to creating a URL from the formula name and homepage
        if let Some(homepage) = &formula.homepage {
            if let Some(version) = &formula.versions.stable {
                if homepage.contains("github.com") {
                    // Create a typical GitHub release URL as fallback
                    format!("{}/archive/v{}.tar.gz", homepage.trim_end_matches('/'), version)
                } else {
                    return Err(BrewRsError::Generic("No source URL available".to_string()));
                }
            } else {
                return Err(BrewRsError::Generic("No source URL available".to_string()));
            }
        } else {
            return Err(BrewRsError::Generic("No source URL available".to_string()));
        }
    };

    println!("==> Downloading source for {}", formula.name);

    download_url(&url, config).await
}

/// Get the main URL for a formula, trying stable URL first
fn get_formula_url(formula: &Formula) -> Option<String> {
    // Try to get the stable URL first
    for (url_type, url_info) in &formula.urls.urls {
        if url_type == "stable" {
            return url_info.url.clone();
        }
    }

    // If no stable URL, take any URL
    for (_, url_info) in &formula.urls.urls {
        if let Some(url) = &url_info.url {
            return Some(url.clone());
        }
    }

    None
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
        .map_err(|e| BrewRsError::Http(e))?;

    if !response.status().is_success() {
        return Err(BrewRsError::Generic(format!("Failed to download: HTTP status {}", response.status())));
    }

    let content = response.bytes()
        .await
        .map_err(|e| BrewRsError::Http(e))?;

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
    formula: &Formula,
    resolved_dependencies: &[ResolvedDependency],
) -> Result<PathBuf> {
    // Get the installation directory
    let install_dir = super::get_formula_cellar_path(formula);

    // Create a temporary directory for building
    let temp_dir = PathBuf::from("/tmp").join(format!("brew-rs-build-{}", formula.name));

    // Remove the directory if it already exists
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)?;
    }

    // Create directory
    fs::create_dir_all(&temp_dir)?;

    println!("==> Extracting source to {}", temp_dir.display());

    // Extract the source
    super::extract_archive(source_path, &temp_dir)?;

    // Get the build directory - assume it's either the formula name or the only directory in temp_dir
    let build_dir = find_build_directory(&temp_dir, &formula.name)?;

    println!("==> Building {} from source in {}", formula.name, build_dir.display());

    // --- Setup Build Environment ---
    println!("==> Setting up build environment");
    let env_map = env::setup_build_environment(formula, resolved_dependencies)?;
    // Optional: Print the environment for debugging
    // for (key, value) in &env_map {
    //     println!("  Env: {}={}", key, value);
    // }

    // Detect and build using the appropriate build system
    detect_and_build(&build_dir, &install_dir, &env_map)?;

    // Write the receipt
    super::write_receipt(formula, &install_dir)?;

    Ok(install_dir)
}

/// Find the directory containing the source code after extraction
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

    Err(BrewRsError::Generic(format!(
        "Could not find build directory for {} in {}",
        formula_name, temp_dir.display()
    )))
}

/// Detect the build system and use the appropriate build method
fn detect_and_build(
    build_dir: &Path,
    install_dir: &Path,
    env_map: &HashMap<String, String>,
) -> Result<()> {
    // First, ensure build directory exists and is accessible
    if !build_dir.exists() {
        return Err(BrewRsError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Build directory does not exist: {}", build_dir.display()),
        )));
    }

    // Check if we can access it
    std::fs::read_dir(build_dir).map_err(|e| {
        BrewRsError::Io(std::io::Error::new(
            e.kind(),
            format!("Cannot access build directory {}: {}", build_dir.display(), e),
        ))
    })?;

    // Change to the build directory
    std::env::set_current_dir(build_dir)?;
    println!("Changed working directory to: {}", build_dir.display());

    // 1. Check for autogen.sh (usually needs to run before configure)
    let autogen_script = build_dir.join("autogen.sh");
    if autogen_script.exists() {
        println!("==> Running ./autogen.sh");

        // Ensure the script is executable
        Command::new("chmod")
            .arg("+x")
            .arg(autogen_script.to_str().unwrap())
            .status()?;

        let mut cmd = Command::new("./autogen.sh");
        cmd.current_dir(build_dir); // Explicitly set working directory
        for (key, value) in env_map {
            cmd.env(key, value);
        }
        let output = cmd.output()?;

        if !output.status.success() {
            eprintln!("stdout:\n{}", String::from_utf8_lossy(&output.stdout));
            eprintln!("stderr:\n{}", String::from_utf8_lossy(&output.stderr));
            return Err(BrewRsError::Generic(format!(
                "autogen.sh failed with status: {}", output.status
            )));
        }
    }
    // 2. Check for configure.ac or configure.in (needs autoconf)
    else if build_dir.join("configure.ac").exists() || build_dir.join("configure.in").exists() {
        println!("==> Running autoconf");

        // Explicitly set M4 environment variable to ensure autoconf uses the right version
        let mut modified_env_map = env_map.clone();

        // --- M4 Detection Logic (inspired by Homebrew) ---
        let m4_var_name = "M4";
        let system_m4_path = PathBuf::from("/usr/bin/m4"); // Standard system path
        let mut m4_cmd_path: Option<PathBuf> = None;

        // Check if M4 is already set in the environment
        if let Some(m4_path_str) = modified_env_map.get(m4_var_name) {
             let path = PathBuf::from(m4_path_str);
             if path.exists() {
                 println!("Using M4 from environment: {}", path.display());
                 m4_cmd_path = Some(path);
             }
        }

        // If not set or invalid, try to find gm4 (GNU m4)
        if m4_cmd_path.is_none() {
            if let Ok(output) = Command::new("which").arg("gm4").output() {
                if output.status.success() {
                    let gm4_path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !gm4_path_str.is_empty() {
                        let gm4_path = PathBuf::from(gm4_path_str);
                        if gm4_path.exists() {
                            println!("Found gm4 (GNU m4) via which: {}", gm4_path.display());
                            m4_cmd_path = Some(gm4_path);
                        }
                    }
                }
            }
        }

        // If gm4 not found via which, check Homebrew's typical opt path
        if m4_cmd_path.is_none() {
             let homebrew_prefix = super::super::get_homebrew_prefix();
             let gm4_opt_path = homebrew_prefix.join("opt/m4/bin/gm4");
             if gm4_opt_path.exists() {
                println!("Found gm4 (GNU m4) via Homebrew opt: {}", gm4_opt_path.display());
                m4_cmd_path = Some(gm4_opt_path);
             }
        }

        // If still no GNU m4, check if the system m4 exists BUT might not be GNU
        if m4_cmd_path.is_none() && system_m4_path.exists() {
             println!("Warning: Found system m4 at {}, but it might not be GNU m4. Autoconf might fail.", system_m4_path.display());
             m4_cmd_path = Some(system_m4_path); // Use system m4 as a last resort
        }

        // Ensure we found *some* m4
        let final_m4_path = match m4_cmd_path {
            Some(path) => path,
            None => {
                 return Err(BrewRsError::Generic(
                    "GNU m4 (or system m4) is required for autoconf but could not be found. Please install m4 (e.g., `brew install m4`).".to_string()
                 ));
            }
        };

        println!("Setting M4 environment variable to: {}", final_m4_path.display());
        modified_env_map.insert(m4_var_name.to_string(), final_m4_path.to_string_lossy().to_string());
        // --- End M4 Detection ---

        // Check if grep is in the PATH (needed by autoconf)
        if Command::new("which").arg("grep").output().map(|output| !output.status.success()).unwrap_or(true) {
            // Add some standard system paths if grep command not found
            if let Some(path) = modified_env_map.get_mut("PATH") {
                *path = format!("{}:/usr/bin:/bin", path);
                println!("Added system paths to PATH for grep: {}", path);
            }
        }

        // Determine if we need to use a specific path for autoconf
        let autoconf_cmd = if let Some(autoconf_path) = modified_env_map.get("AUTOCONF") {
            PathBuf::from(autoconf_path)
        } else if let Ok(output) = Command::new("which").arg("autoconf").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    PathBuf::from(path)
                } else {
                    // Try standard locations
                    let homebrew_prefix = super::super::get_homebrew_prefix();
                    let candidate = homebrew_prefix.join("bin/autoconf");
                    if candidate.exists() {
                        candidate
                    } else {
                        PathBuf::from("autoconf") // fallback to PATH lookup
                    }
                }
            } else {
                // Try standard locations
                let homebrew_prefix = super::super::get_homebrew_prefix();
                let candidate = homebrew_prefix.join("bin/autoconf");
                if candidate.exists() {
                    candidate
                } else {
                    PathBuf::from("autoconf") // fallback to PATH lookup
                }
            }
        } else {
            // Try standard locations
            let homebrew_prefix = super::super::get_homebrew_prefix();
            let candidate = homebrew_prefix.join("bin/autoconf");
            if candidate.exists() {
                candidate
            } else {
                PathBuf::from("autoconf") // fallback to PATH lookup
            }
        };

        println!("Using autoconf command: {:?}", autoconf_cmd);
        let mut cmd = Command::new(&autoconf_cmd);
        for (key, value) in &modified_env_map {
            cmd.env(key, value);
        }

        // Make sure we're in the right directory
        cmd.current_dir(build_dir);

        println!("Executing autoconf in directory: {}", build_dir.display());
        let output = match cmd.output() {
            Ok(output) => output,
            Err(e) => {
                eprintln!("Failed to execute autoconf: {}", e);
                eprintln!("Autoconf command: {:?}", autoconf_cmd);
                eprintln!("Working directory: {}", build_dir.display());
                return Err(BrewRsError::Io(e));
            }
        };

        if !output.status.success() {
            eprintln!("stdout:\n{}", String::from_utf8_lossy(&output.stdout));
            eprintln!("stderr:\n{}", String::from_utf8_lossy(&output.stderr));
            return Err(BrewRsError::Generic(format!(
                "autoconf failed with status: {}", output.status
            )));
        }
    }

    // 3. Check for configure script
    let configure_script = build_dir.join("configure");
    if configure_script.exists() {
        // Ensure the configure script is executable
        if let Err(e) = Command::new("chmod")
            .arg("+x")
            .arg(configure_script.to_str().unwrap())
            .status()
        {
            eprintln!("Warning: Failed to set executable permission on configure script: {}", e);
        }

        return configure_and_make(build_dir, install_dir, env_map);
    }

    // 4. Check for CMakeLists.txt
    if build_dir.join("CMakeLists.txt").exists() {
        return cmake_build(build_dir, install_dir, env_map);
    }

    // 5. Check for meson.build
    if build_dir.join("meson.build").exists() {
        return meson_build(build_dir, install_dir, env_map);
    }

    // 6. Check for Cargo.toml (Rust project)
    if build_dir.join("Cargo.toml").exists() {
        return cargo_build(build_dir, install_dir, env_map);
    }

    // 7. Check for setup.py (Python project)
    if build_dir.join("setup.py").exists() {
        return python_build(build_dir, install_dir, env_map);
    }

    // 8. Check for Makefile directly (simple make project)
    if build_dir.join("Makefile").exists() || build_dir.join("makefile").exists() {
        return simple_make(build_dir, install_dir, env_map);
    }

    // If we get here, we couldn't determine the build system
    Err(BrewRsError::Generic(format!(
        "Could not determine build system for {}",
        build_dir.display()
    )))
}

/// Configure and build with autotools (./configure && make && make install)
fn configure_and_make(
    build_dir: &Path,
    install_dir: &Path,
    env_map: &HashMap<String, String>,
) -> Result<()> {
    println!("==> Running ./configure --prefix={}", install_dir.display());

    // Ensure configure script is executable
    if !is_executable(&build_dir.join("configure"))? {
        Command::new("chmod")
            .arg("+x")
            .arg("./configure")
            .status()?;
    }

    let mut cmd = Command::new("./configure");
    cmd.arg(format!("--prefix={}", install_dir.display()));
    for (key, value) in env_map {
        cmd.env(key, value);
    }
    let output = cmd.output()?;

    // Log output for debugging
    println!("Configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "Configure failed with status: {}", output.status
        )));
    }

    // Run make
    println!("==> Running make");
    let mut cmd = Command::new("make");
    for (key, value) in env_map {
        cmd.env(key, value);
    }
    let output = cmd.output()?;

    // Log output for debugging
    println!("Make stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Make stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "Make failed with status: {}", output.status
        )));
    }

    // Run make install
    println!("==> Running make install");
    let mut cmd = Command::new("make");
    cmd.arg("install");
    for (key, value) in env_map {
        cmd.env(key, value);
    }
    let output = cmd.output()?;

    // Log output for debugging
    println!("Make install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Make install stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "Make install failed with status: {}", output.status
        )));
    }

    Ok(())
}

/// Build with CMake
fn cmake_build(
    build_dir: &Path,
    install_dir: &Path,
    env_map: &HashMap<String, String>,
) -> Result<()> {
    println!("==> Building with CMake");
    let build_subdir = build_dir.join("brew-rs-build");
    fs::create_dir_all(&build_subdir)?;

    println!("==> Running cmake .. -DCMAKE_INSTALL_PREFIX={}", install_dir.display());
    let mut cmd = Command::new("cmake");
    cmd.arg("..")
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", install_dir.display()))
        .args(&[
            "-DCMAKE_FIND_FRAMEWORK=LAST",
            "-DCMAKE_VERBOSE_MAKEFILE=ON",
            "-Wno-dev",
        ])
        .current_dir(&build_subdir);
    for (key, value) in env_map {
        cmd.env(key, value);
    }
    let output = cmd.output()?;

    println!("CMake configure stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("CMake configure stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "CMake configure failed with status: {}", output.status
        )));
    }

    println!("==> Running make install in {}", build_subdir.display());
    let mut cmd = Command::new("make");
    cmd.arg("install")
        .current_dir(&build_subdir);
    for (key, value) in env_map {
        cmd.env(key, value);
    }
    let output = cmd.output()?;

    println!("CMake make install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("CMake make install stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "CMake make install failed with status: {}", output.status
        )));
    }

    Ok(())
}

/// Build with Meson
fn meson_build(
    build_dir: &Path,
    install_dir: &Path,
    env_map: &HashMap<String, String>,
) -> Result<()> {
    println!("==> Building with Meson");
    let build_subdir = build_dir.join("brew-rs-build");

    println!("==> Running meson setup --prefix={} {}", install_dir.display(), build_subdir.display());
    let mut cmd = Command::new("meson");
    cmd.arg("setup")
        .arg(format!("--prefix={}", install_dir.display()))
        .arg(&build_subdir)
        .arg(".")
        .current_dir(build_dir);
    for (key, value) in env_map {
        cmd.env(key, value);
    }
    let output = cmd.output()?;

    println!("Meson setup stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Meson setup stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "Meson setup failed with status: {}", output.status
        )));
    }

    println!("==> Running meson install -C {}", build_subdir.display());
    let mut cmd = Command::new("meson");
    cmd.arg("install")
        .arg("-C")
        .arg(&build_subdir)
        .current_dir(build_dir);
    for (key, value) in env_map {
        cmd.env(key, value);
    }
    let output = cmd.output()?;

    println!("Meson install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Meson install stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "Meson install failed with status: {}", output.status
        )));
    }

    Ok(())
}

/// Build with Cargo
fn cargo_build(
    build_dir: &Path,
    install_dir: &Path,
    env_map: &HashMap<String, String>,
) -> Result<()> {
    println!("==> Building with Cargo");

    // Cargo install directly installs binaries to $CARGO_HOME/bin by default.
    // To install into the Cellar, we usually need `cargo build --release`
    // and then manually copy artifacts.

    println!("==> Running cargo build --release");
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--release")
        .current_dir(build_dir);
    for (key, value) in env_map {
        cmd.env(key, value);
    }
    let output = cmd.output()?;

    println!("Cargo build stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Cargo build stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "Cargo build failed with status: {}", output.status
        )));
    }

    // Manually install artifacts (binaries, libraries, etc.)
    println!("==> Installing artifacts to {}", install_dir.display());
    // Example: Copy binaries from target/release to install_dir/bin
    let target_release_dir = build_dir.join("target/release");
    let install_bin_dir = install_dir.join("bin");
    fs::create_dir_all(&install_bin_dir)?;

    for entry in fs::read_dir(target_release_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && is_executable(&path)? {
            let filename = path.file_name().unwrap();
            let dest_path = install_bin_dir.join(filename);
            println!("  -> Copying {} to {}", path.display(), dest_path.display());
            fs::copy(&path, &dest_path)?;
        }
        // TODO: Handle libraries (.dylib, .so), static libs (.a), etc.
    }

    Ok(())
}

/// Build with Python setup.py
fn python_build(
    build_dir: &Path,
    install_dir: &Path,
    env_map: &HashMap<String, String>,
) -> Result<()> {
    println!("==> Building with Python setup.py");
    // Find the python executable, preferably one managed by Homebrew if available
    // For simplicity, using `python3` for now.
    let python_exe = "python3";

    println!("==> Running {} setup.py install --prefix={}", python_exe, install_dir.display());
    let mut cmd = Command::new(python_exe);
    cmd.arg("setup.py")
        .arg("install")
        .arg(format!("--prefix={}", install_dir.display()))
        .current_dir(build_dir);
    for (key, value) in env_map {
        cmd.env(key, value);
    }
    let output = cmd.output()?;

    println!("Python install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Python install stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "Python setup.py install failed with status: {}", output.status
        )));
    }

    Ok(())
}

/// Build with a simple Makefile
fn simple_make(
    build_dir: &Path,
    install_dir: &Path,
    env_map: &HashMap<String, String>,
) -> Result<()> {
    println!("==> Building with simple Makefile");

    println!("==> Running make");
    let mut cmd = Command::new("make");
    cmd.current_dir(build_dir);
    for (key, value) in env_map {
        cmd.env(key, value);
    }
    let output = cmd.output()?;

    println!("Make stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Make stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Err(BrewRsError::Generic(format!(
            "Make failed with status: {}", output.status
        )));
    }

    println!("==> Running make install PREFIX={}", install_dir.display());
    let mut cmd = Command::new("make");
    cmd.arg("install")
        .arg(format!("PREFIX={}", install_dir.display()))
        .current_dir(build_dir);
    for (key, value) in env_map {
        cmd.env(key, value);
    }
    let output = cmd.output()?;

    println!("Make install stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    println!("Make install stderr:\n{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        // Some Makefiles might not have an install target, this might be okay
        // but we should probably check if files were actually installed.
        // For now, treat it as an error if the command fails.
        return Err(BrewRsError::Generic(format!(
            "Make install failed with status: {}", output.status
        )));
    }

    Ok(())
}

/// Check if a file is executable
fn is_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(path)?;
    let permissions = metadata.permissions();
    // Check if user, group, or other has execute permission
    Ok(permissions.mode() & 0o111 != 0)
}
