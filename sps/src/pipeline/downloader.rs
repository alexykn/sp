// sps/src/pipeline/downloader.rs
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use reqwest::Client as HttpClient;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::{Result as SpsResult, SpsError};
use sps_common::model::InstallTargetIdentifier;
use sps_common::pipeline::{DownloadOutcome, PipelineEvent, PlannedJob}; // MODIFIED: Removed WorkerJob, Added DownloadOutcome
use sps_core::{build, install};
use sps_net::UrlField;
use tokio::sync::{broadcast, mpsc}; // MODIFIED: Added mpsc
use tokio::task::JoinSet;
use tracing::{debug, error, warn}; // Added info

// Import get_panic_message from the runner module (sibling)
use super::runner::get_panic_message;

pub(crate) struct DownloadCoordinator<'a> {
    config: &'a Config,
    cache: Arc<Cache>,
    http_client: Arc<HttpClient>,
    event_tx: broadcast::Sender<PipelineEvent>, // This is a clone of runner_event_tx
}

impl<'a> DownloadCoordinator<'a> {
    pub fn new(
        config: &'a Config,
        cache: Arc<Cache>,
        http_client: Arc<HttpClient>,
        event_tx: broadcast::Sender<PipelineEvent>,
    ) -> Self {
        Self {
            config,
            cache,
            http_client,
            event_tx,
        }
    }

