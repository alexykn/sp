// src/build/env.rs
// Manages the build environment setup for formulae.

use crate::Result;
use crate::model::formula::Formula;
use crate::build; // To access get_cellar_path, etc.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::env;
use std::ffi::OsString; // Added for path joining

/// Represents a resolved dependency with its installation prefix.
/// Placeholder - might need adjustment based on dependency resolution logic.
pub struct ResolvedDependency {
    pub formula: Formula,
    pub prefix: PathBuf,
}

/// Generates the necessary environment variables for building a formula.
///
/// This aims to replicate the environment set up by Homebrew's `superenv`.
pub fn setup_build_environment(
    formula: &Formula,
    resolved_dependencies: &[ResolvedDependency],
) -> Result<HashMap<String, String>> {
    let mut env_map = HashMap::new();
    let homebrew_prefix = get_homebrew_prefix();
    let formula_prefix = build::get_formula_cellar_path(formula);

    // --- Basic Homebrew Paths ---
    env_map.insert("HOMEBREW_ENV".to_string(), "super".to_string()); // Indicate superenv
    env_map.insert("HOMEBREW_BREW_FILE".to_string(), env::current_exe()?.to_string_lossy().to_string());
    env_map.insert("HOMEBREW_PREFIX".to_string(), homebrew_prefix.to_string_lossy().to_string());
    env_map.insert("HOMEBREW_CELLAR".to_string(), build::get_cellar_path().to_string_lossy().to_string());
    env_map.insert("HOMEBREW_OPT".to_string(), homebrew_prefix.join("opt").to_string_lossy().to_string());
    env_map.insert("HOMEBREW_TEMP".to_string(), env::temp_dir().to_string_lossy().to_string()); // Use std::env::temp_dir
    env_map.insert("HOMEBREW_FORMULA_PREFIX".to_string(), formula_prefix.to_string_lossy().to_string());
    env_map.insert("HOMEBREW_FORMULA_NAME".to_string(), formula.name.clone());
    env_map.insert("HOMEBREW_FORMULA_VERSION".to_string(), formula.versions.stable.clone().unwrap_or_default());

    // --- macOS SDK Setup ---
    let mut sdk_path_str = "".to_string();
    if cfg!(target_os = "macos") {
        if let Some(sdk_path) = get_macos_sdk_path() {
            sdk_path_str = sdk_path.to_string_lossy().to_string();
            env_map.insert("SDKROOT".to_string(), sdk_path_str.clone());
            env_map.insert("HOMEBREW_SDKROOT".to_string(), sdk_path_str.clone());
        } else {
            eprintln!("Warning: Could not determine macOS SDK path. Compilation might fail.");
        }
    }

    // --- Architecture & Optimization Flags ---
    let arch = detect_architecture();
    let arch_flag = format!("-arch {}", arch);
    let optimization_level = "Os"; // Default
    let opt_flags = format!("-{optimization_level} {}", arch_flag);

    env_map.insert("HOMEBREW_OPTIMIZATION_LEVEL".to_string(), optimization_level.to_string());
    env_map.insert("HOMEBREW_ARCHFLAGS".to_string(), arch_flag.clone());
    env_map.insert("HOMEBREW_OPTFLAGS".to_string(), opt_flags.clone());

    // --- Compiler Detection & Setup ---
    let (cc, cxx) = detect_compilers(&homebrew_prefix);
    let (shim_cc, shim_cxx) = find_compiler_shims(&homebrew_prefix);
    let final_cc = shim_cc.unwrap_or(cc.clone());
    let final_cxx = shim_cxx.unwrap_or(cxx.clone());

    // Set standard compiler variables (might still be needed by some tools)
    env_map.insert("CC".to_string(), final_cc.to_string_lossy().to_string());
    env_map.insert("CXX".to_string(), final_cxx.to_string_lossy().to_string());

    // Set Homebrew specific compiler info
    env_map.insert("HOMEBREW_CC".to_string(), final_cc.to_string_lossy().to_string());
    env_map.insert("HOMEBREW_CXX".to_string(), final_cxx.to_string_lossy().to_string());
    env_map.insert("HOMEBREW_CC_BASENAME".to_string(), cc.file_name().unwrap_or_default().to_string_lossy().to_string());
    env_map.insert("HOMEBREW_CXX_BASENAME".to_string(), cxx.file_name().unwrap_or_default().to_string_lossy().to_string());

    // --- Determine HOMEBREW_CCCFG (Controls shim behavior) ---
    // Start with "O" for argument refurbishing (standard superenv behavior)
    // Add other flags as needed based on formula requirements or future logic (e.g., "x" for C++11)
    // Add logic for C++11 and stdlib flags
    let mut cccfg = "O".to_string(); // Basic refurbishing

    // Check if the formula requires C++11
    if formula.requires_cpp11.unwrap_or(false) {
        cccfg.push('x');
    }

    // Check if the formula specifies a standard library
    if let Some(stdlib) = formula.stdlib.as_deref() {
        match stdlib {
            "libstdc++" => cccfg.push('g'),
            "libc++" => cccfg.push('h'),
            _ => (),
        }
    }

    env_map.insert("HOMEBREW_CCCFG".to_string(), cccfg.clone());

    // --- Calculate Homebrew Path Variables (for shims) ---
    let (homebrew_include_paths, homebrew_isystem_paths) = calculate_homebrew_include_paths(resolved_dependencies, &homebrew_prefix, &sdk_path_str);
    let homebrew_library_paths = calculate_homebrew_library_paths(resolved_dependencies, &homebrew_prefix, &sdk_path_str);

    env_map.insert("HOMEBREW_INCLUDE_PATHS".to_string(), join_paths(&homebrew_include_paths));
    env_map.insert("HOMEBREW_ISYSTEM_PATHS".to_string(), join_paths(&homebrew_isystem_paths));
    env_map.insert("HOMEBREW_LIBRARY_PATHS".to_string(), join_paths(&homebrew_library_paths));

    // --- Build System Paths (PKG_CONFIG, ACLOCAL, CMAKE) ---
    // (These seem mostly correct, maybe minor adjustments later if needed)
    let (pkg_config_path, pkg_config_libdir) = calculate_pkg_config_paths(resolved_dependencies, &homebrew_prefix);
    let aclocal_path = calculate_aclocal_paths(resolved_dependencies, &homebrew_prefix);
    let cmake_prefix_path = calculate_cmake_paths(resolved_dependencies, &homebrew_prefix);

    env_map.insert("PKG_CONFIG_PATH".to_string(), join_paths(&pkg_config_path));
    env_map.insert("PKG_CONFIG_LIBDIR".to_string(), join_paths(&pkg_config_libdir));
    env_map.insert("ACLOCAL_PATH".to_string(), join_paths(&aclocal_path));
    env_map.insert("CMAKE_PREFIX_PATH".to_string(), join_paths(&cmake_prefix_path));

    // --- Standard Compiler Flags (Set for tools that don't respect HOMEBREW_ vars) ---
    // We derive these from the HOMEBREW_ paths for consistency
    let cppflags = homebrew_isystem_paths.iter()
        .map(|p| format!("-isystem {}", p.display()))
        .chain(homebrew_include_paths.iter().map(|p| format!("-I{}", p.display())))
        .collect::<Vec<_>>().join(" ");

    let ldflags = homebrew_library_paths.iter()
        .map(|p| format!("-L{}", p.display()))
        .collect::<Vec<_>>().join(" ");

    // Combine base opt/arch flags with include flags
    let cflags = format!("{} {}", opt_flags, cppflags).trim().to_string();
    let mut cxxflags = cflags.clone(); // Start CXXFLAGS with CFLAGS
    // Add C++ specific flags based on CCCFG
    if cccfg.contains('h') {
        cxxflags.push_str(" -stdlib=libc++");
    } else if cccfg.contains('g') {
        cxxflags.push_str(" -stdlib=libstdc++");
    }

    env_map.insert("CPPFLAGS".to_string(), cppflags.trim().to_string());
    env_map.insert("CFLAGS".to_string(), cflags);
    env_map.insert("CXXFLAGS".to_string(), cxxflags);
    env_map.insert("LDFLAGS".to_string(), ldflags.trim().to_string());

    // --- Standard Paths (PATH) ---
    let build_path = calculate_build_path(resolved_dependencies, &homebrew_prefix);
    let final_path_str = join_paths(&build_path);
    println!("[DEBUG] Calculated PATH: {}", final_path_str);
    env_map.insert("PATH".to_string(), final_path_str);

    // --- Make Flags ---
    let make_jobs = num_cpus::get().to_string();
    env_map.insert("MAKEFLAGS".to_string(), format!("-j{}", make_jobs));
    env_map.insert("HOMEBREW_MAKE_JOBS".to_string(), make_jobs);

    // --- Other Flags ---
    env_map.insert("OPENSSL_NO_VENDOR".to_string(), "1".to_string());
    env_map.insert("GOTOOLCHAIN".to_string(), "local".to_string());
    env_map.insert("HIDAPI_SYSTEM_HIDAPI".to_string(), "1".to_string());
    env_map.insert("PYZMQ_NO_BUNDLE".to_string(), "1".to_string());
    env_map.insert("SODIUM_INSTALL".to_string(), "system".to_string());
    env_map.insert("TZ".to_string(), "UTC0".to_string());

    // Set HOMEBREW_DEPENDENCIES
    let dep_names = resolved_dependencies.iter().map(|rd| rd.formula.name.clone()).collect::<Vec<_>>().join(",");
    env_map.insert("HOMEBREW_DEPENDENCIES".to_string(), dep_names);

    Ok(env_map)
}

