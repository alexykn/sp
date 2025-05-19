// sps/src/cli/init.rs
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::Arc;

use clap::Args;
use colored::Colorize;
use sps_common::config::Config; // Assuming Config is correctly in sps_common
use sps_common::error::{Result as SpsResult, SpsError};
use tempfile;
use tracing::{debug, error, info, warn};

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Force initialization even if /opt/sps appears to be an sps root already.
    #[arg(long)]
    pub force: bool,
}

impl InitArgs {
    pub async fn run(&self, config: &Config) -> SpsResult<()> {
        info!(
            "Initializing sps environment at {}",
            config.sps_root().display()
        );

        let sps_root = config.sps_root();
        let marker_path = config.sps_root_marker_path();

        // 1. Initial Checks (as current user) - (No change from your existing logic)
        if sps_root.exists() {
            let is_empty = match fs::read_dir(sps_root) {
                Ok(mut entries) => entries.next().is_none(),
                Err(_) => false, // If we can't read it, assume not empty or not accessible
            };

            if marker_path.exists() && !self.force {
                info!(
                    "{} already exists. sps appears to be initialized. Use --force to re-initialize.",
                    marker_path.display()
                );
                return Ok(());
            }
            if !self.force && !is_empty && !marker_path.exists() {
                warn!(
                    "Directory {} exists but does not appear to be an sps root (missing marker {}).",
                    sps_root.display(),
                    marker_path.file_name().unwrap_or_default().to_string_lossy()
                );
                warn!(
                    "Run with --force to initialize anyway (this might overwrite existing data or change permissions)."
                );
                return Err(SpsError::Config(format!(
                    "{} exists but is not a recognized sps root. Aborting.",
                    sps_root.display()
                )));
            }
            if self.force {
                info!(
                    "--force specified. Proceeding with initialization in {}.",
                    sps_root.display()
                );
            } else if is_empty {
                info!(
                    "Directory {} exists but is empty. Proceeding with initialization.",
                    sps_root.display()
                );
            }
        }

        // 2. Privileged Operations
        let current_user_name = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .map_err(|_| {
                SpsError::Generic(
                    "Failed to get current username from USER or LOGNAME environment variables."
                        .to_string(),
                )
            })?;

        let target_group_name = if cfg!(target_os = "macos") {
            "admin" // Standard admin group on macOS
        } else {
            // For Linux, 'staff' might not exist or be appropriate.
            // Often, the user's primary group is used, or a dedicated 'brew' group.
            // For simplicity, let's try to use the current user's name as the group too,
            // which works if the user has a group with the same name.
            // A more robust Linux solution might involve checking for 'staff' or other common
            // groups.
            &current_user_name
        };

        info!(
            "Will attempt to set ownership of sps-managed directories under {} to {}:{}",
            sps_root.display(),
            current_user_name,
            target_group_name
        );

        println!(
            "{}",
            format!(
                "sps may require sudo to create directories and set permissions in {}.",
                sps_root.display()
            )
            .yellow()
        );

        // Define directories sps needs to ensure exist and can manage.
        // These are derived from your Config struct.
        let dirs_to_create_and_manage: Vec<PathBuf> = vec![
            config.sps_root().to_path_buf(), // The root itself
            config.bin_dir(),
            config.cellar_dir(),
            config.cask_room_dir(),
            config.cask_store_dir(), // sps-specific
            config.opt_dir(),
            config.taps_dir(),  // This is now sps_root/Library/Taps
            config.cache_dir(), // sps-specific (e.g., sps_root/sps_cache)
            config.logs_dir(),  // sps-specific (e.g., sps_root/sps_logs)
            config.tmp_dir(),
            config.state_dir(),
            config
                .man_base_dir()
                .parent()
                .unwrap_or(sps_root)
                .to_path_buf(), // share
            config.man_base_dir(), // share/man
            config.sps_root().join("etc"),
            config.sps_root().join("include"),
            config.sps_root().join("lib"),
            config.sps_root().join("share/doc"),
        ];

        // Create directories with mkdir -p (non-destructive)
        for dir_path in &dirs_to_create_and_manage {
            // Only create if it doesn't exist to avoid unnecessary sudo calls if already present
            if !dir_path.exists() {
                debug!(
                    "Ensuring directory exists with sudo: {}",
                    dir_path.display()
                );
                run_sudo_command("mkdir", &["-p", &dir_path.to_string_lossy()])?;
            } else {
                debug!(
                    "Directory already exists, skipping mkdir: {}",
                    dir_path.display()
                );
            }
        }

        // Create the marker file (non-destructive to other content)
        debug!(
            "Creating/updating marker file with sudo: {}",
            marker_path.display()
        );
        let marker_content = "sps root directory version 1";
        // Using a temporary file for sudo tee to avoid permission issues with direct pipe
        let temp_marker_file =
            tempfile::NamedTempFile::new().map_err(|e| SpsError::Io(Arc::new(e)))?;
        fs::write(temp_marker_file.path(), marker_content)
            .map_err(|e| SpsError::Io(Arc::new(e)))?;
        run_sudo_command(
            "cp",
            &[
                temp_marker_file.path().to_str().unwrap(),
                marker_path.to_str().unwrap(),
            ],
        )?;

        #[cfg(unix)]
        {
            // More targeted chown and chmod
            info!(
                "Setting ownership and permissions for sps-managed directories under {}...",
                sps_root.display()
            );

            // Chown/Chmod the top-level sps_root directory itself (non-recursively for chmod
            // initially) This is important if sps_root is /opt/sps and was just created
            // by root. If sps_root is /opt/homebrew, this ensures the current user can
            // at least manage it.
            run_sudo_command(
                "chown",
                &[
                    &format!("{current_user_name}:{target_group_name}"),
                    &sps_root.to_string_lossy(),
                ],
            )?;
            run_sudo_command("chmod", &["ug=rwx,o=rx", &sps_root.to_string_lossy()])?; // 755 for the root

            // For specific subdirectories that sps actively manages and writes into frequently,
            // ensure they are owned by the user and have appropriate permissions.
            // We apply this recursively to sps-specific dirs and key shared dirs.
            let dirs_for_recursive_chown_chmod: Vec<PathBuf> = vec![
                config.cellar_dir(),
                config.cask_room_dir(),
                config.cask_store_dir(), // sps-specific, definitely needs full user control
                config.opt_dir(),
                config.taps_dir(),
                config.cache_dir(), // sps-specific
                config.logs_dir(),  // sps-specific
                config.tmp_dir(),
                config.state_dir(),
                // bin, lib, include, share, etc are often symlink farms.
                // The top-level of these should be writable by the user to create symlinks.
                // The actual kegs in Cellar will have their own permissions.
                config.bin_dir(),
                config.sps_root().join("lib"),
                config.sps_root().join("include"),
                config.sps_root().join("share"),
                config.sps_root().join("etc"),
            ];

            for dir_path in dirs_for_recursive_chown_chmod {
                if dir_path.exists() {
                    // Only operate on existing directories
                    debug!("Setting ownership (recursive) for: {}", dir_path.display());
                    run_sudo_command(
                        "chown",
                        &[
                            "-R",
                            &format!("{current_user_name}:{target_group_name}"),
                            &dir_path.to_string_lossy(),
                        ],
                    )?;

                    debug!(
                        "Setting permissions (recursive ug=rwX,o=rX) for: {}",
                        dir_path.display()
                    );
                    run_sudo_command("chmod", &["-R", "ug=rwX,o=rX", &dir_path.to_string_lossy()])?;
                } else {
                    warn!(
                        "Directory {} was expected but not found for chown/chmod. Marker: {}",
                        dir_path.display(),
                        marker_path.display()
                    );
                }
            }

            // Ensure bin is executable by all
            if config.bin_dir().exists() {
                debug!(
                    "Ensuring execute permissions for bin_dir: {}",
                    config.bin_dir().display()
                );
                run_sudo_command("chmod", &["a+x", &config.bin_dir().to_string_lossy()])?;
                // Also ensure contents of bin (wrappers, symlinks) are executable if they weren't
                // caught by -R ug=rwX This might be redundant if -R ug=rwX
                // correctly sets X for existing executables, but explicit `chmod
                // a+x` on individual files might be needed if they are newly created by sps.
                // For now, relying on the recursive chmod and the a+x on the bin_dir itself.
            }

            // Debug listing (optional, can be verbose)
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!("Listing {} after permission changes:", sps_root.display());
                let ls_output_root = StdCommand::new("ls").arg("-ld").arg(sps_root).output();
                if let Ok(out) = ls_output_root {
                    debug!(
                        "ls -ld {}: \nSTDOUT: {}\nSTDERR: {}",
                        sps_root.display(),
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
                for dir_path in &dirs_to_create_and_manage {
                    if dir_path.exists() && dir_path != sps_root {
                        let ls_output_sub = StdCommand::new("ls").arg("-ld").arg(dir_path).output();
                        if let Ok(out) = ls_output_sub {
                            debug!(
                                "ls -ld {}: \nSTDOUT: {}\nSTDERR: {}",
                                dir_path.display(),
                                String::from_utf8_lossy(&out.stdout),
                                String::from_utf8_lossy(&out.stderr)
                            );
                        }
                    }
                }
            }
        }

        // 3. User-Specific PATH Configuration (runs as the original user) - (No change from your
        //    existing logic)
        if let Err(e) = configure_shell_path(config, &current_user_name) {
            warn!(
                "Could not fully configure shell PATH: {}. Manual configuration might be needed.",
                e
            );
            print_manual_path_instructions(&config.bin_dir().to_string_lossy());
        }

        info!(
            "{} {}",
            "Successfully initialized sps environment at".green(),
            config.sps_root().display().to_string().green()
        );
        Ok(())
    }
}

// run_sudo_command helper (no change from your existing logic)
fn run_sudo_command(command: &str, args: &[&str]) -> SpsResult<()> {
    debug!("Running sudo {} {:?}", command, args);
    let output = StdCommand::new("sudo")
        .arg(command)
        .args(args)
        .output()
        .map_err(|e| SpsError::Io(Arc::new(e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        error!(
            "sudo {} {:?} failed ({}):\nStdout: {}\nStderr: {}",
            command,
            args,
            output.status,
            stdout.trim(),
            stderr.trim()
        );
        Err(SpsError::Generic(format!(
            "Failed to execute `sudo {} {:?}`: {}",
            command,
            args,
            stderr.trim()
        )))
    } else {
        Ok(())
    }
}

// configure_shell_path helper (no change from your existing logic)
fn configure_shell_path(config: &Config, current_user_name_for_log: &str) -> SpsResult<()> {
    info!("Attempting to configure your shell for sps PATH...");

    let sps_bin_path_str = config.bin_dir().to_string_lossy().into_owned();
    let home_dir = config.home_dir();
    if home_dir == PathBuf::from("/") && current_user_name_for_log != "root" {
        warn!(
            "Could not reliably determine your home directory (got '/'). Please add {} to your PATH manually for user {}.",
            sps_bin_path_str, current_user_name_for_log
        );
        print_manual_path_instructions(&sps_bin_path_str);
        return Ok(());
    }

    let shell_path_env = std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string());
    let mut config_files_updated: Vec<String> = Vec::new();
    let mut path_seems_configured = false;

    let sps_path_line_zsh_bash = format!("export PATH=\"{sps_bin_path_str}:$PATH\"");
    let sps_path_line_fish = format!("fish_add_path -P \"{sps_bin_path_str}\"");

    if shell_path_env.contains("zsh") {
        let zshrc_path = home_dir.join(".zshrc");
        if update_shell_config(
            &zshrc_path,
            &sps_path_line_zsh_bash,
            &sps_bin_path_str,
            "Zsh",
            false,
        )? {
            config_files_updated.push(zshrc_path.display().to_string());
        } else if line_exists_in_file(&zshrc_path, &sps_bin_path_str)? {
            path_seems_configured = true;
        }
    } else if shell_path_env.contains("bash") {
        let bashrc_path = home_dir.join(".bashrc");
        let bash_profile_path = home_dir.join(".bash_profile");
        let profile_path = home_dir.join(".profile");

        let mut bash_updated_by_sps = false;
        if update_shell_config(
            &bashrc_path,
            &sps_path_line_zsh_bash,
            &sps_bin_path_str,
            "Bash (.bashrc)",
            false,
        )? {
            config_files_updated.push(bashrc_path.display().to_string());
            bash_updated_by_sps = true;
            if bash_profile_path.exists() {
                ensure_profile_sources_rc(&bash_profile_path, &bashrc_path, "Bash (.bash_profile)");
            } else if profile_path.exists() {
                ensure_profile_sources_rc(&profile_path, &bashrc_path, "Bash (.profile)");
            } else {
                info!("Neither .bash_profile nor .profile found. Creating .bash_profile to source .bashrc.");
                ensure_profile_sources_rc(&bash_profile_path, &bashrc_path, "Bash (.bash_profile)");
            }
        } else if update_shell_config(
            &bash_profile_path,
            &sps_path_line_zsh_bash,
            &sps_bin_path_str,
            "Bash (.bash_profile)",
            false,
        )? {
            config_files_updated.push(bash_profile_path.display().to_string());
            bash_updated_by_sps = true;
        } else if update_shell_config(
            &profile_path,
            &sps_path_line_zsh_bash,
            &sps_bin_path_str,
            "Bash (.profile)",
            false,
        )? {
            config_files_updated.push(profile_path.display().to_string());
            bash_updated_by_sps = true;
        }

        if !bash_updated_by_sps
            && (line_exists_in_file(&bashrc_path, &sps_bin_path_str)?
                || line_exists_in_file(&bash_profile_path, &sps_bin_path_str)?
                || line_exists_in_file(&profile_path, &sps_bin_path_str)?)
        {
            path_seems_configured = true;
        }
    } else if shell_path_env.contains("fish") {
        let fish_config_dir = home_dir.join(".config/fish");
        if !fish_config_dir.exists() {
            if let Err(e) = fs::create_dir_all(&fish_config_dir) {
                warn!(
                    "Could not create Fish config directory {}: {}",
                    fish_config_dir.display(),
                    e
                );
            }
        }
        if fish_config_dir.exists() {
            let fish_config_path = fish_config_dir.join("config.fish");
            if update_shell_config(
                &fish_config_path,
                &sps_path_line_fish,
                &sps_bin_path_str,
                "Fish",
                true,
            )? {
                config_files_updated.push(fish_config_path.display().to_string());
            } else if line_exists_in_file(&fish_config_path, &sps_bin_path_str)? {
                path_seems_configured = true;
            }
        }
    } else {
        warn!(
            "Unsupported shell for automatic PATH configuration: {}. Please add {} to your PATH manually.",
            shell_path_env, sps_bin_path_str
        );
        print_manual_path_instructions(&sps_bin_path_str);
        return Ok(());
    }

    if !config_files_updated.is_empty() {
        println!(
            "{} {} has been added to your PATH by modifying: {}",
            "sps".cyan(),
            sps_bin_path_str.cyan(),
            config_files_updated.join(", ").white()
        );
        println!(
            "{}",
            "Please open a new terminal session or source your shell configuration file for the changes to take effect."
                .yellow()
        );
        if shell_path_env.contains("zsh") {
            println!("  Run: {}", "source ~/.zshrc".green());
        }
        if shell_path_env.contains("bash") {
            println!(
                "  Run: {} (or {} or {})",
                "source ~/.bashrc".green(),
                "source ~/.bash_profile".green(),
                "source ~/.profile".green()
            );
        }
        if shell_path_env.contains("fish") {
            println!(
                "  Run (usually not needed for fish_add_path, but won't hurt): {}",
                "source ~/.config/fish/config.fish".green()
            );
        }
    } else if path_seems_configured {
        info!("sps path ({}) is likely already configured for your shell ({}). No configuration files were modified.", sps_bin_path_str.cyan(), shell_path_env.yellow());
    } else if !shell_path_env.is_empty() && shell_path_env != "unknown" {
        warn!("Could not automatically update PATH for your shell: {}. Please add {} to your PATH manually.", shell_path_env.yellow(), sps_bin_path_str.cyan());
        print_manual_path_instructions(&sps_bin_path_str);
    }
    Ok(())
}

// print_manual_path_instructions helper (no change from your existing logic)
fn print_manual_path_instructions(sps_bin_path_str: &str) {
    println!("\n{} To use sps commands and installed packages directly, please add the following line to your shell configuration file:", "Action Required:".yellow().bold());
    println!("  (e.g., ~/.zshrc, ~/.bashrc, ~/.config/fish/config.fish)");
    println!("\n  For Zsh or Bash:");
    println!(
        "    {}",
        format!("export PATH=\"{sps_bin_path_str}:$PATH\"").green()
    );
    println!("\n  For Fish shell:");
    println!(
        "    {}",
        format!("fish_add_path -P \"{sps_bin_path_str}\"").green()
    );
    println!(
        "\nThen, open a new terminal or run: {}",
        "source <your_shell_config_file>".green()
    );
}

// line_exists_in_file helper (no change from your existing logic)
fn line_exists_in_file(file_path: &Path, sps_bin_path_str: &str) -> SpsResult<bool> {
    if !file_path.exists() {
        return Ok(false);
    }
    let file = File::open(file_path).map_err(|e| SpsError::Io(Arc::new(e)))?;
    let reader = BufReader::new(file);
    let escaped_sps_bin_path = regex::escape(sps_bin_path_str);
    // Regex to find lines that configure PATH, trying to be robust for different shells
    // It looks for lines that set PATH or fish_user_paths and include the sps_bin_path_str
    // while trying to avoid commented out lines.
    let pattern = format!(
        r#"(?m)^\s*[^#]*\b(?:PATH\s*=|export\s+PATH\s*=|set\s*(?:-gx\s*|-U\s*)?\s*fish_user_paths\b|fish_add_path\s*(?:-P\s*|-p\s*)?)?["']?.*{escaped_sps_bin_path}.*["']?"#
    );
    let search_pattern_regex = regex::Regex::new(&pattern)
        .map_err(|e| SpsError::Generic(format!("Failed to compile regex for PATH check: {e}")))?;

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| SpsError::Io(Arc::new(e)))?;
        if search_pattern_regex.is_match(&line) {
            debug!(
                "Found sps PATH ({}) in {}: {}",
                sps_bin_path_str,
                file_path.display(),
                line.trim()
            );
            return Ok(true);
        }
    }
    Ok(false)
}

