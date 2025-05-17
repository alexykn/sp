// sps/src/cli/init.rs
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind as IoErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::Arc; // Keep for SpsError::Io

use clap::Args;
use colored::Colorize;
use sps_common::config::Config;
use sps_common::error::{Result as SpsResult, SpsError};
use tracing::{debug, error, info, warn};
// Removed: use users::{get_current_uid, get_user_by_uid};

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

        // 1. Initial Checks (as current user)
        if sps_root.exists() {
            let is_empty = match fs::read_dir(sps_root) {
                Ok(mut entries) => entries.next().is_none(),
                Err(_) => false,
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
                    "Directory {} exists but does not appear to be an sps root (missing marker).",
                    sps_root.display()
                );
                warn!(
                    "Run with --force to initialize anyway (this might overwrite existing data)."
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
        // Get current username for chown using environment variables.
        let current_user_name = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .map_err(|_| {
                SpsError::Generic(
                    "Failed to get current username from USER or LOGNAME environment variables."
                        .to_string(),
                )
            })?;

        let target_group_name = if cfg!(target_os = "macos") {
            "admin"
        } else {
            "staff"
        };

        info!(
            "Will attempt to set ownership of {} to {}:{}",
            sps_root.display(),
            current_user_name,
            target_group_name
        );

        println!(
            "{}",
            "sps will require sudo to create directories and set permissions in /opt/sps.".yellow()
        );

        let dirs_to_create = vec![
            config.sps_root().to_path_buf(),
            config.bin_dir(),
            config.cellar_dir(),
            config.cask_room_dir(),
            config.cask_store_dir(),
            config.opt_dir(),
            config.taps_dir(),
            config.cache_dir(),
            config.logs_dir(),
            config.tmp_dir(),
            config.state_dir(),
            config
                .man_base_dir()
                .parent()
                .unwrap_or_else(|| Path::new("/opt/sps/share"))
                .to_path_buf(),
            config.man_base_dir(),
            config.sps_root().join("etc"),
            config.sps_root().join("include"),
            config.sps_root().join("lib"),
            config.sps_root().join("share/doc"),
        ];

        for dir_path in dirs_to_create {
            debug!(
                "Ensuring directory exists with sudo: {}",
                dir_path.display()
            );
            run_sudo_command("mkdir", &["-p", &dir_path.to_string_lossy()])?;
        }

        debug!("Creating marker file with sudo: {}", marker_path.display());
        let marker_content = "sps root directory version 1";
        let cmd_str = format!(
            "echo '{}' | sudo tee {}",
            marker_content,
            marker_path.display()
        );
        let status = StdCommand::new("sh")
            .arg("-c")
            .arg(&cmd_str)
            .status()
            .map_err(|e| SpsError::Io(Arc::new(e)))?;
        if !status.success() {
            return Err(SpsError::Io(Arc::new(std::io::Error::new(
                IoErrorKind::PermissionDenied,
                format!(
                    "Failed to create marker file {} with sudo: {}",
                    marker_path.display(),
                    status
                ),
            ))));
        }

        #[cfg(unix)]
        {
            info!("Setting ownership of {} using sudo...", sps_root.display());
            run_sudo_command(
                "chown",
                &[
                    "-R",
                    &format!("{current_user_name}:{target_group_name}"),
                    &sps_root.to_string_lossy(),
                ],
            )?;

            info!(
                "Setting permissions on {} using sudo...",
                sps_root.display()
            );
            run_sudo_command("chmod", &["-R", "ug=rwX,o=rX", &sps_root.to_string_lossy()])?;
            run_sudo_command("chmod", &["a+x", &config.bin_dir().to_string_lossy()])?;

            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!("Listing /opt/sps after permission changes:");
                let ls_output_root = StdCommand::new("ls").arg("-ld").arg(sps_root).output();
                if let Ok(out) = ls_output_root {
                    debug!(
                        "ls -ld /opt/sps: \nSTDOUT: {}\nSTDERR: {}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                } else if let Err(e) = ls_output_root {
                    warn!("Failed to ls /opt/sps: {}", e);
                }

                debug!("Listing /opt/sps/bin after permission changes:");
                let ls_output_bin = StdCommand::new("ls")
                    .arg("-ld")
                    .arg(config.bin_dir())
                    .output();
                if let Ok(out) = ls_output_bin {
                    debug!(
                        "ls -ld /opt/sps/bin: \nSTDOUT: {}\nSTDERR: {}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                } else if let Err(e) = ls_output_bin {
                    warn!("Failed to ls /opt/sps/bin: {}", e);
                }
            }
        }

        // 3. User-Specific PATH Configuration (runs as the original user)
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

fn configure_shell_path(config: &Config, current_user_name_for_log: &str) -> SpsResult<()> {
    info!("Attempting to configure your shell for sps PATH...");

    let sps_bin_path_str = config.bin_dir().to_string_lossy().into_owned();
    // Use config.home_dir() which relies on the 'directories' crate via sps-common
    let home_dir = config.home_dir();
    if home_dir == PathBuf::from("/") && current_user_name_for_log != "root" {
        // Basic check if home_dir is root
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
                // Corrected syntax error here
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

fn line_exists_in_file(file_path: &Path, sps_bin_path_str: &str) -> SpsResult<bool> {
    if !file_path.exists() {
        return Ok(false);
    }
    let file = File::open(file_path).map_err(|e| SpsError::Io(Arc::new(e)))?;
    let reader = BufReader::new(file);
    let escaped_sps_bin_path = regex::escape(sps_bin_path_str);
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
                return Ok(false);
            }
            Ok(false) => { /* Proceed to add */ }
            Err(e) => {
                warn!(
                    "Could not reliably check existing configuration in {} ({}): {}. Attempting to add PATH.",
                    config_path.display(),
                    shell_name_for_log,
                    e
                );
            }
        }
    }

    debug!(
        "Adding sps PATH to {} ({})",
        config_path.display(),
        shell_name_for_log
    );

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
        Ok(false)
    } else {
        info!(
            "Successfully updated {} ({}) with sps PATH.",
            config_path.display(),
            shell_name_for_log
        );
        Ok(true)
    }
}

fn ensure_profile_sources_rc(profile_path: &PathBuf, rc_path: &Path, shell_name_for_log: &str) {
    let rc_path_str = rc_path.to_string_lossy();
    let source_check_pattern = format!(
        r#"(?m)^\s*[^#]*\.\s*["']?{}["']?"#,
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
                    return;
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
                return;
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