/// Gets the Homebrew prefix (e.g., /opt/homebrew or /usr/local)
fn get_homebrew_prefix() -> PathBuf {
    // A simplified check, might need refinement
    if Path::new("/opt/homebrew").exists() {
        PathBuf::from("/opt/homebrew")
    } else {
        PathBuf::from("/usr/local")
    }
}

// --- Helper Functions for Path Calculation ---

/// Joins a slice of PathBufs into a single OSString separated by colons.
fn join_paths(paths: &[PathBuf]) -> String {
    let mut os_string = OsString::new();
    for (i, path) in paths.iter().enumerate() {
        if i > 0 {
            os_string.push(":");
        }
        os_string.push(path);
    }
    os_string.to_string_lossy().into_owned()
}

/// Calculates PKG_CONFIG_PATH and PKG_CONFIG_LIBDIR based on dependencies.
fn calculate_pkg_config_paths(
    dependencies: &[ResolvedDependency],
    homebrew_prefix: &Path,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut pkg_config_path = Vec::new();
    let mut pkg_config_libdir = Vec::new();

    // Add Homebrew's paths first
    let homebrew_pkgconfig = homebrew_prefix.join("lib/pkgconfig");
    let homebrew_share_pkgconfig = homebrew_prefix.join("share/pkgconfig");
    if homebrew_pkgconfig.is_dir() {
        pkg_config_path.push(homebrew_pkgconfig.clone());
        pkg_config_libdir.push(homebrew_pkgconfig); // Add to libdir as well
    }
     if homebrew_share_pkgconfig.is_dir() {
        pkg_config_path.push(homebrew_share_pkgconfig);
    }

    for dep in dependencies {
        let prefix = &dep.prefix; // Use the resolved prefix
        let dep_pkgconfig = prefix.join("lib/pkgconfig");
        let dep_share_pkgconfig = prefix.join("share/pkgconfig");

        if dep_pkgconfig.is_dir() {
            pkg_config_path.push(dep_pkgconfig.clone());
            pkg_config_libdir.push(dep_pkgconfig);
        }
        if dep_share_pkgconfig.is_dir() {
            pkg_config_path.push(dep_share_pkgconfig);
        }
    }

    (pkg_config_path, pkg_config_libdir)
}

