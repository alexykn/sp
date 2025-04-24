// sapphire-core/src/build/env.rs
// *** No major changes needed here for this specific fix, but ensure PERL5LIB/PYTHONPATH handling
// is correct ***

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::build::devtools;
use crate::model::formula::FormulaDependencies;
use crate::utils::error::{Result, SapphireError};

// Constants remain the same...
const ENV_VARS_TO_REMOVE: &[&str] = &[
    "RUBYLIB",
    "RUBYOPT",
    "RUBYPATH",
    "RBENV_VERSION",
    "CHRUBY_RUBY",
    "GEM_HOME",
    "GEM_PATH",
    "GEM_CACHE",
    "PYTHONHOME",
    "PYTHONPATH",
    "PYTHONEXECUTABLE",
    "PIP_REQUIRE_VIRTUALENV",
    "PERL5LIB",
    "PERL_MB_OPT",
    "PERL_MM_OPT",
    "NODE_PATH",
    "GOENV_ROOT",
    "GOPATH",
    "GOBIN",
    "R_ENVIRON_USER",
    "R_PROFILE_USER",
    "JAVA_HOME",
    "_JAVA_OPTIONS",
    "CLASSPATH",
    "JAVA_TOOL_OPTIONS",
    "OBJC_INCLUDE_PATH",
    "MAKEFLAGS",
    "MAKELEVEL",
    "CMAKE_PREFIX_PATH",
    "CMAKE_INCLUDE_PATH",
    "CMAKE_LIBRARY_PATH",
    "CMAKE_FRAMEWORK_PATH",
    "PKG_CONFIG_PATH",
    "PKG_CONFIG_LIBDIR",
    "PKG_CONFIG_SYSROOT_DIR",
    "CPATH",
    "INCLUDE",
    "INCLUDE_PATH",
    "LIBRARY_PATH",
    "LIBPATH",
    "SDKROOT",
    "LDFLAGS",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "OBJCFLAGS",
    "OBJCXXFLAGS",
    "CC",
    "CXX",
    "CPP",
    "LD",
    "DEBUG",
    "GREP_OPTIONS",
    "HOMEBREW_DEBUG",
    "HOMEBREW_VERBOSE",
    "HOMEBREW_DEVELOPER",
    "HOMEBREW_OPTIMIZATION_LEVEL",
    "HOMEBREW_ARCH",
    "HOMEBREW_ARTIFACT_DOMAIN",
    "HOMEBREW_AUTO_UPDATE_SECS",
    "HOMEBREW_BAT",
    "HOMEBREW_BUILD_BOTTLE",
    "HOMEBREW_BUILD_FROM_SOURCE",
    "HOMEBREW_CACHE",
    "HOMEBREW_CASK_OPTS",
    "HOMEBREW_CLEANUP_MAX_AGE_DAYS",
    "HOMEBREW_CORE_GIT_REMOTE",
    "HOMEBREW_CURL_RETRIES",
    "HOMEBREW_CURL_VERBOSE",
    "HOMEBREW_DEVELOPER_DIR",
    "HOMEBREW_DISABLE_LOAD_FORMULA",
    "HOMEBREW_DISPLAY",
    "HOMEBREW_DISPLAY_INSTALL_TIMES",
    "HOMEBREW_ENV_PASSTHROUGH",
    "HOMEBREW_FORCE_BREWED_CA_CERTIFICATES",
    "HOMEBREW_GIT_EMAIL",
    "HOMEBREW_GIT_NAME",
    "HOMEBREW_GITHUB_API_TOKEN",
    "HOMEBREW_INSTALL_BADGE",
    "HOMEBREW_LOGS",
    "HOMEBREW_MAKE_JOBS",
    "HOMEBREW_NO_ANALYTICS",
    "HOMEBREW_NO_AUTO_UPDATE",
    "HOMEBREW_NO_BOTTLE_SOURCE_FALLBACK",
    "HOMEBREW_NO_COLOR",
    "HOMEBREW_NO_COMPAT",
    "HOMEBREW_NO_EMOJI",
    "HOMEBREW_NO_ENV_HINTS",
    "HOMEBREW_NO_FONT_AWESOME",
    "HOMEBREW_NO_GIT_REPO",
    "HOMEBREW_NO_INSECURE_REDIRECT",
    "HOMEBREW_NO_INSTALL_CLEANUP",
    "HOMEBREW_NO_INSTALL_FROM_API",
    "HOMEBREW_PREFIX",
    "HOMEBREW_PRY",
    "HOMEBREW_SKIP_OR_LATER_BLOCK",
    "HOMEBREW_TEMP",
    "HOMEBREW_UPDATE_TO_TAG",
    "HOMEBREW_USE_RUBY_FROM_PATH",
    "HOMEBREW_VERBOSE_USING_DOTS",
    "HOMEBREW_API_DOMAIN",
    "HOMEBREW_BOTTLE_DOMAIN",
    "HOMEBREW_BREW_GIT_REMOTE",
    "HOMEBREW_FORCE_HOMEBREW_ON_LINUX",
    "HOMEBREW_SORBET_RUNTIME",
    "HOMEBREW_SYSTEM_ENV_PASSTHROUGH",
];
const ENV_VARS_TO_KEEP: &[&str] = &[
    "USER",
    "LOGNAME",
    "HOME",
    "TMPDIR",
    "TERM",
    "SHELL",
    "EDITOR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "DISPLAY",
    "XAUTHORITY",
    "TZ",
];

