// sps/src/pipeline/downloader.rs
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use reqwest::Client as HttpClient;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::model::InstallTargetIdentifier;
use sps_common::pipeline::{DownloadOutcome, PipelineEvent, PlannedJob};
use sps_common::SpsError;
use sps_core::{build, install};
use sps_net::UrlField;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;
use tracing::{error, warn};

use super::runner::get_panic_message;

pub(crate) struct DownloadCoordinator {
    config: Config,
    cache: Arc<Cache>,
    http_client: Arc<HttpClient>,
    event_tx: Option<broadcast::Sender<PipelineEvent>>,
}

impl DownloadCoordinator {
    pub fn new(
        config: Config,
        cache: Arc<Cache>,
        http_client: Arc<HttpClient>,
        event_tx: broadcast::Sender<PipelineEvent>,
    ) -> Self {
        Self {
            config,
            cache,
            http_client,
            event_tx: Some(event_tx),
        }
    }

    pub async fn coordinate_downloads(
        &mut self,
        planned_jobs: Vec<PlannedJob>,
        download_outcome_tx: mpsc::Sender<DownloadOutcome>,
    ) -> Vec<(String, SpsError)> {
        let mut download_tasks = JoinSet::new();
        let mut critical_spawn_errors: Vec<(String, SpsError)> = Vec::new();

        for planned_job in planned_jobs {
            let _job_id_for_task = planned_job.target_id.clone();

            let task_config = self.config.clone();
            let task_cache = Arc::clone(&self.cache);
            let task_http_client = Arc::clone(&self.http_client);
            let task_event_tx = self.event_tx.as_ref().cloned();
            let outcome_tx_clone = download_outcome_tx.clone();
            let current_planned_job_for_task = planned_job.clone();

            download_tasks.spawn(async move {
                let job_id_in_task = current_planned_job_for_task.target_id.clone();
                let download_path_result: Result<PathBuf, SpsError>;

                if let Some(private_path) = current_planned_job_for_task.use_private_store_source.clone() {
                    download_path_result = Ok(private_path);
                } else {
                    let display_url_for_event = match &current_planned_job_for_task.target_definition {
                        InstallTargetIdentifier::Formula(f) => {
                            if !current_planned_job_for_task.is_source_build {
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
                        || (display_url_for_event.is_empty() && !current_planned_job_for_task.is_source_build)
                    {
                        let _err_msg = "Download URL is missing or invalid".to_string();
                        let sps_err = SpsError::Generic(format!(
                            "Download URL is missing or invalid for job {job_id_in_task}"
                        ));
                        if let Some(ref tx) = task_event_tx {
                            tx.send(PipelineEvent::download_failed(
                                job_id_in_task.clone(),
                                display_url_for_event,
                                &sps_err,
                            )).ok();
                        }
                        download_path_result = Err(sps_err);
                    } else {
                        if let Some(ref tx) = task_event_tx {
                            tx.send(PipelineEvent::DownloadStarted {
                                target_id: job_id_in_task.clone(),
                                url: display_url_for_event.clone(),
                            }).ok();
                        }

                        let actual_download_result: Result<PathBuf, SpsError> =
                            match &current_planned_job_for_task.target_definition {
                                InstallTargetIdentifier::Formula(f) => {
                                    if current_planned_job_for_task.is_source_build {
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

                        match actual_download_result {
                            Ok(path) => {
                                let size_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                                if let Some(ref tx) = task_event_tx {
                                    tx.send(PipelineEvent::DownloadFinished {
                                        target_id: job_id_in_task.clone(),
                                        path: path.clone(),
                                        size_bytes,
                                    }).ok();
                                }
                                download_path_result = Ok(path);
                            }
                            Err(e) => {
                                warn!(
                                    "[DownloaderTask:{}] Download failed from {}: {}",
                                    job_id_in_task, display_url_for_event, e
                                );
                                if let Some(ref tx) = task_event_tx {
                                    tx.send(PipelineEvent::download_failed(
                                        job_id_in_task.clone(),
                                        display_url_for_event,
                                        &e,
                                    )).ok();
                                }
                                download_path_result = Err(e);
                            }
                        }
                    }
                }

                let outcome = DownloadOutcome {
                    planned_job: current_planned_job_for_task,
                    result: download_path_result,
                };

                if let Err(send_err) = outcome_tx_clone.send(outcome).await {
                    error!(
                        "[DownloaderTask:{}] CRITICAL: Failed to send download outcome to runner: {}. Job processing will likely stall.",
                        job_id_in_task, send_err
                    );
                }
            });
        }

        while let Some(join_result) = download_tasks.join_next().await {
            if let Err(e) = join_result {
                let panic_msg = get_panic_message(e.into_panic());
                error!(
                    "[Downloader] A download task panicked: {}. This job's outcome was not sent.",
                    panic_msg
                );
                critical_spawn_errors.push((
                    "[UnknownDownloadTaskPanic]".to_string(),
                    SpsError::Generic(format!("A download task panicked: {panic_msg}")),
                ));
            }
        }
        self.event_tx = None;
        critical_spawn_errors
    }
}