/// Calculates ACLOCAL_PATH based on dependencies.
fn calculate_aclocal_paths(
    dependencies: &[ResolvedDependency],
    homebrew_prefix: &Path,
) -> Vec<PathBuf> {
    let mut aclocal_paths = Vec::new();

    // Add Homebrew's path first
    let homebrew_aclocal = homebrew_prefix.join("share/aclocal");
    if homebrew_aclocal.is_dir() {
        aclocal_paths.push(homebrew_aclocal);
    }

    for dep in dependencies {
        let prefix = &dep.prefix;
        let dep_aclocal = prefix.join("share/aclocal");
        if dep_aclocal.is_dir() {
            aclocal_paths.push(dep_aclocal);
        }
    }

     // Add system path as fallback? Homebrew seems to add `/usr/share/aclocal`
     let system_aclocal = PathBuf::from("/usr/share/aclocal");
     if system_aclocal.is_dir() {
         aclocal_paths.push(system_aclocal);
     }

    aclocal_paths
}

/// Calculates CMAKE_PREFIX_PATH based on dependencies.
fn calculate_cmake_paths(
    dependencies: &[ResolvedDependency],
    homebrew_prefix: &Path,
) -> Vec<PathBuf> {
    let mut cmake_paths = Vec::new();

    // Add Homebrew prefix itself
    cmake_paths.push(homebrew_prefix.to_path_buf());

    for dep in dependencies {
        cmake_paths.push(dep.prefix.clone()); // Add the prefix of each dependency
    }

    cmake_paths
}