/// Represents the sanitized build environment, mimicking Homebrew's "superenv".
#[derive(Debug, Clone)]
pub struct BuildEnvironment {
    /// The final map of environment variables to be used for build commands.
    vars: HashMap<String, String>,
    /// The ordered list of directories constituting the final PATH.
    #[allow(dead_code)]
    path_dirs: Vec<PathBuf>,
    /// The root installation directory for Sapphire (e.g., /opt/homebrew or /usr/local).
    #[allow(dead_code)]
    sapphire_prefix: PathBuf,
    /// The specific installation prefix for the formula being built.
    #[allow(dead_code)]
    formula_install_prefix: PathBuf,
    /// Resolved path to the C compiler.
    #[allow(dead_code)]
    cc: PathBuf,
    /// Resolved path to the C++ compiler.
    #[allow(dead_code)]
    cxx: PathBuf,
    /// Resolved path to the macOS SDK (or "/" if not applicable).
    #[allow(dead_code)]
    sdk_path: PathBuf,
}

impl BuildEnvironment {
    /// Creates a new sanitized build environment for a given formula.
    pub fn new<F: FormulaDependencies>(
        formula: &F,
        sapphire_prefix: &Path,
        cellar_path: &Path,
        all_installed_opt_paths: &[PathBuf],
    ) -> Result<Self> {
        debug!(
            "Creating BuildEnvironment for formula '{}'...",
            formula.name()
        );
        debug!("Sapphire Prefix: {}", sapphire_prefix.display());
        debug!(
            "Provided installed dependency OPT paths: {:?}",
            all_installed_opt_paths
        );

        let mut vars = HashMap::new();
        let mut path_dirs = Vec::new();

        filter_initial_environment(&mut vars);
        debug!("Initial environment filtering complete.");

        let cc = devtools::find_compiler("cc")?;
        let cxx = devtools::find_compiler("c++")?;
        let sdk_path = devtools::find_sdk_path()?;
        let macos_version = devtools::get_macos_version()?;
        let arch_flag = devtools::get_arch_flag();
        let formula_install_prefix = formula.install_prefix(cellar_path)?;

        debug!(
            "Resolved tools: CC={}, CXX={}, SDK={}, macOS={}, ArchFlag='{}', InstallPrefix={}",
            cc.display(),
            cxx.display(),
            sdk_path.display(),
            macos_version,
            arch_flag,
            formula_install_prefix.display()
        );

        let mut include_paths = Vec::new();
        let mut lib_paths = Vec::new();
        let mut pkgconfig_paths = Vec::new();
        let mut aclocal_paths = Vec::new();
        let mut cmake_prefix_paths = Vec::new();
        let mut cmake_framework_paths = Vec::new();

        debug!("Processing provided dependency paths for environment...");
        for dep_opt_path in all_installed_opt_paths {
            debug!("Adding paths for dependency: {}", dep_opt_path.display());
            let bin_path = dep_opt_path.join("bin");
            if bin_path.is_dir() {
                path_dirs.push(bin_path);
            }
            let sbin_path = dep_opt_path.join("sbin");
            if sbin_path.is_dir() {
                path_dirs.push(sbin_path);
            }
            let include_path = dep_opt_path.join("include");
            if include_path.is_dir() {
                include_paths.push(include_path);
            }
            let lib_path = dep_opt_path.join("lib");
            if lib_path.is_dir() {
                lib_paths.push(lib_path.clone());
                let pkgconfig_lib_path = lib_path.join("pkgconfig");
                if pkgconfig_lib_path.is_dir() {
                    pkgconfig_paths.push(pkgconfig_lib_path);
                }
            }
            let share_path = dep_opt_path.join("share");
            if share_path.is_dir() {
                let pkgconfig_share_path = share_path.join("pkgconfig");
                if pkgconfig_share_path.is_dir() {
                    pkgconfig_paths.push(pkgconfig_share_path);
                }
                let aclocal_share_path = share_path.join("aclocal");
                if aclocal_share_path.is_dir() {
                    aclocal_paths.push(aclocal_share_path);
                }
            }
            let framework_path = dep_opt_path.join("Frameworks");
            if framework_path.is_dir() {
                cmake_framework_paths.push(framework_path);
            }
            if dep_opt_path.is_dir() {
                cmake_prefix_paths.push(dep_opt_path.clone());
            }
        }
        debug!("Dependency paths collected.");

        let sapphire_bin = sapphire_prefix.join("bin");
        if sapphire_bin.is_dir() {
            path_dirs.insert(0, sapphire_bin);
        }
        let sapphire_sbin = sapphire_prefix.join("sbin");
        if sapphire_sbin.is_dir() {
            path_dirs.insert(0, sapphire_sbin);
        }
        debug!("Prepended Sapphire bin/sbin to PATH list.");

        if let Some(compiler_bin) = cc.parent() {
            path_dirs.insert(0, compiler_bin.to_path_buf());
            debug!(
                "Prepended compiler bin to PATH list: {}",
                compiler_bin.display()
            );
        }
        let standard_paths = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
        for spath in standard_paths.iter().map(PathBuf::from) {
            if !path_dirs
                .iter()
                .any(|p| p == &spath || p.starts_with(&spath))
            {
                path_dirs.push(spath);
            }
        }

        let mut unique_path_dirs = Vec::new();
        let mut seen_paths = HashSet::new();
        for dir in path_dirs {
            if seen_paths.insert(dir.clone()) {
                unique_path_dirs.push(dir);
            }
        }
        path_dirs = unique_path_dirs;

        let final_path_string = std::env::join_paths(path_dirs.iter())
            .map_err(|e| SapphireError::BuildEnvError(format!("Failed to join PATH: {e}")))?
            .into_string()
            .map_err(|os_str| {
                SapphireError::BuildEnvError(format!(
                    "Final PATH contains non-UTF8 characters: {os_str:?}"
                ))
            })?;
        vars.insert("PATH".to_string(), final_path_string.clone());
        debug!("Final PATH: {}", final_path_string);

        if cfg!(target_os = "macos") {
            if sdk_path != PathBuf::from("/") {
                vars.insert(
                    "SDKROOT".to_string(),
                    sdk_path.to_string_lossy().to_string(),
                );
            }
            vars.insert(
                "MACOSX_DEPLOYMENT_TARGET".to_string(),
                macos_version.clone(),
            );
            debug!(
                "Set SDKROOT={} MACOSX_DEPLOYMENT_TARGET={}",
                sdk_path.display(),
                macos_version
            );
        }

        vars.insert("CC".to_string(), cc.to_string_lossy().to_string());
        vars.insert("CXX".to_string(), cxx.to_string_lossy().to_string());
        let stdlib_flag = if cfg!(target_os = "macos") {
            "-stdlib=libc++"
        } else {
            ""
        };
        debug!("Set CC={} CXX={}", cc.display(), cxx.display());
        if !stdlib_flag.is_empty() {
            debug!("Adding default C++ stdlib flag: {}", stdlib_flag);
        }

        let cppflags = include_paths
            .iter()
            .map(|p| format!("-I{}", p.display()))
            .collect::<Vec<_>>()
            .join(" ");
        vars.insert("CPPFLAGS".to_string(), cppflags.clone());
        debug!("Set CPPFLAGS={}", cppflags);

        let sysroot_flag = if cfg!(target_os = "macos") && sdk_path != PathBuf::from("/") {
            format!("-isysroot {}", sdk_path.display())
        } else {
            String::new()
        };
        let cflags = format!("{arch_flag} -O2 {sysroot_flag}").trim().to_string();
        vars.insert("CFLAGS".to_string(), cflags.clone());
        debug!("Set CFLAGS={}", cflags);

        let cxxflags = format!("{cflags} {stdlib_flag}").trim().to_string();
        vars.insert("CXXFLAGS".to_string(), cxxflags.clone());
        debug!("Set CXXFLAGS={}", cxxflags);

        let ldflags_lib_part = lib_paths
            .iter()
            .map(|p| format!("-L{}", p.display()))
            .collect::<Vec<_>>()
            .join(" ");
        let ldflags = format!("{ldflags_lib_part} {arch_flag} {sysroot_flag}")
            .trim()
            .to_string();
        vars.insert("LDFLAGS".to_string(), ldflags.clone());
        debug!("Set LDFLAGS={}", ldflags);

        let jobs = num_cpus::get().to_string();
        vars.insert("MAKEFLAGS".to_string(), format!("-j{jobs}"));
        debug!("Set MAKEFLAGS=-j{}", jobs);

        Self::set_path_list_var(&mut vars, "PKG_CONFIG_PATH", &pkgconfig_paths)?;
        Self::set_path_list_var(&mut vars, "PKG_CONFIG_LIBDIR", &pkgconfig_paths)?;
        Self::set_path_list_var(&mut vars, "ACLOCAL_PATH", &aclocal_paths)?;
        Self::set_path_list_var(&mut vars, "CMAKE_PREFIX_PATH", &cmake_prefix_paths)?;
        Self::set_path_list_var(&mut vars, "CMAKE_FRAMEWORK_PATH", &cmake_framework_paths)?;
        Self::set_path_list_var(&mut vars, "CMAKE_INCLUDE_PATH", &include_paths)?;
        Self::set_path_list_var(&mut vars, "CMAKE_LIBRARY_PATH", &lib_paths)?;

        // *** Ensure PERL5LIB and PYTHONPATH are NOT set globally here ***
        // They should be handled specifically during resource installation or via wrapper scripts.
        // The initial filtering should remove them, but double-check they aren't added back.
        if vars.contains_key("PERL5LIB") {
            tracing::warn!("PERL5LIB unexpectedly present in global build env, removing.");
            vars.remove("PERL5LIB");
        }
        if vars.contains_key("PYTHONPATH") {
            tracing::warn!("PYTHONPATH unexpectedly present in global build env, removing.");
            vars.remove("PYTHONPATH");
        }

        debug!("BuildEnvironment created successfully.");

        Ok(Self {
            vars,
            path_dirs, // Keep for reference
            sapphire_prefix: sapphire_prefix.to_path_buf(),
            formula_install_prefix,
            cc,
            cxx,
            sdk_path,
        })
    }

