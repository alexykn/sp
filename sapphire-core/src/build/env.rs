use crate::model::formula::FormulaDependencies; // Keep using the trait for name() and install_prefix()
use crate::utils::error::{Result, SapphireError};
use crate::build::devtools; // Import the devtools module
use std::{
    collections::{HashMap, HashSet},
    env,
    path::{Path, PathBuf},
    process::Command,
};

// Environment variables to remove, inspired by Homebrew's list
const ENV_VARS_TO_REMOVE: &[&str] = &[
    // General interfering vars
    "RUBYLIB", "RUBYOPT", "RUBYPATH", "RBENV_VERSION", "CHRUBY_RUBY",
    "GEM_HOME", "GEM_PATH", "GEM_CACHE",
    "PYTHONHOME", "PYTHONPATH", "PYTHONEXECUTABLE", "PIP_REQUIRE_VIRTUALENV",
    "PERL5LIB", "PERL_MB_OPT", "PERL_MM_OPT",
    "NODE_PATH",
    "GOENV_ROOT", "GOPATH", "GOBIN",
    "R_ENVIRON_USER", "R_PROFILE_USER",
    "JAVA_HOME", "_JAVA_OPTIONS", "CLASSPATH", "JAVA_TOOL_OPTIONS",
    "OBJC_INCLUDE_PATH",
    // Build system vars we want to control
    "MAKEFLAGS", "MAKELEVEL", // Control job count via our MAKEFLAGS
    "CMAKE_PREFIX_PATH", "CMAKE_INCLUDE_PATH", "CMAKE_LIBRARY_PATH", "CMAKE_FRAMEWORK_PATH", // We set these
    "PKG_CONFIG_PATH", "PKG_CONFIG_LIBDIR", "PKG_CONFIG_SYSROOT_DIR", // We set these
    "CPATH", "INCLUDE", "INCLUDE_PATH", // Let our CPPFLAGS manage includes
    "LIBRARY_PATH", "LIBPATH", // Let our LDFLAGS manage libraries
    "SDKROOT", // We set this explicitly on macOS
    "LDFLAGS", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "OBJCFLAGS", "OBJCXXFLAGS", // We set these explicitly
    "CC", "CXX", "CPP", "LD", // We set these explicitly via HOMEBREW_* vars usually
    // Potentially problematic user settings
    "DEBUG", // Can interfere with configure/make flags (was 'println' which is likely a typo)
    "GREP_OPTIONS", // Can interfere with configure scripts
    // Homebrew internals (might not apply directly but good to filter)
    "HOMEBREW_DEBUG", "HOMEBREW_VERBOSE", "HOMEBREW_DEVELOPER", // Corrected DEBUG var
    "HOMEBREW_OPTIMIZATION_LEVEL", "HOMEBREW_ARCH", "HOMEBREW_ARTIFACT_DOMAIN",
    "HOMEBREW_AUTO_UPDATE_SECS", "HOMEBREW_BAT", "HOMEBREW_BUILD_BOTTLE",
    "HOMEBREW_BUILD_FROM_SOURCE", "HOMEBREW_CACHE", "HOMEBREW_CASK_OPTS",
    "HOMEBREW_CLEANUP_MAX_AGE_DAYS", "HOMEBREW_CORE_GIT_REMOTE",
    "HOMEBREW_CURL_RETRIES", "HOMEBREW_CURL_VERBOSE", "HOMEBREW_DEVELOPER_DIR",
    "HOMEBREW_DISABLE_LOAD_FORMULA", "HOMEBREW_DISPLAY", "HOMEBREW_DISPLAY_INSTALL_TIMES",
    "HOMEBREW_ENV_PASSTHROUGH", "HOMEBREW_FORCE_BREWED_CA_CERTIFICATES",
    "HOMEBREW_GIT_EMAIL", "HOMEBREW_GIT_NAME", "HOMEBREW_GITHUB_API_TOKEN",
    "HOMEBREW_INSTALL_BADGE", "HOMEBREW_LOGS", "HOMEBREW_MAKE_JOBS", // We set this via MAKEFLAGS
    "HOMEBREW_NO_ANALYTICS", "HOMEBREW_NO_AUTO_UPDATE", "HOMEBREW_NO_BOTTLE_SOURCE_FALLBACK",
    "HOMEBREW_NO_COLOR", "HOMEBREW_NO_COMPAT", "HOMEBREW_NO_EMOJI", "HOMEBREW_NO_ENV_HINTS",
    "HOMEBREW_NO_FONT_AWESOME", "HOMEBREW_NO_GIT_REPO", "HOMEBREW_NO_INSECURE_REDIRECT",
    "HOMEBREW_NO_INSTALL_CLEANUP", "HOMEBREW_NO_INSTALL_FROM_API", "HOMEBREW_PREFIX",
    "HOMEBREW_PRY", "HOMEBREW_SKIP_OR_LATER_BLOCK", "HOMEBREW_TEMP",
    "HOMEBREW_UPDATE_TO_TAG", "HOMEBREW_USE_RUBY_FROM_PATH", "HOMEBREW_VERBOSE_USING_DOTS",
    "HOMEBREW_API_DOMAIN", "HOMEBREW_BOTTLE_DOMAIN", "HOMEBREW_BREW_GIT_REMOTE",
    "HOMEBREW_FORCE_HOMEBREW_ON_LINUX", "HOMEBREW_SORBET_RUNTIME",
    "HOMEBREW_SYSTEM_ENV_PASSTHROUGH", // Any other HOMEBREW_* we don't explicitly set/keep
];

