// sps/src/cli/status.rs
use std::collections::HashMap;
use std::io::{self, Write};
use std::time::Instant;

use colored::*;
use sps_common::config::Config;
use sps_common::pipeline::{PipelineEvent, PipelinePackageType};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobStatus {
    Waiting,
    Downloading,
    Downloaded,
    Processing,
    Installing,
    Linking,
    Success,
    Failed,
}

impl JobStatus {
    fn display_state(&self) -> &'static str {
        match self {
            JobStatus::Waiting => "waiting",
            JobStatus::Downloading => "downloading",
            JobStatus::Downloaded => "downloaded",
            JobStatus::Processing => "processing",
            JobStatus::Installing => "installing",
            JobStatus::Linking => "linking",
            JobStatus::Success => "success",
            JobStatus::Failed => "failed",
        }
    }

    fn slot_indicator(&self) -> String {
        match self {
            JobStatus::Waiting => " ·".dimmed().to_string(),
            JobStatus::Downloading => " ↓".yellow().to_string(),
            JobStatus::Downloaded => " ✓".green().to_string(),
            JobStatus::Processing => " ⚙".blue().to_string(),
            JobStatus::Installing => " ⚙".magenta().to_string(),
            JobStatus::Linking => " →".cyan().to_string(),
            JobStatus::Success => " ✓".green().bold().to_string(),
            JobStatus::Failed => " ✗".red().bold().to_string(),
        }
    }

    fn colored_state(&self) -> ColoredString {
        match self {
            JobStatus::Waiting => self.display_state().dimmed(),
            JobStatus::Downloading => self.display_state().yellow(),
            JobStatus::Downloaded => self.display_state().green(),
            JobStatus::Processing => self.display_state().blue(),
            JobStatus::Installing => self.display_state().magenta(),
            JobStatus::Linking => self.display_state().cyan(),
            JobStatus::Success => self.display_state().green().bold(),
            JobStatus::Failed => self.display_state().red().bold(),
        }
    }
}

struct JobInfo {
    name: String,
    status: JobStatus,
    size_bytes: Option<u64>,
    start_time: Option<Instant>,
    pool_id: usize,
}

impl JobInfo {
    fn _elapsed_str(&self) -> String {
        match self.start_time {
            Some(start) => format!("{:.1}s", start.elapsed().as_secs_f64()),
            None => "–".to_string(),
        }
    }

    fn size_str(&self) -> String {
        match self.size_bytes {
            Some(bytes) => format_bytes(bytes),
            None => "–".to_string(),
        }
    }
}

struct StatusDisplay {
    jobs: HashMap<String, JobInfo>,
    job_order: Vec<String>,
    total_jobs: usize,
    next_pool_id: usize,
    start_time: Instant,
    active_downloads: usize,
    total_bytes: u64,
    downloaded_bytes: u64,
    last_speed_update: Instant,
    last_downloaded_bytes: u64,
    current_speed_bps: f64,
    header_printed: bool,
    last_line_count: usize,
}

