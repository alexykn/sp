// sps/src/cli/status.rs
use colored::*;
use sps_common::config::Config;
use sps_common::pipeline::{PipelineEvent, PipelinePackageType};
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobStatus {
    Queued,
    Downloading,
    Downloaded,
    Processing,
    Installing,
    Linking,
    Success,
    Failed,
}

struct JobInfo {
    name: String,
    status: JobStatus,
    message: String,
}

pub async fn handle_events(
    _config: Config,
    mut event_rx: broadcast::Receiver<PipelineEvent>,
) {
    let mut jobs: HashMap<String, JobInfo> = HashMap::new();
    let mut total_jobs: usize = 0;
    let mut completed_jobs: usize = 0;
    let mut failed_jobs: usize = 0;
    let start_time = Instant::now();

    println!("{}", "Starting pipeline...".cyan().bold());

    loop {
        match event_rx.recv().await {
            Ok(event) => {
                match event {
                    PipelineEvent::PipelineStarted { total_jobs: t } => {
                        total_jobs = t;
                        println!(
                            "{} {}",
                            "Planned jobs:".bold(),
                            t
                        );
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
                        println!(
                            "{} {}",
                            "Planning finished. Jobs:".bold(),
                            job_count
                        );
                    }
                    PipelineEvent::DownloadStarted { target_id, url } => {
                        println!(
                            "{} {} ({})",
                            "Downloading:".yellow(),
                            target_id.cyan(),
                            url
                        );
                        jobs.entry(target_id.clone()).or_insert(JobInfo {
                            name: target_id,
                            status: JobStatus::Downloading,
                            message: "Downloading".to_string(),
                        });
                    }
                    PipelineEvent::DownloadFinished { target_id, path, size_bytes } => {
                        println!(
                            "{} {} ({} bytes) -> {}",
                            "Downloaded:".green(),
                            target_id.cyan(),
                            size_bytes,
                            path.display()
                        );
                        jobs.entry(target_id.clone()).and_modify(|j| {
                            j.status = JobStatus::Downloaded;
                            j.message = "Downloaded".to_string();
                        });
                    }
                    PipelineEvent::DownloadFailed { target_id, error, .. } => {
                        println!(
                            "{} {}: {}",
                            "Download failed:".red(),
                            target_id.cyan(),
                            error.red()
                        );
                        jobs.entry(target_id.clone()).and_modify(|j| {
                            j.status = JobStatus::Failed;
                            j.message = format!("Download failed: {}", error);
                        });
                        failed_jobs += 1;
                    }
                    PipelineEvent::JobProcessingStarted { target_id } => {
                        println!(
                            "{} {}",
                            "Processing:".yellow(),
                            target_id.cyan()
                        );
                        jobs.entry(target_id.clone()).or_insert(JobInfo {
                            name: target_id,
                            status: JobStatus::Processing,
                            message: "Processing".to_string(),
                        });
                    }
                    PipelineEvent::BuildStarted { target_id } => {
                        println!(
                            "{} {}",
                            "Building from source:".yellow(),
                            target_id.cyan()
                        );
                    }
                    PipelineEvent::InstallStarted { target_id, pkg_type } => {
                        let type_str = match pkg_type {
                            PipelinePackageType::Formula => "Formula",
                            PipelinePackageType::Cask => "Cask",
                        };
                        println!(
                            "{} {} ({})",
                            "Installing:".yellow(),
                            target_id.cyan(),
                            type_str
                        );
                        jobs.entry(target_id.clone()).and_modify(|j| {
                            j.status = JobStatus::Installing;
                            j.message = "Installing".to_string();
                        });
                    }
                    PipelineEvent::LinkStarted { target_id, .. } => {
                        println!(
                            "{} {}",
                            "Linking:".yellow(),
                            target_id.cyan()
                        );
                        jobs.entry(target_id.clone()).and_modify(|j| {
                            j.status = JobStatus::Linking;
                            j.message = "Linking".to_string();
                        });
                    }
                    PipelineEvent::JobSuccess { target_id, action, pkg_type } => {
                        let type_str = match pkg_type {
                            PipelinePackageType::Formula => "Formula",
                            PipelinePackageType::Cask => "Cask",
                        };
                        let action_str = match action {
                            sps_common::pipeline::JobAction::Install => "Installed",
                            sps_common::pipeline::JobAction::Upgrade { .. } => "Upgraded",
                            sps_common::pipeline::JobAction::Reinstall { .. } => "Reinstalled",
                        };
                        println!(
                            "{} {} ({}) {}",
                            "✓".green().bold(),
                            target_id.cyan(),
                            type_str,
                            action_str.green()
                        );
                        jobs.entry(target_id.clone()).and_modify(|j| {
                            j.status = JobStatus::Success;
                            j.message = "Success".to_string();
                        });
                        completed_jobs += 1;
                    }
                    PipelineEvent::JobFailed { target_id, error, .. } => {
                        println!(
                            "{} {}: {}",
                            "✗".red().bold(),
                            target_id.cyan(),
                            error.red()
                        );
                        jobs.entry(target_id.clone()).and_modify(|j| {
                            j.status = JobStatus::Failed;
                            j.message = format!("Failed: {}", error);
                        });
                        failed_jobs += 1;
                    }
                    PipelineEvent::LogInfo { message } => {
                        println!("{}", message);
                    }
                    PipelineEvent::LogWarn { message } => {
                        println!("{}", message.yellow());
                    }
                    PipelineEvent::LogError { message } => {
                        println!("{}", message.red());
                    }
                    PipelineEvent::PipelineFinished { duration_secs, success_count, fail_count } => {
                        println!(
                            "{} in {:.2}s ({} succeeded, {} failed)",
                            "Pipeline finished".bold(),
                            duration_secs,
                            success_count,
                            fail_count
                        );
                    }
                    _ => {}
                }
            }
            Err(broadcast::error::RecvError::Closed) => {
                break;
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // Ignore lag for now
            }
        }
    }

    let elapsed = start_time.elapsed().as_secs_f64();
    println!(
        "\n{}: {}  {}: {}  {}: {}  {}: {:.2}s",
        "Total jobs".bold(),
        total_jobs,
        "Completed".green().bold(),
        completed_jobs,
        "Failed".red().bold(),
        failed_jobs,
        "Elapsed".bold(),
        elapsed
    );
}
