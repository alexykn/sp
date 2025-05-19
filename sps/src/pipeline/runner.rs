// sps/src/pipeline/runner.rs
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use colored::Colorize;
use crossbeam_channel::bounded;
use reqwest::Client as HttpClient;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::{Result as SpsResult, SpsError};
use sps_common::model::{Cask, Formula, InstallTargetIdentifier};
use sps_common::pipeline::{PipelineEvent, WorkerJob};
use sps_net::api;
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tracing::{debug, error, instrument, warn}; // Added info

use self::downloader::DownloadCoordinator;
use self::planner::OperationPlanner;
use super::{downloader, planner};

const WORKER_JOB_CHANNEL_SIZE: usize = 100;
const EVENT_CHANNEL_SIZE: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandType {
    Install,
    Reinstall,
    Upgrade { all: bool },
}

#[derive(Debug, Clone)]
pub struct PipelineFlags {
    pub build_from_source: bool,
    pub include_optional: bool,
    pub skip_recommended: bool,
}

#[instrument(skip_all, fields(cmd = ?command_type, targets = ?initial_targets))]
pub async fn run_pipeline(
    initial_targets: &[String],
    command_type: CommandType,
    config: &Config,
    cache: Arc<Cache>,
    flags: &PipelineFlags,
) -> SpsResult<()> {
    let start_time = Instant::now();
    let final_success_count = Arc::new(AtomicUsize::new(0));
    let final_fail_count = Arc::new(AtomicUsize::new(0));

    let (event_tx, mut _first_event_rx) = broadcast::channel::<PipelineEvent>(EVENT_CHANNEL_SIZE);
    // Immediately drop _first_event_rx if it's not going to be used by run_pipeline itself,
    // so it doesn't hold up the receiver count unnecessarily.
    // The status_handle will create its own subscription.
    drop(_first_event_rx);

    let runner_event_tx = event_tx.clone();
    let (worker_job_tx, worker_job_rx) = bounded::<WorkerJob>(WORKER_JOB_CHANNEL_SIZE);
    let core_config = config.clone();
    let core_cache_clone = cache.clone();
    let core_event_tx_for_worker_manager = event_tx.clone();
    let core_success_count_clone = Arc::clone(&final_success_count);
    let core_fail_count_clone = Arc::clone(&final_fail_count);
    let core_handle = std::thread::spawn(move || {
        sps_core::pipeline::engine::start_worker_pool_manager(
            core_config,
            core_cache_clone,
            worker_job_rx,
            core_event_tx_for_worker_manager,
            core_success_count_clone,
            core_fail_count_clone,
        )
    });

    let status_config = config.clone();
    let status_event_rx = event_tx.subscribe();
    let status_handle = tokio::spawn(crate::cli::status::handle_events(
        status_config,
        status_event_rx,
    ));

    // --- Planning Phase ---
    let planned_jobs: Vec<sps_common::pipeline::PlannedJob>;
    let mut overall_errors: Vec<(String, SpsError)>;
    let already_satisfied: HashSet<String>;

    {
        // Scope for planner to ensure its event_tx clone is dropped
        let planner_event_tx_clone = runner_event_tx.clone();
        planner_event_tx_clone
            .send(PipelineEvent::PlanningStarted)
            .ok();

        debug!("Initializing OperationPlanner...");
        let operation_planner =
            OperationPlanner::new(config, cache.clone(), flags, planner_event_tx_clone); // Pass cloned tx

        debug!("Starting operation planning...");
        let planned_ops_result = operation_planner
            .plan_operations(initial_targets, command_type.clone())
            .await;

        match planned_ops_result {
            Ok(ops) => {
                planned_jobs = ops.jobs;
                overall_errors = ops.errors;
                already_satisfied = ops.already_installed_or_up_to_date;
            }
            Err(e) => {
                error!("Fatal planning error: {}", e);
                runner_event_tx
                    .send(PipelineEvent::LogError {
                        message: format!("Fatal planning error: {e}"),
                    })
                    .ok();
                drop(worker_job_tx);
                if let Err(join_err) = core_handle.join() {
                    error!(
                        "Core thread join error after planning failure: {:?}",
                        get_panic_message(join_err)
                    );
                }
                let duration = start_time.elapsed();
                runner_event_tx
                    .send(PipelineEvent::PipelineFinished {
                        duration_secs: duration.as_secs_f64(),
                        success_count: 0,
                        fail_count: initial_targets.len(),
                    })
                    .ok();

                // Ensure all event_tx senders related to this scope are dropped
                drop(runner_event_tx);
                drop(event_tx);
                if let Err(join_err) = status_handle.await {
                    error!("Status task join error: {}", join_err);
                }
                return Err(e);
            }
        }
        // planner_event_tx_clone (and operation_planner) goes out of scope here
    }

    runner_event_tx
        .send(PipelineEvent::PlanningFinished {
            job_count: planned_jobs.len(),
        })
        .ok();

    for name in already_satisfied {
        debug!("{} is already installed", name.cyan())
    }
    for (name, err) in &overall_errors {
        let msg = format!("Error during planning for '{}': {}", name.cyan(), err);
        runner_event_tx
            .send(PipelineEvent::LogError { message: msg })
            .ok();
    }

    // --- Download Phase ---
    let total_jobs = planned_jobs.len();
    if total_jobs == 0 {
        let msg = if overall_errors.is_empty() {
            "No packages need to be installed, upgraded, or reinstalled."
        } else {
            "No operations possible due to planning errors."
        };
        debug!("{}", msg);
        runner_event_tx
            .send(PipelineEvent::LogInfo {
                message: msg.to_string(),
            })
            .ok();
        runner_event_tx
            .send(PipelineEvent::PipelineStarted { total_jobs: 0 })
            .ok();
        drop(worker_job_tx);
    } else {
        runner_event_tx
            .send(PipelineEvent::PipelineStarted { total_jobs })
            .ok();
        let download_coordinator_event_tx_clone = runner_event_tx.clone();
        let http_client = Arc::new(HttpClient::new());
        // DownloadCoordinator takes ownership of its event_tx clone.
        let download_coordinator = DownloadCoordinator::new(
            config,
            cache.clone(),
            http_client,
            download_coordinator_event_tx_clone,
        );
        debug!("Starting download coordination for {} jobs...", total_jobs);
        let download_coordinator_errors = download_coordinator
            .coordinate_downloads(planned_jobs, worker_job_tx.clone())
            .await;

        // download_coordinator goes out of scope here

        let download_fail_count = download_coordinator_errors.len();
        final_fail_count.fetch_add(download_fail_count, Ordering::Relaxed);
        overall_errors.extend(download_coordinator_errors);

        if download_fail_count > 0 {
            warn!("Encountered {} download error(s).", download_fail_count);
            runner_event_tx
                .send(PipelineEvent::LogWarn {
                    message: format!("Encountered {download_fail_count} download error(s)."),
                })
                .ok();
        }
        debug!("Download phase complete. Closing worker job channel.");
        drop(worker_job_tx);
    }

    // --- Core Processing ---
    match core_handle.join() {
        Ok(Ok(())) => debug!("run_pipeline: Core worker pool manager thread completed."),
        Ok(Err(e)) => {
            error!("Core worker pool manager thread failed: {}", e);
            overall_errors.push(("[Core Manager]".to_string(), e));
        }
        Err(e) => {
            let panic_msg = get_panic_message(e);
            error!("Core worker pool manager thread panicked: {}", panic_msg);
            overall_errors.push((
                "[Core Manager]".to_string(),
                SpsError::Generic(format!("Core thread panicked: {panic_msg}")),
            ));
        }
    }

    // --- Shutdown Status Handler ---
    drop(runner_event_tx);
    drop(event_tx);

    if let Err(e) = status_handle.await {
        warn!(
            "Status handler task failed or panicked after event_tx drop: {}",
            e
        );
    } else {
        debug!("Status handler task completed successfully.");
    }

    // --- Final Reporting ---
    let final_success = final_success_count.load(Ordering::Relaxed);
    let accumulated_failures_in_counter = final_fail_count.load(Ordering::Relaxed);

    let specific_planning_download_errors_count = overall_errors
        .iter()
        .filter(|(source, _)| {
            source != "[Core Manager]" && !source.starts_with("[DownloadTaskPanic]")
        })
        .count();

    let final_total_failures =
        accumulated_failures_in_counter + specific_planning_download_errors_count;

    if !overall_errors.is_empty() {
        for (src, e) in &overall_errors {
            debug!("  Err Src: {}, Details: {}", src, e);
        }
    }

    if final_total_failures == 0
        && (final_success > 0 || (total_jobs == 0 && overall_errors.is_empty()))
    {
        Ok(())
    } else {
        error!(
            "Pipeline execution completed with {} total reported failure(s).",
            final_total_failures
        );
        let specific_error_msg = overall_errors
            .into_iter()
            .map(|(n, e)| format!("'{n}': {e}"))
            .collect::<Vec<_>>()
            .join("; ");
        Err(SpsError::InstallError(format!(
            "Operation failed with {final_total_failures} total failure(s). Planning/Download errors: [{specific_error_msg}] (Worker errors counted in total)"
        )))
    }
}