impl StatusDisplay {
    fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            job_order: Vec::new(),
            total_jobs: 0,
            next_pool_id: 1,
            start_time: Instant::now(),
            active_downloads: 0,
            total_bytes: 0,
            downloaded_bytes: 0,
            last_speed_update: Instant::now(),
            last_downloaded_bytes: 0,
            current_speed_bps: 0.0,
            header_printed: false,
            last_line_count: 0,
        }
    }

    fn add_job(&mut self, target_id: String, status: JobStatus, size_bytes: Option<u64>) {
        if !self.jobs.contains_key(&target_id) {
            let job_info = JobInfo {
                name: target_id.clone(),
                status,
                size_bytes,
                start_time: if status != JobStatus::Waiting {
                    Some(Instant::now())
                } else {
                    None
                },
                pool_id: self.next_pool_id,
            };

            if let Some(bytes) = size_bytes {
                self.total_bytes += bytes;
            }

            self.jobs.insert(target_id.clone(), job_info);
            self.job_order.push(target_id);
            self.next_pool_id += 1;
        }
    }

    fn update_job_status(&mut self, target_id: &str, status: JobStatus, size_bytes: Option<u64>) {
        if let Some(job) = self.jobs.get_mut(target_id) {
            let was_downloading = job.status == JobStatus::Downloading;
            let is_downloading = status == JobStatus::Downloading;

            job.status = status;

            if job.start_time.is_none() && status != JobStatus::Waiting {
                job.start_time = Some(Instant::now());
            }

            if let Some(bytes) = size_bytes {
                if job.size_bytes.is_none() {
                    self.total_bytes += bytes;
                }
                job.size_bytes = Some(bytes);
            }

            // Update download counts
            if was_downloading && !is_downloading {
                self.active_downloads = self.active_downloads.saturating_sub(1);
                if let Some(bytes) = job.size_bytes {
                    self.downloaded_bytes += bytes;
                }
            } else if !was_downloading && is_downloading {
                self.active_downloads += 1;
            }
        }
    }

    fn update_speed(&mut self) {
        let now = Instant::now();
        let time_diff = now.duration_since(self.last_speed_update).as_secs_f64();

        if time_diff >= 0.5 {
            // Update speed every 0.5 seconds
            let bytes_diff = self
                .downloaded_bytes
                .saturating_sub(self.last_downloaded_bytes);
            self.current_speed_bps = bytes_diff as f64 / time_diff;
            self.last_speed_update = now;
            self.last_downloaded_bytes = self.downloaded_bytes;
        }
    }

    fn render(&mut self) {
        self.update_speed();

        if !self.header_printed {
            // First render - print header and jobs
            self.print_header();
            let job_output = self.build_job_rows();
            print!("{job_output}");
            self.header_printed = true;
            // Count lines: header + jobs + separator + summary
            let job_lines = job_output.lines().count();
            self.last_line_count = 1 + job_lines + 1 + 1;
        } else {
            // Subsequent renders - only clear and reprint job rows and summary
            self.clear_previous_output();
            self.print_header();
            let job_output = self.build_job_rows();
            print!("{job_output}");
            // Update line count
            let job_lines = job_output.lines().count();
            self.last_line_count = 1 + job_lines + 1 + 1;
        }

        // Print separator
        println!("{}", "─".repeat(49).dimmed());

        // Print status summary
        let completed = self
            .jobs
            .values()
            .filter(|j| matches!(j.status, JobStatus::Success))
            .count();
        let failed = self
            .jobs
            .values()
            .filter(|j| matches!(j.status, JobStatus::Failed))
            .count();
        let progress_chars = self.generate_progress_bar(completed, failed);
        let speed_str = format_speed(self.current_speed_bps);

        println!("{} net {}", progress_chars, speed_str.blue());

        io::stdout().flush().unwrap();
    }

    fn print_header(&self) {
        println!(
            "{:<6} {:<12} {:<15} {:>8} {}",
            "IID".bold().dimmed(),
            "STATE".bold().dimmed(),
            "PKG".bold().dimmed(),
            "SIZE".bold().dimmed(),
            "SLOT".bold().dimmed()
        );
    }

    fn build_job_rows(&self) -> String {
        let mut output = String::new();

        // Job rows
        for target_id in &self.job_order {
            if let Some(job) = self.jobs.get(target_id) {
                output.push_str(&format!(
                    "{:<6} {:<12} {:<15} {:>8} {}\n",
                    format!("#{:02}", job.pool_id).cyan(),
                    job.status.colored_state(),
                    job.name.cyan(),
                    job.size_str(),
                    job.status.slot_indicator()
                ));
            }
        }

        output
    }

    fn clear_previous_output(&self) {
        // Move cursor up and clear lines
        for _ in 0..self.last_line_count {
            print!("\x1b[1A\x1b[2K"); // Move up one line and clear it
        }
        io::stdout().flush().unwrap();
    }

    fn generate_progress_bar(&self, completed: usize, failed: usize) -> String {
        if self.total_jobs == 0 {
            return "".to_string();
        }

        let total_done = completed + failed;
        let progress_width = 8;
        let filled = (total_done * progress_width) / self.total_jobs;
        let remaining = progress_width - filled;

        let filled_str = "▍".repeat(filled).green();
        let remaining_str = "·".repeat(remaining).dimmed();

        format!("{filled_str}{remaining_str}")
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "kB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit_idx = 0;

    while value >= 1000.0 && unit_idx < UNITS.len() - 1 {
        value /= 1000.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{bytes}B")
    } else {
        format!("{:.1}{}", value, UNITS[unit_idx])
    }
}

fn format_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 1.0 {
        return "0 B/s".to_string();
    }

    const UNITS: &[&str] = &["B/s", "kB/s", "MB/s", "GB/s"];
    let mut value = bytes_per_sec;
    let mut unit_idx = 0;

    while value >= 1000.0 && unit_idx < UNITS.len() - 1 {
        value /= 1000.0;
        unit_idx += 1;
    }

    format!("{:.1} {}", value, UNITS[unit_idx])
}