/// Calculates the PATH environment variable order for the build process.
fn calculate_build_path(
    dependencies: &[ResolvedDependency],
    homebrew_prefix: &Path,
) -> Vec<PathBuf> {
    let mut path_entries = Vec::new();

    // 1. Add Homebrew shim directory
    let shim_path = homebrew_prefix.join("Library/Homebrew/shims/mac/super");
    if shim_path.is_dir() {
        path_entries.push(shim_path);
    }

    // 2. Essential Build Tool Paths from Homebrew opt directory
    let opt_path = homebrew_prefix.join("opt");
    let build_tools = ["pkg-config", "autoconf", "automake", "libtool", "m4", "gm4"];
    for tool in build_tools.iter() {
        let tool_opt_bin_path = opt_path.join(tool).join("bin");
        if tool_opt_bin_path.is_dir() {
            path_entries.push(tool_opt_bin_path);
        }
    }

    // 3. Dependency bin directories
    for dep in dependencies {
        let dep_bin = dep.prefix.join("bin");
        if dep_bin.is_dir() {
            path_entries.push(dep_bin);
        }
        let dep_sbin = dep.prefix.join("sbin");
        if dep_sbin.is_dir() {
            path_entries.push(dep_sbin);
        }
    }

    // 4. Homebrew bin/sbin
    let homebrew_bin = homebrew_prefix.join("bin");
    if homebrew_bin.is_dir() {
        path_entries.push(homebrew_bin);
    }
    let homebrew_sbin = homebrew_prefix.join("sbin");
    if homebrew_sbin.is_dir() {
        path_entries.push(homebrew_sbin);
    }

    // 5. Standard system paths (CRITICAL for finding system compilers)
    path_entries.push(PathBuf::from("/usr/bin"));
    path_entries.push(PathBuf::from("/bin"));
    path_entries.push(PathBuf::from("/usr/sbin"));
    path_entries.push(PathBuf::from("/sbin"));

    path_entries
}

// --- Compiler Detection Helpers ---

/// Detects the base C and C++ compilers.
/// Placeholder implementation - needs refinement.
fn detect_compilers(homebrew_prefix: &Path) -> (PathBuf, PathBuf) {
    // Prioritize Homebrew LLVM if installed, otherwise fallback to system
    let llvm_bin = homebrew_prefix.join("opt/llvm/bin");
    let clang = llvm_bin.join("clang");
    let clang_pp = llvm_bin.join("clang++");

    let default_cc = PathBuf::from("/usr/bin/clang");
    let default_cxx = PathBuf::from("/usr/bin/clang++");

    let cc = if clang.is_file() { clang } else { default_cc };
    let cxx = if clang_pp.is_file() { clang_pp } else { default_cxx };

    (cc, cxx)
}

/// Finds the Homebrew compiler shim scripts.
fn find_compiler_shims(homebrew_prefix: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let shim_dir = homebrew_prefix.join("Library/Homebrew/shims/mac/super");
    let shim_cc = shim_dir.join("cc");
    let shim_cxx = shim_dir.join("cxx");
    let shim_cpp = shim_dir.join("c++");

    let cc = if shim_cc.is_file() { Some(shim_cc) } else { None };
    let _cxx = if shim_cxx.is_file() { Some(shim_cxx) } else { None };
    let cpp = if shim_cpp.is_file() { Some(shim_cpp) } else { None };

    if cc.is_none() || cpp.is_none() {
        eprintln!("Warning: Homebrew compiler shims 'cc' or 'c++' not found in {}", shim_dir.display());
    }

    (cc, cpp)
}

// --- Architecture Detection Helper ---

/// Detects the current CPU architecture.
/// Simple implementation, might need refinement for universality.
fn detect_architecture() -> String {
    #[cfg(target_arch = "x86_64")]
    {
        "x86_64".to_string()
    }
    #[cfg(target_arch = "aarch64")]
    {
        // Homebrew uses "arm64" for Apple Silicon
        "arm64".to_string()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        // Fallback or error for unsupported architectures
        eprintln!("Warning: Unsupported architecture detected.");
        env::consts::ARCH.to_string()
    }
}