// update_shell_config helper (no change from your existing logic)
fn update_shell_config(
    config_path: &PathBuf,
    line_to_add: &str,
    sps_bin_path_str: &str,
    shell_name_for_log: &str,
    is_fish_shell: bool,
) -> SpsResult<bool> {
    let sps_comment_tag_start = "# SPS Path Management Start";
    let sps_comment_tag_end = "# SPS Path Management End";

    if config_path.exists() {
        match line_exists_in_file(config_path, sps_bin_path_str) {
            Ok(true) => {
                debug!(
                    "sps path ({}) already configured or managed in {} ({}). Skipping modification.",
                    sps_bin_path_str,
                    config_path.display(),
                    shell_name_for_log
                );
                return Ok(false); // Path already seems configured
            }
            Ok(false) => { /* Proceed to add */ }
            Err(e) => {
                warn!(
                    "Could not reliably check existing configuration in {} ({}): {}. Attempting to add PATH.",
                    config_path.display(),
                    shell_name_for_log,
                    e
                );
                // Proceed with caution, might add duplicate if check failed but line exists
            }
        }
    }

    debug!(
        "Adding sps PATH to {} ({})",
        config_path.display(),
        shell_name_for_log
    );

    // Ensure parent directory exists
    if let Some(parent_dir) = config_path.parent() {
        if !parent_dir.exists() {
            fs::create_dir_all(parent_dir).map_err(|e| {
                SpsError::Io(Arc::new(std::io::Error::new(
                    e.kind(),
                    format!(
                        "Failed to create parent directory {}: {}",
                        parent_dir.display(),
                        e
                    ),
                )))
            })?;
        }
    }

    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(config_path)
        .map_err(|e| {
            let msg = format!(
                "Could not open/create shell config {} ({}): {}. Please add {} to your PATH manually.",
                config_path.display(), shell_name_for_log, e, sps_bin_path_str
            );
            error!("{}", msg);
            SpsError::Io(Arc::new(std::io::Error::new(e.kind(), msg)))
        })?;

    // Construct the block to add, ensuring it's idempotent for fish
    let block_to_add = if is_fish_shell {
        format!(
            "\n{sps_comment_tag_start}\n# Add sps to PATH if not already present\nif not contains \"{sps_bin_path_str}\" $fish_user_paths\n    {line_to_add}\nend\n{sps_comment_tag_end}\n"
        )
    } else {
        format!("\n{sps_comment_tag_start}\n{line_to_add}\n{sps_comment_tag_end}\n")
    };

    if let Err(e) = writeln!(file, "{block_to_add}") {
        warn!(
            "Failed to write to shell config {} ({}): {}",
            config_path.display(),
            shell_name_for_log,
            e
        );
        Ok(false) // Indicate that update was not successful
    } else {
        info!(
            "Successfully updated {} ({}) with sps PATH.",
            config_path.display(),
            shell_name_for_log
        );
        Ok(true) // Indicate successful update
    }
}