// Environment variables to *keep* or pass through from the user's environment
// Add more if specific build systems require them (e.g., X11 related).
const ENV_VARS_TO_KEEP: &[&str] = &[
    "USER",
    "LOGNAME",
    "HOME", // We might override this, but keep the original value accessible if needed
    "TMPDIR", // We will likely set our own, but keep original for reference
    "TERM",
    "SHELL",
    "EDITOR",
    "LANG", "LC_ALL", "LC_CTYPE", // Locale settings are often needed
    "DISPLAY", "XAUTHORITY", // For X11 interaction if needed by builds
    "TZ", // Timezone
    // Note: PATH is intentionally *not* kept; we rebuild it entirely.
];

/// Represents the sanitized build environment, mimicking Homebrew's "superenv".
#[derive(Debug, Clone)]
pub struct BuildEnvironment {
    /// The final map of environment variables to be used for build commands.
    vars: HashMap<String, String>,
    /// The ordered list of directories constituting the final PATH.
    #[allow(dead_code)] // Keep for potential debugging or future use
    path_dirs: Vec<PathBuf>,
    /// The root installation directory for Sapphire (e.g., /opt/homebrew or /usr/local).
    #[allow(dead_code)] // Keep for potential debugging or future use
    sapphire_prefix: PathBuf,
    /// The specific installation prefix for the formula being built.
    #[allow(dead_code)] // Keep for potential debugging or future use
    formula_install_prefix: PathBuf,
    /// Resolved path to the C compiler.
    #[allow(dead_code)] // Keep for potential debugging or future use
    cc: PathBuf,
    /// Resolved path to the C++ compiler.
    #[allow(dead_code)] // Keep for potential debugging or future use
    cxx: PathBuf,
    /// Resolved path to the macOS SDK (or "/" if not applicable).
    #[allow(dead_code)] // Keep for potential debugging or future use
    sdk_path: PathBuf,
}