pub(crate) fn get_panic_message(e: Box<dyn std::any::Any + Send>) -> String {
    match e.downcast_ref::<&'static str>() {
        Some(s) => (*s).to_string(),
        None => match e.downcast_ref::<String>() {
            Some(s) => s.clone(),
            None => "Unknown panic payload".to_string(),
        },
    }
}

#[instrument(skip(cache))]
pub(crate) async fn fetch_target_definitions(
    names: &[String],
    cache: Arc<Cache>,
) -> HashMap<String, SpsResult<InstallTargetIdentifier>> {
    let mut results = HashMap::new();
    if names.is_empty() {
        return results;
    }
    let mut futures = JoinSet::new();

    let formulae_map_handle = tokio::spawn(load_or_fetch_formulae_map(Arc::clone(&cache)));
    let casks_map_handle = tokio::spawn(load_or_fetch_casks_map(Arc::clone(&cache)));

    let formulae_map = match formulae_map_handle.await {
        Ok(Ok(map)) => Some(map),
        Ok(Err(e)) => {
            warn!("[FetchDefs] Failed to load/fetch full formulae list: {}", e);
            None
        }
        Err(e) => {
            warn!(
                "[FetchDefs] Formulae map loading task panicked: {}",
                get_panic_message(e.into_panic())
            );
            None
        }
    };
    let casks_map = match casks_map_handle.await {
        Ok(Ok(map)) => Some(map),
        Ok(Err(e)) => {
            warn!("[FetchDefs] Failed to load/fetch full casks list: {}", e);
            None
        }
        Err(e) => {
            warn!(
                "[FetchDefs] Casks map loading task panicked: {}",
                get_panic_message(e.into_panic())
            );
            None
        }
    };

    for name_str in names {
        let name = name_str.clone();
        let local_formulae_map = formulae_map.clone();
        let local_casks_map = casks_map.clone();

        futures.spawn(async move {
            if let Some(ref map) = local_formulae_map {
                if let Some(f_arc) = map.get(&name) {
                    return (name, Ok(InstallTargetIdentifier::Formula(f_arc.clone())));
                }
            }
            if let Some(ref map) = local_casks_map {
                if let Some(c_arc) = map.get(&name) {
                    return (name, Ok(InstallTargetIdentifier::Cask(c_arc.clone())));
                }
            }
            warn!("[FetchDefs] Definition for '{}' not found in cached lists, fetching directly from API...", name);
            match api::get_formula(&name).await {
                Ok(formula) => return (name, Ok(InstallTargetIdentifier::Formula(Arc::new(formula)))),
                Err(SpsError::NotFound(_)) => {}
                Err(e) => return (name, Err(e)),
            }
            match api::get_cask(&name).await {
                Ok(cask) => (name, Ok(InstallTargetIdentifier::Cask(Arc::new(cask)))),
                Err(SpsError::NotFound(_)) => (name.clone(), Err(SpsError::NotFound(format!("Formula or Cask '{name}' not found")))),
                Err(e) => (name, Err(e)),
            }
        });
    }

    while let Some(res) = futures.join_next().await {
        match res {
            Ok((name, result)) => {
                results.insert(name, result);
            }
            Err(e) => {
                let panic_message = get_panic_message(e.into_panic());
                error!(
                    "[FetchDefs] Task panicked during definition fetch: {}",
                    panic_message
                );
                results.insert(
                    format!("[unknown_target_due_to_panic_{}]", results.len()),
                    Err(SpsError::Generic(format!(
                        "Definition fetching task panicked: {panic_message}"
                    ))),
                );
            }
        }
    }
    results
}