    // is_controlled_homebrew_var remains unchanged
    fn is_controlled_homebrew_var(key: &str) -> bool {
        matches!(
            key,
            "HOMEBREW_CC"
                | "HOMEBREW_CXX"
                | "HOMEBREW_CFLAGS"
                | "HOMEBREW_CXXFLAGS"
                | "HOMEBREW_CPPFLAGS"
                | "HOMEBREW_LDFLAGS"
                | "HOMEBREW_OPTFLAGS"
                | "HOMEBREW_TEMP"
                | "HOMEBREW_CACHE"
                | "HOMEBREW_LOGS"
                | "HOMEBREW_PREFIX"
                | "HOMEBREW_CELLAR"
                | "HOMEBREW_REPOSITORY"
                | "HOMEBREW_MAKE_JOBS"
        )
    }

    // set_path_list_var remains unchanged
    fn set_path_list_var(
        vars: &mut HashMap<String, String>,
        name: &str,
        paths: &[PathBuf],
    ) -> Result<()> {
        let existing_paths: Vec<String> = paths
            .iter()
            .filter(|p| p.is_dir())
            .filter_map(|p| p.to_str())
            .map(|s| s.to_string())
            .collect();
        if !existing_paths.is_empty() {
            let mut unique_paths = Vec::new();
            let mut seen = HashSet::new();
            for path in existing_paths {
                if seen.insert(path.clone()) {
                    unique_paths.push(path);
                }
            }
            let joined_path = unique_paths.join(":");
            if !joined_path.is_empty() {
                debug!("Setting {}={}", name, joined_path);
                vars.insert(name.to_string(), joined_path);
            } else {
                debug!(
                    "No valid directories found for {}, not setting variable.",
                    name
                );
            }
        } else {
            debug!(
                "No existing directories provided for {}, not setting variable.",
                name
            );
        }
        Ok(())
    }