    pub async fn coordinate_downloads(
        self, // Takes ownership
        planned_jobs: Vec<PlannedJob>,
        download_outcome_tx: mpsc::Sender<DownloadOutcome>, // MODIFIED: Changed argument
    ) -> Result<(), SpsError> { // MODIFIED: Changed return type
        let mut download_tasks = JoinSet::new();
        // download_errors removed

        for planned_job in planned_jobs {
            let job_id_for_event = planned_job.target_id.clone(); // Used for events

            if let Some(private_path) = planned_job.use_private_store_source.clone() {
                debug!(
                    "[Downloader] Using app from private store for {}: {}",
                    job_id_for_event,
                    private_path.display()
                );
                // Emit events for consistency
                self.event_tx
                    .send(PipelineEvent::DownloadStarted {
                        target_id: job_id_for_event.clone(),
                        url: format!("local:{}", private_path.display()),
                    })
                    .ok();

                let size_bytes = fs::metadata(&private_path).map(|m| m.len()).unwrap_or(0);
                self.event_tx
                    .send(PipelineEvent::DownloadFinished {
                        target_id: job_id_for_event.clone(),
                        path: private_path.clone(),
                        size_bytes,
                    })
                    .ok();

                let outcome = DownloadOutcome {
                    planned_job, // Move planned_job
                    result: Ok(private_path),
                };
                if download_outcome_tx.send(outcome).await.is_err() {
                    error!(
                        "[Downloader] Failed to send DownloadOutcome for private store job {}: receiver dropped.",
                        job_id_for_event
                    );
                    // This specific job's outcome failed to send.
                    // The coordinator continues to spawn other tasks.
                }
                continue;
            }

            // Regular download path
            let task_config = self.config.clone();
            let task_cache = Arc::clone(&self.cache);
            let task_http_client = Arc::clone(&self.http_client);
            let task_event_tx = self.event_tx.clone(); // For PipelineEvents
            let task_outcome_tx = download_outcome_tx.clone(); // For DownloadOutcome
            let current_planned_job = planned_job.clone(); // Clone for the task

            download_tasks.spawn(async move {
                let job_id_in_task = current_planned_job.target_id.clone();

                let display_url_for_event = match &current_planned_job.target_definition {
                    InstallTargetIdentifier::Formula(f) => {
                        if !current_planned_job.is_source_build {
                            sps_core::install::bottle::exec::get_bottle_for_platform(f)
                                .map_or_else(|_| f.url.clone(), |(_, spec)| spec.url.clone())
                        } else {
                            f.url.clone()
                        }
                    }
                    InstallTargetIdentifier::Cask(c) => match c.url.as_ref() {
                        Some(UrlField::Simple(s)) => s.clone(),
                        Some(UrlField::WithSpec { url, .. }) => url.clone(),
                        None => "N/A (No Cask URL)".to_string(),
                    },
                };

                let outcome: DownloadOutcome; // Declare outcome to be sent

                if display_url_for_event == "N/A (No Cask URL)"
                    || (display_url_for_event.is_empty() && current_planned_job.is_source_build)
                {
                    let err_msg = "Download URL is missing or invalid".to_string();
                    let sps_error = SpsError::Generic(format!(
                        "Download URL is missing or invalid for job {job_id_in_task}: {err_msg}"
                    ));
                    task_event_tx // Still send the event
                        .send(PipelineEvent::download_failed(
                            job_id_in_task.clone(),
                            display_url_for_event.clone(), // or a placeholder
                            sps_error.clone(),
                        ))
                        .ok();
                    outcome = DownloadOutcome {
                        planned_job: current_planned_job,
                        result: Err(sps_error),
                    };
                } else {
                    task_event_tx
                        .send(PipelineEvent::DownloadStarted {
                            target_id: job_id_in_task.clone(),
                            url: display_url_for_event.clone(),
                        })
                        .ok();

                    let download_result: SpsResult<PathBuf> =
                        match &current_planned_job.target_definition {
                            InstallTargetIdentifier::Formula(f) => {
                                if current_planned_job.is_source_build {
                                    build::compile::download_source(f, &task_config).await
                                } else {
                                    install::bottle::exec::download_bottle(
                                        f,
                                        &task_config,
                                        &task_http_client,
                                    )
                                    .await
                                }
                            }
                            InstallTargetIdentifier::Cask(c) => {
                                install::cask::download_cask(c, task_cache.as_ref()).await
                            }
                        };

                    match download_result {
                        Ok(download_path) => {
                            let size_bytes =
                                fs::metadata(&download_path).map(|m| m.len()).unwrap_or(0);
                            task_event_tx
                                .send(PipelineEvent::DownloadFinished {
                                    target_id: job_id_in_task.clone(),
                                    path: download_path.clone(),
                                    size_bytes,
                                })
                                .ok();
                            outcome = DownloadOutcome {
                                planned_job: current_planned_job,
                                result: Ok(download_path),
                            };
                        }
                        Err(e) => {
                            warn!(
                                "[Downloader:{}] Download failed for {} from {}: {}",
                                job_id_in_task, // Context
                                job_id_in_task, // Subject of the log
                                display_url_for_event,
                                e
                            );
                            task_event_tx
                                .send(PipelineEvent::download_failed(
                                    job_id_in_task.clone(),
                                    display_url_for_event,
                                    e.clone(),
                                ))
                                .ok();
                            outcome = DownloadOutcome {
                                planned_job: current_planned_job,
                                result: Err(e),
                            };
                        }
                    }
                }
                // Send the outcome
                if task_outcome_tx.send(outcome).await.is_err() {
                    error!(
                        "[Downloader] Failed to send DownloadOutcome for job {}: receiver dropped.",
                        job_id_in_task
                    );
                }
                // Task returns nothing. Errors/results are sent via channel.
            });
        }

        // Wait for all download tasks to complete.
        // Panics are logged; individual errors/successes are sent via download_outcome_tx by each task.
        while let Some(result) = download_tasks.join_next().await {
            if let Err(join_error) = result {
                let panic_message = get_panic_message(join_error.into_panic());
                error!("[Downloader] Download task panicked: {}", panic_message);
                // The coordinator itself doesn't fail due to a task panic, it just logs it.
                // The absence of a DownloadOutcome for a panicked task would be the indicator upstream.
            }
        }
        // self.event_tx (the coordinator's original or cloned sender) is dropped when 'self'
        // (DownloadCoordinator) goes out of scope at the end of this method because it's taken by value.
        Ok(()) // Coordinator successfully spawned and managed tasks.
    }
}
