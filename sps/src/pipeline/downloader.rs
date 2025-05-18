// sps/src/pipeline/downloader.rs
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use reqwest::Client as HttpClient;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::{Result as SpsResult, SpsError};
use sps_common::model::InstallTargetIdentifier;
use sps_common::pipeline::{PipelineEvent, PlannedJob, WorkerJob};
use sps_core::{build, install};
use sps_net::UrlField;
use tokio::sync::broadcast;
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
        self, /* Take ownership to ensure self.event_tx is dropped when this method returns if
               * not explicitly dropped earlier */
        planned_jobs: Vec<PlannedJob>,
        worker_job_tx: crossbeam_channel::Sender<WorkerJob>,
    ) -> Vec<(String, SpsError)> {
        let mut download_tasks = JoinSet::new();
        let mut download_errors: Vec<(String, SpsError)> = Vec::new();

        for planned_job in planned_jobs {
            let job_id_for_task = planned_job.target_id.clone();

            if let Some(private_path) = planned_job.use_private_store_source.clone() {
                debug!(
                    "[Downloader] Using app from private store for WorkerJob: {}",
                    private_path.display()
                );
                let size = fs::metadata(&private_path).map(|m| m.len()).unwrap_or(0);
                let worker_job_payload = WorkerJob {
                    request: planned_job,
                    download_path: private_path.clone(),
                    download_size_bytes: size,
                    is_source_from_private_store: true,
                };
                if let Err(e) = worker_job_tx.send(worker_job_payload) {
                    error!(
                        "[Downloader] Failed to queue worker job (private store) for {}: {}",
                        job_id_for_task, e
                    );
                    download_errors.push((
                        job_id_for_task,
                        SpsError::Generic(format!(
                            "Failed to queue worker job (private store): {e}"
                        )),
                    ));
                }
                continue;
            }

            let task_config = self.config.clone();
            let task_cache = Arc::clone(&self.cache);
            let task_http_client = Arc::clone(&self.http_client);
            // Each task gets its own clone of the DownloadCoordinator's event_tx
            let task_event_tx = self.event_tx.clone();
            let current_planned_job = planned_job.clone();

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
                    InstallTargetIdentifier::Cask(c) => match &c.url {
                        Some(UrlField::Simple(s)) => s.clone(),
                        Some(UrlField::WithSpec { url, .. }) => url.clone(),
                        None => "N/A (No Cask URL)".to_string(),
                    },
                };

                if display_url_for_event == "N/A (No Cask URL)"
                    || (display_url_for_event.is_empty() && current_planned_job.is_source_build)
                {
                    let err_msg = "Download URL is missing or invalid".to_string();
                    // Use task_event_tx for sending events from within the task
                    task_event_tx
                        .send(PipelineEvent::download_failed(
                            job_id_in_task.clone(),
                            display_url_for_event,
                            SpsError::Generic(err_msg.clone()),
                        ))
                        .ok();
                    return Err((
                        job_id_in_task.clone(),
                        SpsError::Generic(format!(
                            "Download URL is missing or invalid for job {job_id_in_task}"
                        )),
                    ));
                }
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
                        let size_bytes = fs::metadata(&download_path).map(|m| m.len()).unwrap_or(0);
                        task_event_tx
                            .send(PipelineEvent::DownloadFinished {
                                target_id: job_id_in_task.clone(),
                                path: download_path.clone(),
                                size_bytes,
                            })
                            .ok();
                        Ok(WorkerJob {
                            request: current_planned_job,
                            download_path,
                            download_size_bytes: size_bytes,
                            is_source_from_private_store: false,
                        })
                    }
                    Err(e) => {
                        warn!(
                            "[Downloader:{}] Download failed from {}: {}",
                            job_id_in_task, display_url_for_event, e
                        );
                        task_event_tx
                            .send(PipelineEvent::download_failed(
                                job_id_in_task.clone(),
                                display_url_for_event,
                                e.clone(),
                            ))
                            .ok();
                        Err((job_id_in_task.clone(), e))
                    }
                }
                // task_event_tx is dropped when this async block ends
            });
        }

        while let Some(result) = download_tasks.join_next().await {
            match result {
                Ok(Ok(worker_job)) => {
                    if worker_job_tx.send(worker_job).is_err() {
                        error!("[Downloader] Worker job channel closed. Core likely shut down.");
                        break;
                    }
                }
                Ok(Err((id, e))) => {
                    download_errors.push((id, e));
                }
                Err(join_error) => {
                    let panic_message = get_panic_message(join_error.into_panic());
                    error!("[Downloader] Download task panicked: {}", panic_message);
                    download_errors.push((
                        "[DownloadTaskPanic]".to_string(),
                        SpsError::Generic(format!("Download task panicked: {panic_message}")),
                    ));
                }
            }
        }
        // Explicitly drop the DownloadCoordinator's clone of event_tx
        download_errors
    }
}