    /// Applies the sanitized environment to a `std::process::Command`.
    pub fn apply_to_command(&self, command: &mut std::process::Command) {
        // Unchanged
        command.env_clear();
        command.envs(&self.vars);
        debug!(
            "Applying sanitized environment to command: {:?}",
            command.get_program()
        );
        // Avoid logging args verbosely unless needed
        // debug!("  Arguments: {:?}", command.get_args().collect::<Vec<_>>());
    }

    /// Gets the configured PATH string.
    pub fn get_path_string(&self) -> Option<&str> {
        // Unchanged
        self.vars.get("PATH").map(|s| s.as_str())
    }

    /// Gets the full map of environment variables.
    pub fn get_vars(&self) -> &HashMap<String, String> {
        // Unchanged
        &self.vars
    }

    /// Gets a specific variable from the sanitized environment.
    pub fn get_var(&self, key: &str) -> Option<&str> {
        // Unchanged
        self.vars.get(key).map(|s| s.as_str())
    }
}

/// Filters the initial environment, keeping only specified safe variables.
fn filter_initial_environment(vars: &mut HashMap<String, String>) {
    // Unchanged
    let initial_env: HashMap<String, String> = std::env::vars().collect();
    let vars_to_remove_set: HashSet<&str> = ENV_VARS_TO_REMOVE.iter().cloned().collect();
    let vars_to_keep_set: HashSet<&str> = ENV_VARS_TO_KEEP.iter().cloned().collect();
    *vars = HashMap::new();
    for (key, value) in initial_env.iter() {
        let key_upper = key.to_uppercase();
        if vars_to_remove_set.contains(key.as_str()) {
            debug!("Removing env var: {}", key);
            continue;
        }
        if key_upper.starts_with("HOMEBREW_") && !BuildEnvironment::is_controlled_homebrew_var(key)
        {
            debug!("Removing potentially interfering Homebrew env var: {}", key);
            continue;
        }
        if vars_to_keep_set.contains(key.as_str()) {
            debug!("Keeping env var: {}", key);
            vars.insert(key.clone(), value.clone());
        }
    }
}