/// Gets the path to the active macOS SDK.
#[cfg(target_os = "macos")]
fn get_macos_sdk_path() -> Option<PathBuf> {
    // Try using `xcrun` first, as it respects the selected Xcode version
    if let Ok(output) = std::process::Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
    {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path_str.is_empty() {
                let path = PathBuf::from(path_str);
                if path.exists() {
                    println!("Found SDK path via xcrun: {}", path.display());
                    return Some(path);
                }
            }
        }
    }

    // Fallback: Check common locations if xcrun fails
    let applications_xcode = PathBuf::from("/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk");
    if applications_xcode.exists() {
        println!("Found SDK path via default Xcode location: {}", applications_xcode.display());
        return Some(applications_xcode);
    }

    let clt_sdk = PathBuf::from("/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk");
    if clt_sdk.exists() {
        println!("Found SDK path via Command Line Tools location: {}", clt_sdk.display());
        return Some(clt_sdk);
    }

    // Add more fallbacks if needed, e.g., checking different SDK versions

    None // SDK path couldn't be determined
}

// --- Recalculated Path Helpers (to match Homebrew logic more closely) ---

/// Calculates HOMEBREW_INCLUDE_PATHS and HOMEBREW_ISYSTEM_PATHS.
fn calculate_homebrew_include_paths(
    dependencies: &[ResolvedDependency],
    homebrew_prefix: &Path,
    sdk_path: &str,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut include_paths = Vec::new(); // For -I
    let mut isystem_paths = Vec::new(); // For -isystem

    // 1. Homebrew Prefix Include (as -isystem)
    let homebrew_include = homebrew_prefix.join("include");
    if homebrew_include.is_dir() {
        isystem_paths.push(homebrew_include);
    }

    // 2. SDK Include Paths (as -isystem, if SDKROOT is set)
    if !sdk_path.is_empty() {
        let sdk_include = PathBuf::from(sdk_path).join("usr/include");
        if sdk_include.is_dir() {
            isystem_paths.push(sdk_include);
            // Add specific framework/lib includes if needed (like Homebrew does)
            let sdk_opengl = PathBuf::from(sdk_path).join("System/Library/Frameworks/OpenGL.framework/Versions/Current/Headers");
            if sdk_opengl.is_dir() { isystem_paths.push(sdk_opengl); }
            let sdk_libxml2 = PathBuf::from(sdk_path).join("usr/include/libxml2");
            if sdk_libxml2.is_dir() { isystem_paths.push(sdk_libxml2); }
            // Add more specific SDK paths as identified from Homebrew logic
        }
    }

    // 3. Keg-Only Dependency Includes (as -I)
    for dep in dependencies {
        if is_keg_only(&dep.formula) {
            let dep_include = dep.prefix.join("include");
            if dep_include.is_dir() {
                include_paths.push(dep_include);
            }
        }
    }

    // 4. System include path (fallback, as -isystem)
    // Generally avoided by Homebrew superenv, but might be needed if not using shims fully
    // isystem_paths.push(PathBuf::from("/usr/include"));

    (include_paths, isystem_paths)
}

/// Calculates HOMEBREW_LIBRARY_PATHS.
fn calculate_homebrew_library_paths(
    dependencies: &[ResolvedDependency],
    homebrew_prefix: &Path,
    sdk_path: &str,
) -> Vec<PathBuf> {
    let mut lib_paths = Vec::new();

    // 1. Keg-Only Dependency Libs
    // TODO: Need a way to identify keg-only dependencies
    // For now, add all dependency libs here
    for dep in dependencies {
        // Skip LLVM paths to avoid linking issues, like Homebrew does
        if dep.formula.name.starts_with("llvm") { continue; }

        let dep_lib = dep.prefix.join("lib");
        if dep_lib.is_dir() {
            lib_paths.push(dep_lib);
        }
    }

    // 2. Homebrew Prefix Lib
    let homebrew_lib = homebrew_prefix.join("lib");
    if homebrew_lib.is_dir() {
        lib_paths.push(homebrew_lib);
    }

    // 3. SDK Library Paths (if SDKROOT is set)
    if !sdk_path.is_empty() {
        let sdk_lib = PathBuf::from(sdk_path).join("usr/lib");
        if sdk_lib.is_dir() {
            lib_paths.push(sdk_lib);
            // Add specific framework lib paths if needed
            let sdk_opengl_lib = PathBuf::from(sdk_path).join("System/Library/Frameworks/OpenGL.framework/Versions/Current/Libraries");
            if sdk_opengl_lib.is_dir() { lib_paths.push(sdk_opengl_lib); }
            // Add more specific SDK paths as identified from Homebrew logic
        }
    }

    // 4. System library path (fallback)
    // Generally avoided by Homebrew superenv
    // lib_paths.push(PathBuf::from("/usr/lib"));

    lib_paths
}

/// Determines if a dependency is keg-only based on its formula metadata.
fn is_keg_only(formula: &Formula) -> bool {
    formula.keg_only // Access the bool field directly
}