pub async fn handle_events(_config: Config, mut event_rx: broadcast::Receiver<PipelineEvent>) {
    let mut display = StatusDisplay::new();
    let mut logs_buffer = Vec::new();
    let mut pipeline_active = false;

    loop {
        match event_rx.recv().await {
            Ok(event) => match event {
                PipelineEvent::PipelineStarted { total_jobs } => {
                    pipeline_active = true;
                    display.total_jobs = total_jobs;
                    println!("{}", "Starting pipeline...".cyan().bold());
                }
                PipelineEvent::PlanningStarted => {
                    println!("{}", "Planning operations...".cyan());
                }
                PipelineEvent::DependencyResolutionStarted => {
                    println!("{}", "Resolving dependencies...".cyan());
                }
                PipelineEvent::DependencyResolutionFinished => {
                    println!("{}", "Dependency resolution complete.".cyan());
                }
                PipelineEvent::PlanningFinished { job_count } => {
                    println!("{} {}", "Planning finished. Jobs:".bold(), job_count);
                    println!(); // Add blank line before table
                }
                PipelineEvent::DownloadStarted { target_id, url: _ } => {
                    display.add_job(target_id.clone(), JobStatus::Downloading, None);
                    if pipeline_active {
                        display.render();
                    }
                }
                PipelineEvent::DownloadFinished {
                    target_id,
                    size_bytes,
                    ..
                } => {
                    display.update_job_status(&target_id, JobStatus::Downloaded, Some(size_bytes));
                    if pipeline_active {
                        display.render();
                    }
                }
                PipelineEvent::DownloadFailed {
                    target_id, error, ..
                } => {
                    display.update_job_status(&target_id, JobStatus::Failed, None);
                    logs_buffer.push(format!(
                        "{} {}: {}",
                        "Download failed:".red(),
                        target_id.cyan(),
                        error.red()
                    ));
                    if pipeline_active {
                        display.render();
                    }
                }
                PipelineEvent::JobProcessingStarted { target_id } => {
                    display.update_job_status(&target_id, JobStatus::Processing, None);
                    if pipeline_active {
                        display.render();
                    }
                }
                PipelineEvent::BuildStarted { target_id } => {
                    display.update_job_status(&target_id, JobStatus::Processing, None);
                    if pipeline_active {
                        display.render();
                    }
                }
                PipelineEvent::InstallStarted { target_id, .. } => {
                    display.update_job_status(&target_id, JobStatus::Installing, None);
                    if pipeline_active {
                        display.render();
                    }
                }
                PipelineEvent::LinkStarted { target_id, .. } => {
                    display.update_job_status(&target_id, JobStatus::Linking, None);
                    if pipeline_active {
                        display.render();
                    }
                }
                PipelineEvent::JobSuccess {
                    target_id,
                    action,
                    pkg_type,
                } => {
                    display.update_job_status(&target_id, JobStatus::Success, None);
                    let type_str = match pkg_type {
                        PipelinePackageType::Formula => "Formula",
                        PipelinePackageType::Cask => "Cask",
                    };
                    let action_str = match action {
                        sps_common::pipeline::JobAction::Install => "Installed",
                        sps_common::pipeline::JobAction::Upgrade { .. } => "Upgraded",
                        sps_common::pipeline::JobAction::Reinstall { .. } => "Reinstalled",
                    };
                    logs_buffer.push(format!(
                        "{}: {} ({})",
                        action_str.green(),
                        target_id.cyan(),
                        type_str,
                    ));
                    if pipeline_active {
                        display.render();
                    }
                }
                PipelineEvent::JobFailed {
                    target_id, error, ..
                } => {
                    display.update_job_status(&target_id, JobStatus::Failed, None);
                    logs_buffer.push(format!(
                        "{} {}: {}",
                        "✗".red().bold(),
                        target_id.cyan(),
                        error.red()
                    ));
                    if pipeline_active {
                        display.render();
                    }
                }
                PipelineEvent::LogInfo { message } => {
                    logs_buffer.push(message);
                }
                PipelineEvent::LogWarn { message } => {
                    logs_buffer.push(message.yellow().to_string());
                }
                PipelineEvent::LogError { message } => {
                    logs_buffer.push(message.red().to_string());
                }
                PipelineEvent::PipelineFinished {
                    duration_secs,
                    success_count,
                    fail_count,
                } => {
                    // Final render to show completed state
                    if display.header_printed {
                        display.render();
                    }

                    println!(); // Add blank line after final table

                    // Print summary
                    println!(
                        "{} in {:.2}s ({} succeeded, {} failed)",
                        "Pipeline finished".bold(),
                        duration_secs,
                        success_count,
                        fail_count
                    );

                    // Print any buffered logs
                    if !logs_buffer.is_empty() {
                        println!(); // Blank line before logs
                        for log in &logs_buffer {
                            println!("{log}");
                        }
                    }

                    let elapsed = display.start_time.elapsed().as_secs_f64();
                    println!(
                        "\n{}: {}  {}: {}  {}: {}  {}: {:.2}s",
                        "Total jobs".bold(),
                        display.total_jobs,
                        "Completed".green().bold(),
                        success_count,
                        "Failed".red().bold(),
                        fail_count,
                        "Elapsed".bold(),
                        elapsed
                    );

                    break;
                }
                _ => {}
            },
            Err(broadcast::error::RecvError::Closed) => {
                break;
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // Ignore lag for now
            }
        }
    }
}