async fn load_or_fetch_formulae_map(cache: Arc<Cache>) -> SpsResult<HashMap<String, Arc<Formula>>> {
    match cache.load_raw("formula.json") {
        Ok(data) => {
            let formulas: Vec<Formula> = serde_json::from_str(&data)
                .map_err(|e| SpsError::Cache(format!("Parse cached formula.json failed: {e}")))?;
            Ok(formulas
                .into_iter()
                .map(|f| (f.name.clone(), Arc::new(f)))
                .collect())
        }
        Err(_) => {
            debug!("[FetchDefs] Cache miss for formula.json, fetching from API...");
            let raw_data = api::fetch_all_formulas().await?;
            if let Err(e) = cache.store_raw("formula.json", &raw_data) {
                warn!("Failed to store formula.json in cache: {}", e);
            }
            let formulas: Vec<Formula> =
                serde_json::from_str(&raw_data).map_err(|e| SpsError::Json(Arc::new(e)))?;
            Ok(formulas
                .into_iter()
                .map(|f| (f.name.clone(), Arc::new(f)))
                .collect())
        }
    }
}

async fn load_or_fetch_casks_map(cache: Arc<Cache>) -> SpsResult<HashMap<String, Arc<Cask>>> {
    match cache.load_raw("cask.json") {
        Ok(data) => {
            let casks: Vec<Cask> = serde_json::from_str(&data)
                .map_err(|e| SpsError::Cache(format!("Parse cached cask.json failed: {e}")))?;
            Ok(casks
                .into_iter()
                .map(|c| (c.token.clone(), Arc::new(c)))
                .collect())
        }
        Err(_) => {
            debug!("[FetchDefs] Cache miss for cask.json, fetching from API...");
            let raw_data = api::fetch_all_casks().await?;
            if let Err(e) = cache.store_raw("cask.json", &raw_data) {
                warn!("Failed to store cask.json in cache: {}", e);
            }
            let casks: Vec<Cask> =
                serde_json::from_str(&raw_data).map_err(|e| SpsError::Json(Arc::new(e)))?;
            Ok(casks
                .into_iter()
                .map(|c| (c.token.clone(), Arc::new(c)))
                .collect())
        }
    }
}