impl BuildEnvironment {
    /// Creates a new sanitized build environment for a given formula.
    ///
    /// This function mimics Homebrew's `superenv` by:
    /// 1. Filtering potentially harmful environment variables.
    /// 2. Constructing a controlled PATH including dependency directories.
    /// 3. Setting essential build variables (compilers, flags, SDK, tool paths).
    ///
    /// # Arguments
    /// * `formula` - A reference to the formula being built, implementing `FormulaDependencies`.
    /// * `sapphire_prefix` - The root directory of the Sapphire installation (e.g., /opt/homebrew).
    /// * `cellar_path` - The root directory of the Cellar (e.g., /opt/homebrew/Cellar).
    /// * `build_dep_opt_paths` - A slice of resolved 'opt' paths for build-time dependencies.
    pub fn new<F: FormulaDependencies>(
        formula: &F,
        sapphire_prefix: &Path,
        cellar_path: &Path,
        build_dep_opt_paths: &[PathBuf], // Added parameter
    ) -> Result<Self> {
        println!("Creating BuildEnvironment for formula '{}'...", formula.name());

        let mut vars = HashMap::new();
        let mut path_dirs = Vec::new();

        // 1. Filter environment variables
        let initial_env: HashMap<String, String> = env::vars().collect();
        let vars_to_remove_set: HashSet<&str> = ENV_VARS_TO_REMOVE.iter().cloned().collect();
        let vars_to_keep_set: HashSet<&str> = ENV_VARS_TO_KEEP.iter().cloned().collect();

        for (key, value) in initial_env.iter() {
            let key_upper = key.to_uppercase(); // Normalize for checks like HOMEBREW_*

            // Remove explicitly blacklisted variables
            if vars_to_remove_set.contains(key.as_str()) {
                println!("Removing env var: {}", key);
                continue;
            }
            // Remove any HOMEBREW_* variables not explicitly controlled by us
            if key_upper.starts_with("HOMEBREW_") && !Self::is_controlled_homebrew_var(key) {
                 println!("Removing potentially interfering Homebrew env var: {}", key);
                 continue;
            }
            // Keep explicitly whitelisted variables
            if vars_to_keep_set.contains(key.as_str()) {
                 println!("Keeping env var: {}", key);
                 vars.insert(key.clone(), value.clone());
            } else {
                 // Variable is not explicitly removed or kept. Default to removing it
                 // for a stricter sandbox, unless it's needed (e.g., `TERM`).
                 // For now, let's only keep the explicit list + what we set.
                 println!("Ignoring unspecified env var: {}", key);
            }
        }
        println!("Initial environment filtering complete.");


        // 2. Determine Build Tools and System Info (using devtools module)
        let cc = devtools::find_compiler("cc")?;
        let cxx = devtools::find_compiler("c++")?;
        let sdk_path = devtools::find_sdk_path()?;
        let macos_version = devtools::get_macos_version()?;
        let arch_flag = devtools::get_arch_flag();
        let formula_install_prefix = formula.install_prefix(cellar_path)?; // Get the target install dir

        println!(
            "Resolved tools: CC={}, CXX={}, SDK={}, macOS={}, ArchFlag='{}', InstallPrefix={}",
            cc.display(), cxx.display(), sdk_path.display(), macos_version, arch_flag, formula_install_prefix.display()
        );

        // 3. Get Runtime Dependency Paths (We get build paths via parameter now)
        // These runtime paths are primarily needed for setting include/lib flags,
        // not necessarily for the PATH itself unless tools from runtime deps are needed at build time.
        // Let's assume `build_dep_opt_paths` contains *all* dependencies needed during the build.
        let runtime_dep_opt_paths = build_dep_opt_paths; // For now, assume build deps cover runtime build needs too.
                                                        // A more precise approach would involve getting runtime deps separately if needed.
        println!("Using build dependency paths for include/lib flags: {:?}", runtime_dep_opt_paths.iter().map(|p| p.display()).collect::<Vec<_>>());


        // 4. Construct PATH (Order matters!)
        //    - Start with known-good system paths (lowest priority)
        path_dirs.push(PathBuf::from("/usr/bin"));
        path_dirs.push(PathBuf::from("/bin"));
        path_dirs.push(PathBuf::from("/usr/sbin"));
        path_dirs.push(PathBuf::from("/sbin"));

        //    - Prepend **build dependency** bin/sbin paths (High priority!)
        for dep_opt_path in build_dep_opt_paths.iter().rev() { // Iterate reverse for correct prepend order
            let dep_bin = dep_opt_path.join("bin");
            if dep_bin.is_dir() { // Check existence before adding
                path_dirs.insert(0, dep_bin.clone());
                println!("Prepending build dependency bin to PATH: {}", dep_bin.display());
            }
            let dep_sbin = dep_opt_path.join("sbin");
            if dep_sbin.is_dir() { // Check existence before adding
                // Insert sbin after corresponding bin if possible, otherwise just prepend
                 let bin_pos = path_dirs.iter().position(|p| p == &dep_bin).unwrap_or(0);
                 path_dirs.insert(bin_pos + 1, dep_sbin.clone());
                 println!("Prepending build dependency sbin to PATH: {}", dep_sbin.display());
            }
        }


        //    - Add Sapphire's bin/sbin directories (Medium priority)
        let sapphire_bin = sapphire_prefix.join("bin");
        if sapphire_bin.is_dir() {
            // Insert after build deps, before system paths
             let first_system_path_idx = path_dirs.iter().position(|p| p.starts_with("/usr/bin") || p.starts_with("/bin")).unwrap_or(path_dirs.len());
            path_dirs.insert(first_system_path_idx, sapphire_bin);
            println!("Prepending Sapphire bin to PATH: {}", sapphire_prefix.join("bin").display());
        }
        let sapphire_sbin = sapphire_prefix.join("sbin");
        if sapphire_sbin.is_dir() {
            // sbin usually comes after bin in precedence
            let bin_pos = path_dirs.iter().position(|p| p == &sapphire_prefix.join("bin")).unwrap_or(path_dirs.len());
            path_dirs.insert(bin_pos + 1, sapphire_sbin);
             println!("Prepending Sapphire sbin to PATH: {}", sapphire_prefix.join("sbin").display());
        }

        //    - Prepend compiler path (Highest priority for build tools like cc itself)
        if let Some(compiler_dir) = cc.parent() {
             let compiler_path_buf = compiler_dir.to_path_buf();
             // Remove if already present from deps perhaps, then insert at front
             path_dirs.retain(|p| p != &compiler_path_buf);
             path_dirs.insert(0, compiler_path_buf.clone());
             println!("Prepending compiler bin to PATH: {}", compiler_path_buf.display());
        }

        //    - Deduplicate while preserving order (simple linear scan)
        let mut unique_path_dirs = Vec::new();
        for dir in path_dirs {
            if !unique_path_dirs.contains(&dir) {
                unique_path_dirs.push(dir);
            }
        }
        path_dirs = unique_path_dirs;


        let final_path_str = env::join_paths(path_dirs.iter())
            .map_err(|e| SapphireError::BuildEnvError(format!("Failed to join PATH: {}", e)))?
            .into_string()
            .map_err(|os_str| SapphireError::BuildEnvError(format!("Final PATH contains non-UTF8 characters: {:?}", os_str)))?;

        println!("Final PATH: {}", final_path_str);


        // 5. Set Essential Variables
        vars.insert("PATH".to_string(), final_path_str.clone());

        // Basic essentials
        vars.insert("HOME".to_string(), env::var("HOME").unwrap_or_else(|_| {
             println!("Could not read HOME env var, using '/'");
             "/".to_string()
        }));
        // Use a Sapphire-managed temporary directory if possible
        let tmpdir_base = env::temp_dir().join("sapphire-builds");
         std::fs::create_dir_all(&tmpdir_base).map_err(|e| SapphireError::BuildEnvError(format!("Failed to create base temp dir {}: {}", tmpdir_base.display(), e)))?;
         let tmpdir = tempfile::Builder::new()
             .prefix(&format!("{}-", formula.name()))
             .tempdir_in(&tmpdir_base)
             .map_err(|e| SapphireError::BuildEnvError(format!("Failed to create temp dir: {}", e)))?
             .into_path(); // Get PathBuf for the temp dir specific to this build


        vars.insert("TMPDIR".to_string(), tmpdir.to_string_lossy().to_string());
        vars.insert("TEMP".to_string(), tmpdir.to_string_lossy().to_string()); // For windowsy tools
        vars.insert("TMP".to_string(), tmpdir.to_string_lossy().to_string()); // For other tools

        // macOS Specific System Settings
        if cfg!(target_os = "macos") {
            if sdk_path != PathBuf::from("/") { // Only set if we found a real SDK
                vars.insert("SDKROOT".to_string(), sdk_path.to_string_lossy().to_string());
                println!("Setting SDKROOT={}", sdk_path.display());
            } else {
                println!("No valid SDKROOT found, build might fail.");
            }
            vars.insert("MACOSX_DEPLOYMENT_TARGET".to_string(), macos_version.clone());
             println!("Setting MACOSX_DEPLOYMENT_TARGET={}", macos_version);
        }

        // Set Compilers (both standard and HOMEBREW_ prefixed)
        vars.insert("CC".to_string(), cc.to_string_lossy().to_string());
        vars.insert("CXX".to_string(), cxx.to_string_lossy().to_string());
        vars.insert("HOMEBREW_CC".to_string(), cc.to_string_lossy().to_string());
        vars.insert("HOMEBREW_CXX".to_string(), cxx.to_string_lossy().to_string());
         println!("Setting CC={} CXX={}", cc.display(), cxx.display());

        // Set Compiler Flags (CFLAGS, CXXFLAGS, CPPFLAGS, LDFLAGS)
        let opt_level = "-O2"; // TODO: Make configurable (e.g., based on debug/release build)
        let sysroot_flag = if cfg!(target_os = "macos") && sdk_path != PathBuf::from("/") {
             format!("-isysroot {}", sdk_path.to_string_lossy())
         } else {
             String::new()
         };

        // Base flags common to C/C++ (Arch, Optimization, Sysroot)
        let mut base_flags_vec = vec![];
        if !arch_flag.is_empty() { base_flags_vec.push(arch_flag.as_str()); }
        base_flags_vec.push(opt_level);
         if !sysroot_flag.is_empty() { base_flags_vec.push(sysroot_flag.as_str()); }

        // Include paths (-I) - These go primarily in CPPFLAGS according to superenv
        let mut cppflags_vec = vec![];
        // Add sapphire prefix include first
         let prefix_include = sapphire_prefix.join("include");
         if prefix_include.is_dir() {
             cppflags_vec.push(format!("-I{}", prefix_include.display()));
         }
        // Add dependency includes (use runtime_dep_opt_paths or build_dep_opt_paths?)
        // Use the same paths used for PATH construction for consistency.
        for dep_path in runtime_dep_opt_paths { // Assuming these cover necessary includes
            let dep_include = dep_path.join("include");
            if dep_include.is_dir() {
                cppflags_vec.push(format!("-I{}", dep_include.display()));
            }
        }

        // CFLAGS = Base flags
        let cflags_vec = base_flags_vec.clone();

        // CXXFLAGS = Base flags + C++ specific flags (e.g., stdlib)
        let mut cxxflags_vec = base_flags_vec.clone();
         if cfg!(target_os = "macos") {
             // TODO: Implement more sophisticated C++ stdlib selection based on compiler/OS
             // See Homebrew's `compilers.rb` and `cxxstdlib.rb` for complexity.
             // Defaulting to libc++ is usually safe on modern macOS.
             let stdlib_flag = "-stdlib=libc++";
              println!("Adding default C++ stdlib flag: {}", stdlib_flag);
             cxxflags_vec.push(stdlib_flag);
         }

        // LDFLAGS = Library paths (-L) + Base Linker Flags (Arch, Sysroot)
        let mut ldflags_vec = vec![];
        // Add sapphire prefix lib first
        let prefix_lib = sapphire_prefix.join("lib");
         if prefix_lib.is_dir() {
             ldflags_vec.push(format!("-L{}", prefix_lib.display()));
         }
        // Add dependency libs (use runtime_dep_opt_paths or build_dep_opt_paths?)
        for dep_path in runtime_dep_opt_paths { // Assuming these cover necessary libs
            let dep_lib = dep_path.join("lib");
            if dep_lib.is_dir() {
                ldflags_vec.push(format!("-L{}", dep_lib.display()));
            }
        }
        // Add base linker flags (arch, sysroot - often needed by linker too)
        if !arch_flag.is_empty() { ldflags_vec.push(arch_flag.to_string()); }
        if !sysroot_flag.is_empty() { ldflags_vec.push(sysroot_flag.to_string()); }

        // Join flags into strings
        let cppflags = cppflags_vec.join(" ");
        let cflags = cflags_vec.join(" ");
        let cxxflags = cxxflags_vec.join(" ");
        let ldflags = ldflags_vec.join(" ");

        println!("Setting CPPFLAGS={}", cppflags);
        println!("Setting CFLAGS={}", cflags);
         println!("Setting CXXFLAGS={}", cxxflags);
        println!("Setting LDFLAGS={}", ldflags);

        vars.insert("CPPFLAGS".to_string(), cppflags.clone());
        vars.insert("CFLAGS".to_string(), cflags.clone());
        vars.insert("CXXFLAGS".to_string(), cxxflags.clone());
        vars.insert("LDFLAGS".to_string(), ldflags.clone());
        vars.insert("OBJCFLAGS".to_string(), cflags.clone()); // Usually same as CFLAGS
        vars.insert("OBJCXXFLAGS".to_string(), cxxflags.clone()); // Usually same as CXXFLAGS

        // Homebrew specific flag vars (often copies of standard ones)
        vars.insert("HOMEBREW_CPPFLAGS".to_string(), cppflags.clone());
        vars.insert("HOMEBREW_CFLAGS".to_string(), cflags.clone());
        vars.insert("HOMEBREW_CXXFLAGS".to_string(), cxxflags.clone());
        vars.insert("HOMEBREW_LDFLAGS".to_string(), ldflags.clone());
        vars.insert("HOMEBREW_OPTFLAGS".to_string(), opt_level.to_string());


        // Makeflags (Parallel build) - Use num_cpus or HOMEBREW_MAKE_JOBS if set
        let jobs = env::var("HOMEBREW_MAKE_JOBS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or_else(num_cpus::get);
        vars.insert("MAKEFLAGS".to_string(), format!("-j{}", jobs));
        println!("Setting MAKEFLAGS=-j{}", jobs);


        // Configure Tool Paths (pkg-config, aclocal, cmake) using dependency paths
        let mut pkg_config_path_dirs = Vec::new();
        let mut pkg_config_libdir_dirs = Vec::new();
        let mut aclocal_path_dirs = Vec::new();
        // Start CMAKE_PREFIX_PATH with the sapphire prefix itself, then add deps
        let mut cmake_prefix_path_dirs = vec![sapphire_prefix.to_path_buf()];

        // Add paths from dependencies (use build_dep_opt_paths for tools)
        for dep_path in build_dep_opt_paths {
             // Check if this path should be added for CMake (it likely should be)
             if !cmake_prefix_path_dirs.contains(dep_path) {
                 cmake_prefix_path_dirs.push(dep_path.clone());
             }

             let pkgconfig_lib = dep_path.join("lib/pkgconfig");
             let pkgconfig_share = dep_path.join("share/pkgconfig");
             let aclocal_share = dep_path.join("share/aclocal");

             if pkgconfig_lib.is_dir() {
                 pkg_config_path_dirs.push(pkgconfig_lib.clone());
                 pkg_config_libdir_dirs.push(pkgconfig_lib);
             }
             if pkgconfig_share.is_dir() {
                 pkg_config_path_dirs.push(pkgconfig_share.clone());
                  pkg_config_libdir_dirs.push(pkgconfig_share);
             }
             if aclocal_share.is_dir() {
                 aclocal_path_dirs.push(aclocal_share);
             }
        }

        // Add paths from sapphire prefix itself (potentially redundant if already in build_dep_opt_paths, but safe)
        let prefix_pkgconfig_lib = sapphire_prefix.join("lib/pkgconfig");
        let prefix_pkgconfig_share = sapphire_prefix.join("share/pkgconfig");
        let prefix_aclocal_share = sapphire_prefix.join("share/aclocal");

         if prefix_pkgconfig_lib.is_dir() && !pkg_config_path_dirs.contains(&prefix_pkgconfig_lib) {
             pkg_config_path_dirs.push(prefix_pkgconfig_lib.clone());
             pkg_config_libdir_dirs.push(prefix_pkgconfig_lib);
         }
         if prefix_pkgconfig_share.is_dir() && !pkg_config_path_dirs.contains(&prefix_pkgconfig_share) {
             pkg_config_path_dirs.push(prefix_pkgconfig_share.clone());
              pkg_config_libdir_dirs.push(prefix_pkgconfig_share);
         }
          if prefix_aclocal_share.is_dir() && !aclocal_path_dirs.contains(&prefix_aclocal_share) {
             aclocal_path_dirs.push(prefix_aclocal_share);
         }

        // Set environment variables if paths were found
        Self::set_path_list_var(&mut vars, "PKG_CONFIG_PATH", &pkg_config_path_dirs)?;
        Self::set_path_list_var(&mut vars, "PKG_CONFIG_LIBDIR", &pkg_config_libdir_dirs)?;
        Self::set_path_list_var(&mut vars, "ACLOCAL_PATH", &aclocal_path_dirs)?;
        // Deduplicate cmake paths before setting
         let mut unique_cmake_paths = Vec::new();
         for path in cmake_prefix_path_dirs {
             if !unique_cmake_paths.contains(&path) {
                 unique_cmake_paths.push(path);
             }
         }
        Self::set_path_list_var(&mut vars, "CMAKE_PREFIX_PATH", &unique_cmake_paths)?;


        // Homebrew also sets CMAKE_FRAMEWORK_PATH, CMAKE_INCLUDE_PATH, CMAKE_LIBRARY_PATH
        // based on these prefixes. Let's add them.
        let mut cmake_framework_path_dirs = vec![sapphire_prefix.join("Frameworks")];
        let mut cmake_include_path_dirs = vec![sapphire_prefix.join("include")];
        let mut cmake_library_path_dirs = vec![sapphire_prefix.join("lib")];

        // Use build_dep_opt_paths here too for consistency
        for dep_path in build_dep_opt_paths {
            cmake_framework_path_dirs.push(dep_path.join("Frameworks"));
            cmake_include_path_dirs.push(dep_path.join("include"));
            cmake_library_path_dirs.push(dep_path.join("lib"));
        }
         Self::set_path_list_var(&mut vars, "CMAKE_FRAMEWORK_PATH", &cmake_framework_path_dirs)?;
         Self::set_path_list_var(&mut vars, "CMAKE_INCLUDE_PATH", &cmake_include_path_dirs)?;
         Self::set_path_list_var(&mut vars, "CMAKE_LIBRARY_PATH", &cmake_library_path_dirs)?;

        // Final check for required variables (e.g., HOME, PATH)
        if !vars.contains_key("HOME") || vars["HOME"].is_empty() {
             return Err(SapphireError::BuildEnvError("HOME environment variable is missing or empty after setup.".to_string()));
        }
         if !vars.contains_key("PATH") || vars["PATH"].is_empty() {
             return Err(SapphireError::BuildEnvError("PATH environment variable is missing or empty after setup.".to_string()));
        }


        println!("BuildEnvironment created successfully.");

        Ok(Self {
            vars,
            path_dirs, // Store the final ordered list
            sapphire_prefix: sapphire_prefix.to_path_buf(),
            formula_install_prefix,
            cc,
            cxx,
            sdk_path,
        })
    }

    /// Helper to check if a HOMEBREW_* var is one we explicitly control or keep.
    fn is_controlled_homebrew_var(key: &str) -> bool {
        match key {
            "HOMEBREW_CC" | "HOMEBREW_CXX" | "HOMEBREW_CFLAGS" | "HOMEBREW_CXXFLAGS" |
            "HOMEBREW_CPPFLAGS" | "HOMEBREW_LDFLAGS" | "HOMEBREW_OPTFLAGS" |
            "HOMEBREW_TEMP" | "HOMEBREW_CACHE" | "HOMEBREW_LOGS" | "HOMEBREW_PREFIX" |
            "HOMEBREW_CELLAR" | "HOMEBREW_REPOSITORY" | // HOMEBREW_LIBRARY (usually derived)
            "HOMEBREW_MAKE_JOBS" // Check if we want to keep user's pref here? Currently we override.
             => true,
            _ => false,
        }
    }


    /// Helper function to join a list of paths and set an environment variable.
    /// Filters out non-existent directories. Uses ":" as separator.
    fn set_path_list_var(vars: &mut HashMap<String, String>, name: &str, paths: &[PathBuf]) -> Result<()> {
        let existing_paths: Vec<String> = paths.iter()
            .filter(|p| p.is_dir()) // Only include existing directories
            .filter_map(|p| p.to_str()) // Convert to &str, filtering None
            .map(|s| s.to_string()) // Convert to String
            .collect();

        if !existing_paths.is_empty() {
            // Deduplicate while preserving order
            let mut unique_paths = Vec::new();
            let mut seen = HashSet::new();
            for path in existing_paths {
                 if seen.insert(path.clone()) {
                     unique_paths.push(path);
                 }
            }

            let joined_path = unique_paths.join(":"); // Use ":" as separator

             if !joined_path.is_empty() {
                 println!("Setting {}={}", name, joined_path);
                 vars.insert(name.to_string(), joined_path);
             } else {
                  println!("No valid directories found for {}, not setting variable.", name);
             }
        } else {
             println!("No existing directories provided for {}, not setting variable.", name);
        }
        Ok(())
    }


    /// Applies the sanitized environment to a `std::process::Command`.
    ///
    /// This clears the command's existing environment and populates it
    /// with the variables defined in this `BuildEnvironment`.
    pub fn apply_to_command(&self, command: &mut Command) {
        command.env_clear(); // Start clean before applying our vars
        command.envs(&self.vars);
        // Avoid printing the full command with env vars here as it can be very verbose.
        // Log the command name and args separately if needed before execution.
        // println!("Applied sanitized environment to command: {:?}", command);
        println!("Applying sanitized environment to command: {:?}", command.get_program());
        println!("  Arguments: {:?}", command.get_args().collect::<Vec<_>>());
        // Example of logging a few key env vars:
        // println!("  Env PATH: {}", self.get_var("PATH").unwrap_or("N/A"));
        // println!("  Env CFLAGS: {}", self.get_var("CFLAGS").unwrap_or("N/A"));
    }

    /// Gets the configured PATH string.
    pub fn get_path_string(&self) -> Option<&str> {
        self.vars.get("PATH").map(|s| s.as_str())
    }

    /// Gets the configured target installation prefix for the formula.
    #[allow(dead_code)] // Keep for potential future use
    pub fn get_formula_install_prefix(&self) -> &Path {
        &self.formula_install_prefix
    }

    /// Gets a specific variable from the sanitized environment.
    #[allow(dead_code)] // Keep for potential future use
    pub fn get_var(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(|s| s.as_str())
    }

    /// Gets the full map of environment variables.
    pub fn get_vars(&self) -> &HashMap<String, String> {
        &self.vars
    }
}


// --- Ensure Dependencies are in sapphire-core's Cargo.toml ---
// [dependencies]
// num_cpus = "1.16" # Or latest
// which = "4.4" # Or latest -> Make sure it's at least 4.0 for which_in
// tempfile = "3.8" # Or latest for temp dir management
// log = "0.4" # If using log crate
// thiserror = "1.0" # If using thiserror for SapphireError