// ensure_profile_sources_rc helper (no change from your existing logic)
fn ensure_profile_sources_rc(profile_path: &PathBuf, rc_path: &Path, shell_name_for_log: &str) {
    let rc_path_str = rc_path.to_string_lossy();
    // Regex to check if the profile file already sources the rc file.
    // Looks for lines like:
    // . /path/to/.bashrc
    // source /path/to/.bashrc
    // [ -f /path/to/.bashrc ] && . /path/to/.bashrc (and similar variants)
    let source_check_pattern = format!(
        r#"(?m)^\s*[^#]*(\.|source|\bsource\b)\s+["']?{}["']?"#, /* More general source command
                                                                  * matching */
        regex::escape(&rc_path_str)
    );
    let source_check_regex = match regex::Regex::new(&source_check_pattern) {
        Ok(re) => re,
        Err(e) => {
            warn!("Failed to compile regex for sourcing check: {}. Skipping ensure_profile_sources_rc for {}", e, profile_path.display());
            return;
        }
    };

    if profile_path.exists() {
        match fs::read_to_string(profile_path) {
            Ok(existing_content) => {
                if source_check_regex.is_match(&existing_content) {
                    debug!(
                        "{} ({}) already sources {}.",
                        profile_path.display(),
                        shell_name_for_log,
                        rc_path.display()
                    );
                    return; // Already configured
                }
            }
            Err(e) => {
                warn!(
                    "Could not read {} to check if it sources {}: {}. Will attempt to append.",
                    profile_path.display(),
                    rc_path.display(),
                    e
                );
            }
        }
    }

    // Block to add to .bash_profile or .profile to source .bashrc
    let source_block_to_add = format!(
        "\n# Source {rc_filename} if it exists and is readable\nif [ -f \"{rc_path_str}\" ] && [ -r \"{rc_path_str}\" ]; then\n    . \"{rc_path_str}\"\nfi\n",
        rc_filename = rc_path.file_name().unwrap_or_default().to_string_lossy(),
        rc_path_str = rc_path_str
    );

    info!(
        "Attempting to ensure {} ({}) sources {}",
        profile_path.display(),
        shell_name_for_log,
        rc_path.display()
    );

    if let Some(parent_dir) = profile_path.parent() {
        if !parent_dir.exists() {
            if let Err(e) = fs::create_dir_all(parent_dir) {
                warn!(
                    "Failed to create parent directory for {}: {}",
                    profile_path.display(),
                    e
                );
                return; // Cannot proceed if parent dir creation fails
            }
        }
    }

    match OpenOptions::new()
        .append(true)
        .create(true)
        .open(profile_path)
    {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{source_block_to_add}") {
                warn!(
                    "Failed to write to {} ({}): {}",
                    profile_path.display(),
                    shell_name_for_log,
                    e
                );
            } else {
                info!(
                    "Updated {} ({}) to source {}.",
                    profile_path.display(),
                    shell_name_for_log,
                    rc_path.display()
                );
            }
        }
        Err(e) => {
            warn!(
                "Could not open or create {} ({}) for updating: {}",
                profile_path.display(),
                shell_name_for_log,
                e
            );
        }
    }
}
