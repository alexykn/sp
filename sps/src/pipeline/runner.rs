// sps/src/pipeline/runner.rs
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use colored::Colorize;
use crossbeam_channel::bounded as crossbeam_bounded;
use reqwest::Client as HttpClient;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::dependency::resolver::{ResolutionStatus, ResolvedGraph};
use sps_common::error::{Result as SpsResult, SpsError};
use sps_common::model::InstallTargetIdentifier;
use sps_common::pipeline::{
    DownloadOutcome, JobProcessingState, PipelineEvent, PlannedJob,
    PlannedOperations as PlannerOutputCommon, WorkerJob,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, error, instrument, warn};

use super::downloader::DownloadCoordinator;
use super::planner::OperationPlanner;

const WORKER_JOB_CHANNEL_SIZE: usize = 100;
const EVENT_CHANNEL_SIZE: usize = 100;
const DOWNLOAD_OUTCOME_CHANNEL_SIZE: usize = 100;

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

struct PropagationContext {
    all_planned_jobs: Arc<Vec<PlannedJob>>,
    job_states: Arc<Mutex<HashMap<String, JobProcessingState>>>,
    resolved_graph: Arc<ResolvedGraph>,
    event_tx: Option<broadcast::Sender<PipelineEvent>>,
    final_fail_count: Arc<AtomicUsize>,
}

fn err_to_string(e: &SpsError) -> String {
    e.to_string()
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

    let (event_tx, mut event_rx_for_runner) =
        broadcast::channel::<PipelineEvent>(EVENT_CHANNEL_SIZE);

    let runner_event_tx_clone = event_tx.clone();

    let (worker_job_tx, worker_job_rx_for_core) =
        crossbeam_bounded::<WorkerJob>(WORKER_JOB_CHANNEL_SIZE);

    let core_config = config.clone();
    let core_cache_clone = cache.clone();
    let core_event_tx_for_worker_manager = event_tx.clone();
    let core_success_count_clone = Arc::clone(&final_success_count);
    let core_fail_count_clone = Arc::clone(&final_fail_count);
    let core_handle = std::thread::spawn(move || {
        sps_core::pipeline::engine::start_worker_pool_manager(
            core_config,
            core_cache_clone,
            worker_job_rx_for_core,
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

    let (download_outcome_tx, mut download_outcome_rx) =
        mpsc::channel::<DownloadOutcome>(DOWNLOAD_OUTCOME_CHANNEL_SIZE);

    let planner_output: PlannerOutputCommon;
    {
        let planner_event_tx_clone = runner_event_tx_clone.clone();

        let operation_planner =
            OperationPlanner::new(config, cache.clone(), flags, planner_event_tx_clone);

        match operation_planner
            .plan_operations(initial_targets, command_type.clone())
            .await
        {
            Ok(ops) => {
                planner_output = ops;
            }
            Err(e) => {
                error!("RUNNER: Fatal planning error: {}", e);
                runner_event_tx_clone
                    .send(PipelineEvent::LogError {
                        message: format!("Fatal planning error: {e}"),
                    })
                    .ok();
                drop(worker_job_tx);
                if let Err(join_err) = core_handle.join() {
                    error!(
                        "RUNNER: Core thread join error after planning failure: {:?}",
                        get_panic_message(join_err)
                    );
                }
                let duration = start_time.elapsed();
                runner_event_tx_clone
                    .send(PipelineEvent::PipelineFinished {
                        duration_secs: duration.as_secs_f64(),
                        success_count: 0,
                        fail_count: initial_targets.len(),
                    })
                    .ok();

                drop(runner_event_tx_clone);

                drop(event_tx);

                if let Err(join_err) = status_handle.await {
                    error!(
                        "RUNNER: Status task join error after planning failure: {}",
                        join_err
                    );
                }
                return Err(e);
            }
        }
    }

    let planned_jobs = Arc::new(planner_output.jobs);
    let resolved_graph = planner_output.resolved_graph.clone().ok_or_else(|| {
        SpsError::Generic("ResolvedGraph missing from planner output".to_string())
    })?;

    runner_event_tx_clone
        .send(PipelineEvent::PlanningFinished {
            job_count: planned_jobs.len(),
        })
        .ok();

    for name in &planner_output.already_installed_or_up_to_date {
        let msg = format!("{} is already installed or up-to-date.", name.cyan());
        runner_event_tx_clone
            .send(PipelineEvent::LogInfo { message: msg })
            .ok();
    }
    for (name, err) in &planner_output.errors {
        let msg = format!("Error during planning for '{}': {}", name.cyan(), err);

        runner_event_tx_clone
            .send(PipelineEvent::LogError { message: msg })
            .ok();
    }

    let job_processing_states = Arc::new(Mutex::new(HashMap::<String, JobProcessingState>::new()));
    let mut jobs_pending_or_active = 0;

    {
        let mut states_guard = job_processing_states.lock().unwrap();
        for job in planned_jobs.iter() {
            if planner_output
                .already_installed_or_up_to_date
                .contains(&job.target_id)
            {
                states_guard.insert(job.target_id.clone(), JobProcessingState::Succeeded);
                final_success_count.fetch_add(1, Ordering::Relaxed);
            } else if let Some((_, err)) = planner_output
                .errors
                .iter()
                .find(|(name, _)| name == &job.target_id)
            {
                states_guard.insert(
                    job.target_id.clone(),
                    JobProcessingState::Failed(Arc::new(err.clone())),
                );
                final_fail_count.fetch_add(1, Ordering::Relaxed);
            } else if job.use_private_store_source.is_some() {
                let path = job.use_private_store_source.clone().unwrap();
                states_guard.insert(
                    job.target_id.clone(),
                    JobProcessingState::Downloaded(path.clone()),
                );
                jobs_pending_or_active += 1;
            } else {
                states_guard.insert(job.target_id.clone(), JobProcessingState::PendingDownload);
                jobs_pending_or_active += 1;
            }
        }
    }

    let mut downloads_to_initiate = Vec::new();
    {
        let states_guard = job_processing_states.lock().unwrap();
        for job in planned_jobs.iter() {
            if matches!(
                states_guard.get(&job.target_id),
                Some(JobProcessingState::PendingDownload)
            ) {
                downloads_to_initiate.push(job.clone());
            }
        }
    }

    let mut download_coordinator_task_handle: Option<JoinHandle<Vec<(String, SpsError)>>> = None;

    if !downloads_to_initiate.is_empty() {
        let download_coordinator_event_tx_clone = runner_event_tx_clone.clone();
        let http_client = Arc::new(HttpClient::new());
        let config_for_downloader_owned = config.clone();

        let mut download_coordinator = DownloadCoordinator::new(
            config_for_downloader_owned,
            cache.clone(),
            http_client,
            download_coordinator_event_tx_clone,
        );
        let tx_for_download_task = download_outcome_tx.clone();

        download_coordinator_task_handle = Some(tokio::spawn(async move {
            let result = download_coordinator
                .coordinate_downloads(downloads_to_initiate, tx_for_download_task)
                .await;
            result
        }));
    } else if jobs_pending_or_active > 0 {
        check_and_dispatch(
            planned_jobs.clone(),
            job_processing_states.clone(),
            resolved_graph.clone(),
            &worker_job_tx,
            runner_event_tx_clone.clone(),
            config,
            flags,
        );
    }

    drop(download_outcome_tx);

    if !planned_jobs.is_empty() {
        runner_event_tx_clone
            .send(PipelineEvent::PipelineStarted {
                total_jobs: planned_jobs.len(),
            })
            .ok();
    }

    let mut propagation_ctx = PropagationContext {
        all_planned_jobs: planned_jobs.clone(),
        job_states: job_processing_states.clone(),
        resolved_graph: resolved_graph.clone(),
        event_tx: Some(runner_event_tx_clone.clone()),
        final_fail_count: final_fail_count.clone(),
    };

    debug!(
        "RUNNER: Entering main event loop. Jobs pending/active: {}",
        jobs_pending_or_active
    );
    while jobs_pending_or_active > 0 {
        tokio::select! {
            biased;
            Some(download_outcome) = download_outcome_rx.recv() => {
                debug!("RUNNER: Received DownloadOutcome for '{}'.", download_outcome.planned_job.target_id);
                process_download_outcome(
                    download_outcome,
                    &propagation_ctx,
                    &mut jobs_pending_or_active,
                );
                debug!("RUNNER: After process_download_outcome, jobs_pending_or_active: {}. Triggering check_and_dispatch.", jobs_pending_or_active);
                 check_and_dispatch(
                    planned_jobs.clone(),
                    job_processing_states.clone(),
                    resolved_graph.clone(),
                    &worker_job_tx,
                    runner_event_tx_clone.clone(),
                    config,
                    flags,
                );
            }
            Ok(event) = event_rx_for_runner.recv() => {
                match event {
                    PipelineEvent::JobSuccess { ref target_id, .. } => {
                        debug!("RUNNER: Received JobSuccess for '{}'.", target_id);
                        process_core_worker_feedback(
                            target_id.clone(),
                            true,
                            None,
                            job_processing_states.clone(),
                            &mut jobs_pending_or_active,
                        );
                        debug!("RUNNER: After JobSuccess for '{}', jobs_pending_or_active: {}. Triggering check_and_dispatch.", target_id, jobs_pending_or_active);
                        check_and_dispatch(
                            planned_jobs.clone(),
                            job_processing_states.clone(),
                            resolved_graph.clone(),
                            &worker_job_tx,
                            runner_event_tx_clone.clone(),
                            config,
                            flags,
                        );
                    }
                    PipelineEvent::JobFailed { ref target_id, ref error, ref action } => {
                        debug!("RUNNER: Received JobFailed for '{}' (Action: {:?}, Error: {}).", target_id, action, error);
                        process_core_worker_feedback(
                            target_id.clone(),
                            false,
                            Some(SpsError::Generic(error.clone())),
                            job_processing_states.clone(),
                            &mut jobs_pending_or_active,
                        );
                        debug!("RUNNER: After JobFailed for '{}', jobs_pending_or_active: {}. Triggering failure propagation.", target_id, jobs_pending_or_active);
                        propagate_failure(
                            target_id,
                            Arc::new(SpsError::Generic(format!("Core worker failed for {target_id}: {error}"))),
                            &propagation_ctx,
                            &mut jobs_pending_or_active,
                        );
                         check_and_dispatch(
                            planned_jobs.clone(),
                            job_processing_states.clone(),
                            resolved_graph.clone(),
                            &worker_job_tx,
                            runner_event_tx_clone.clone(),
                            config,
                            flags,
                        );
                    }
                    _ => {}
                }
            }
            else => {
                debug!("RUNNER: Main select loop 'else' branch. jobs_pending_or_active = {}. download_outcome_rx or event_rx_for_runner might be closed.", jobs_pending_or_active);
                if jobs_pending_or_active > 0 {
                    warn!("RUNNER: Exiting main loop prematurely but still have {} jobs pending/active. This might indicate a stall or logic error.", jobs_pending_or_active);
                }
                break;
            }
        }
        debug!(
            "RUNNER: End of select! loop iteration. Jobs pending/active: {}",
            jobs_pending_or_active
        );
    }
    debug!(
        "RUNNER: Main event loop finished. Final jobs_pending_or_active: {}",
        jobs_pending_or_active
    );

    drop(download_outcome_rx);

    if let Some(handle) = download_coordinator_task_handle {
        match handle.await {
            Ok(critical_download_errors) => {
                if !critical_download_errors.is_empty() {
                    warn!(
                        "RUNNER: DownloadCoordinator task reported critical errors: {:?}",
                        critical_download_errors
                    );
                    final_fail_count.fetch_add(critical_download_errors.len(), Ordering::Relaxed);
                }
            }
            Err(e) => {
                let panic_msg = get_panic_message(Box::new(e));
                error!(
                    "RUNNER: DownloadCoordinator task panicked or failed to join: {}",
                    panic_msg
                );
                final_fail_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    drop(worker_job_tx);
    match core_handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            error!("RUNNER: Core worker pool manager thread failed: {}", e);
            final_fail_count.fetch_add(1, Ordering::Relaxed);
        }
        Err(e) => {
            error!(
                "RUNNER: Core worker pool manager thread panicked: {:?}",
                get_panic_message(e)
            );
            final_fail_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    let duration = start_time.elapsed();
    let success_total = final_success_count.load(Ordering::Relaxed);
    let fail_total = final_fail_count.load(Ordering::Relaxed);

    if let Err(e) = runner_event_tx_clone.send(PipelineEvent::PipelineFinished {
        duration_secs: duration.as_secs_f64(),
        success_count: success_total,
        fail_count: fail_total,
    }) {
        warn!("RUNNER: Failed to send PipelineFinished event: {:?}. Status handler might not receive it.", e);
    }

    propagation_ctx.event_tx = None;

    drop(runner_event_tx_clone);

    drop(event_tx);

    if let Err(e) = status_handle.await {
        warn!("RUNNER: Status handler task failed or panicked: {}", e);
    }

    if fail_total == 0 {
        Ok(())
    } else {
        let mut accumulated_errors = Vec::new();
        for (name, err_obj) in planner_output.errors {
            accumulated_errors.push(format!("Planning for '{name}': {err_obj}"));
        }
        let states_guard = job_processing_states.lock().unwrap();
        for job in planned_jobs.iter() {
            if let Some(JobProcessingState::Failed(err_arc)) = states_guard.get(&job.target_id) {
                let err_str = err_to_string(err_arc);
                let job_err_msg = format!("Processing '{}': {}", job.target_id, err_str);
                if !accumulated_errors.contains(&job_err_msg) {
                    accumulated_errors.push(job_err_msg);
                }
            }
        }
        drop(states_guard);

        let specific_error_msg = if accumulated_errors.is_empty() {
            "No specific errors logged, check core worker logs.".to_string()
        } else {
            accumulated_errors.join("; ")
        };

        error!(
            "RUNNER: Pipeline execution completed with {} total failure(s) in {:.2}s.",
            fail_total,
            duration.as_secs_f64()
        );
        Err(SpsError::InstallError(format!(
            "Operation failed with {fail_total} total failure(s). Details: [{specific_error_msg}] (Worker errors are included in total)"
        )))
    }
}

fn process_download_outcome(
    outcome: DownloadOutcome,
    propagation_ctx: &PropagationContext,
    jobs_pending_or_active: &mut usize,
) {
    let job_id = outcome.planned_job.target_id.clone();
    let mut states_guard = propagation_ctx.job_states.lock().unwrap();

    match states_guard.get(&job_id) {
        Some(JobProcessingState::Succeeded) | Some(JobProcessingState::Failed(_)) => {
            debug!(
                "[{}] DownloadOutcome: Job already in terminal state {:?}. Ignoring outcome.",
                job_id,
                states_guard.get(&job_id)
            );
            return;
        }
        _ => {}
    }

    match outcome.result {
        Ok(path) => {
            debug!(
                "[{}] DownloadOutcome: Success. Path: {}. Updating state to Downloaded.",
                job_id,
                path.display()
            );
            states_guard.insert(job_id.clone(), JobProcessingState::Downloaded(path));
        }
        Err(e) => {
            warn!(
                "[{}] DownloadOutcome: Failed. Error: {}. Updating state to Failed.",
                job_id, e
            );
            let error_arc = Arc::new(e);
            states_guard.insert(
                job_id.clone(),
                JobProcessingState::Failed(error_arc.clone()),
            );

            if let Some(ref tx) = propagation_ctx.event_tx {
                tx.send(PipelineEvent::job_failed(
                    job_id.clone(),
                    outcome.planned_job.action.clone(),
                    &error_arc,
                ))
                .ok();
            }
            propagation_ctx
                .final_fail_count
                .fetch_add(1, Ordering::Relaxed);

            if *jobs_pending_or_active > 0 {
                *jobs_pending_or_active -= 1;
                debug!("[{}] DownloadOutcome: Decremented jobs_pending_or_active to {} due to download failure.", job_id, *jobs_pending_or_active);
            } else {
                warn!("[{}] DownloadOutcome: jobs_pending_or_active is already 0, cannot decrement for download failure.", job_id);
            }

            drop(states_guard);
            debug!("[{}] DownloadOutcome: Propagating failure.", job_id);
            propagate_failure(&job_id, error_arc, propagation_ctx, jobs_pending_or_active);
        }
    }
}

fn process_core_worker_feedback(
    target_id: String,
    success: bool,
    error: Option<SpsError>,
    job_states: Arc<Mutex<HashMap<String, JobProcessingState>>>,
    jobs_pending_or_active: &mut usize,
) {
    let mut states_guard = job_states.lock().unwrap();

    match states_guard.get(&target_id) {
        Some(JobProcessingState::Succeeded) | Some(JobProcessingState::Failed(_)) => {
            debug!("[{}] CoreFeedback: Job already in terminal state {:?}. Ignoring active job count update.", target_id, states_guard.get(&target_id));
            return;
        }
        _ => {}
    }

    if success {
        debug!(
            "[{}] CoreFeedback: Success. Updating state to Succeeded.",
            target_id
        );
        states_guard.insert(target_id.clone(), JobProcessingState::Succeeded);
    } else {
        let err_msg = error.as_ref().map_or_else(
            || "Unknown core worker error".to_string(),
            |e| e.to_string(),
        );
        debug!(
            "[{}] CoreFeedback: Failed. Error: {}. Updating state to Failed.",
            target_id, err_msg
        );
        let err_arc = Arc::new(
            error.unwrap_or_else(|| SpsError::Generic("Unknown core worker error".into())),
        );
        states_guard.insert(target_id.clone(), JobProcessingState::Failed(err_arc));
    }

    if *jobs_pending_or_active > 0 {
        *jobs_pending_or_active -= 1;
        debug!(
            "[{}] CoreFeedback: Decremented jobs_pending_or_active to {}.",
            target_id, *jobs_pending_or_active
        );
    } else {
        warn!(
            "[{}] CoreFeedback: jobs_pending_or_active is already 0, cannot decrement.",
            target_id
        );
    }
}

fn check_and_dispatch(
    planned_jobs_arc: Arc<Vec<PlannedJob>>,
    job_states: Arc<Mutex<HashMap<String, JobProcessingState>>>,
    resolved_graph: Arc<ResolvedGraph>,
    worker_job_tx: &crossbeam_channel::Sender<WorkerJob>,
    event_tx: broadcast::Sender<PipelineEvent>,
    config: &Config,
    flags: &PipelineFlags,
) {
    debug!("RUNNER: --- Enter check_and_dispatch ---");
    let mut states_guard = job_states.lock().unwrap();
    let mut dispatched_this_round = 0;

    for planned_job in planned_jobs_arc.iter() {
        let job_id = &planned_job.target_id;
        debug!("[{}] CheckDispatch: Evaluating job.", job_id);

        let (current_state_is_dispatchable, path_for_dispatch) = {
            match states_guard.get(job_id) {
                Some(JobProcessingState::Downloaded(ref path)) => {
                    debug!("[{}] CheckDispatch: Current state is Downloaded.", job_id);
                    (true, Some(path.clone()))
                }
                Some(JobProcessingState::WaitingForDependencies(ref path)) => {
                    debug!(
                        "[{}] CheckDispatch: Current state is WaitingForDependencies.",
                        job_id
                    );
                    (true, Some(path.clone()))
                }
                other_state => {
                    debug!(
                        "[{}] CheckDispatch: Not in a dispatchable state. Current state: {:?}.",
                        job_id,
                        other_state.map(|s| format!("{s:?}"))
                    );
                    (false, None)
                }
            }
        };

        if current_state_is_dispatchable {
            let path = path_for_dispatch.unwrap();
            drop(states_guard);
            debug!(
                "[{}] CheckDispatch: Calling are_dependencies_succeeded.",
                job_id
            );
            let dependencies_succeeded = are_dependencies_succeeded(
                job_id,
                &planned_job.target_definition,
                job_states.clone(),
                &resolved_graph,
                config,
                flags,
            );
            states_guard = job_states.lock().unwrap();
            debug!(
                "[{}] CheckDispatch: are_dependencies_succeeded returned: {}.",
                job_id, dependencies_succeeded
            );

            let current_state_after_dep_check = states_guard.get(job_id).cloned();
            if !matches!(
                current_state_after_dep_check,
                Some(JobProcessingState::Downloaded(_))
                    | Some(JobProcessingState::WaitingForDependencies(_))
            ) {
                debug!("[{}] CheckDispatch: State changed to {:?} while checking dependencies. Skipping dispatch.", job_id, current_state_after_dep_check);
                continue;
            }

            if dependencies_succeeded {
                debug!(
                    "[{}] CheckDispatch: All dependencies satisfied. Dispatching to core worker.",
                    job_id
                );
                let worker_job = WorkerJob {
                    request: planned_job.clone(),
                    download_path: path.clone(),
                    download_size_bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                    is_source_from_private_store: planned_job.use_private_store_source.is_some(),
                };
                if worker_job_tx.send(worker_job).is_ok() {
                    states_guard.insert(
                        job_id.clone(),
                        JobProcessingState::DispatchedToCore(path.clone()),
                    );
                    event_tx
                        .send(PipelineEvent::JobDispatchedToCore {
                            target_id: job_id.clone(),
                        })
                        .ok();
                    dispatched_this_round += 1;
                    debug!("[{}] CheckDispatch: Successfully dispatched.", job_id);
                } else {
                    error!("[{}] CheckDispatch: Failed to send job to worker channel (channel closed?). Marking as failed.", job_id);
                    let err = Arc::new(SpsError::Generic("Worker channel closed".to_string()));
                    if !matches!(
                        states_guard.get(job_id),
                        Some(JobProcessingState::Failed(_))
                    ) {
                        states_guard
                            .insert(job_id.clone(), JobProcessingState::Failed(err.clone()));
                        event_tx
                            .send(PipelineEvent::job_failed(
                                job_id.clone(),
                                planned_job.action.clone(),
                                &err,
                            ))
                            .ok();
                    }
                }
            } else if matches!(
                current_state_after_dep_check,
                Some(JobProcessingState::Downloaded(_))
            ) {
                debug!("[{}] CheckDispatch: Dependencies not met. Updating state to WaitingForDependencies.", job_id);
                states_guard.insert(
                    job_id.clone(),
                    JobProcessingState::WaitingForDependencies(path.clone()),
                );
            } else {
                debug!(
                    "[{}] CheckDispatch: Dependencies not met. State remains {:?}.",
                    job_id, current_state_after_dep_check
                );
            }
        }
    }
    if dispatched_this_round > 0 {
        debug!(
            "RUNNER: Dispatched {} jobs to core workers in this round.",
            dispatched_this_round
        );
    }
}

fn are_dependencies_succeeded(
    target_id: &str,
    target_def: &InstallTargetIdentifier,
    job_states_arc: Arc<Mutex<HashMap<String, JobProcessingState>>>,
    resolved_graph: &ResolvedGraph,
    config: &Config,
    flags: &PipelineFlags,
) -> bool {
    debug!("[{}] AreDepsSucceeded: Checking dependencies...", target_id);
    let dependencies_to_check: Vec<String> = match target_def {
        InstallTargetIdentifier::Formula(formula_arc) => {
            if let Some(resolved_dep_info) =
                resolved_graph.resolution_details.get(formula_arc.name())
            {
                let parent_strategy = resolved_dep_info.determined_install_strategy;
                let context = sps_common::dependency::ResolutionContext {
                    formulary: &sps_common::formulary::Formulary::new(config.clone()),
                    keg_registry: &sps_common::keg::KegRegistry::new(config.clone()),
                    sps_prefix: config.sps_root(),
                    include_optional: flags.include_optional,
                    include_test: false,
                    skip_recommended: flags.skip_recommended,
                    initial_target_preferences: &Default::default(),
                    build_all_from_source: flags.build_from_source,
                    cascade_source_preference_to_dependencies: true,
                    has_bottle_for_current_platform:
                        sps_core::install::bottle::has_bottle_for_current_platform,
                };

                let deps: Vec<String> = formula_arc
                    .dependencies()
                    .unwrap_or_default()
                    .iter()
                    .filter(|dep_edge| {
                        context.should_process_dependency_edge(
                            formula_arc,
                            dep_edge.tags,
                            parent_strategy,
                        )
                    })
                    .map(|dep_edge| dep_edge.name.clone())
                    .collect();
                deps
            } else {
                warn!("[{}] AreDepsSucceeded: Formula not found in ResolvedGraph. Assuming no dependencies.", target_id);
                Vec::new()
            }
        }
        InstallTargetIdentifier::Cask(cask_arc) => {
            if let Some(deps_on) = &cask_arc.depends_on {
                deps_on.formula.clone()
            } else {
                Vec::new()
            }
        }
    };

    if dependencies_to_check.is_empty() {
        return true;
    }

    let states_guard = job_states_arc.lock().unwrap();
    for dep_name in &dependencies_to_check {
        match states_guard.get(dep_name) {
            Some(JobProcessingState::Succeeded) => {}
            Some(JobProcessingState::Failed(err)) => {
                debug!(
                    "[{}] AreDepsSucceeded: Dependency '{}' is FAILED ({}). Returning false.",
                    target_id,
                    dep_name,
                    err_to_string(err)
                );
                return false;
            }
            None => {
                if let Some(resolved_dep_detail) = resolved_graph.resolution_details.get(dep_name) {
                    if resolved_dep_detail.status == ResolutionStatus::Installed {
                    } else {
                        return false;
                    }
                } else {
                    warn!("[{}] AreDepsSucceeded: Dependency '{}' not found in job_states OR ResolvedGraph. Assuming not met. Returning false.", target_id, dep_name);
                    return false;
                }
            }
            _other_state => {
                return false;
            }
        }
    }
    true
}

fn propagate_failure(
    failed_job_id: &str,
    failure_reason: Arc<SpsError>,
    ctx: &PropagationContext,
    jobs_pending_or_active: &mut usize,
) {
    debug!(
        "[{}] PropagateFailure: Starting for reason: {}",
        failed_job_id, failure_reason
    );
    let mut dependents_to_fail_queue = vec![failed_job_id.to_string()];
    let mut newly_failed_dependents = HashSet::new();

    {
        let mut states_guard = ctx.job_states.lock().unwrap();
        if !matches!(
            states_guard.get(failed_job_id),
            Some(JobProcessingState::Failed(_))
        ) {
            states_guard.insert(
                failed_job_id.to_string(),
                JobProcessingState::Failed(failure_reason.clone()),
            );
        }
    }

    let mut current_idx = 0;
    while current_idx < dependents_to_fail_queue.len() {
        let current_source_of_failure = dependents_to_fail_queue[current_idx].clone();
        current_idx += 1;

        for job_to_check in ctx.all_planned_jobs.iter() {
            if job_to_check.target_id == failed_job_id
                || newly_failed_dependents.contains(&job_to_check.target_id)
            {
                continue;
            }

            let is_dependent = match &job_to_check.target_definition {
                InstallTargetIdentifier::Formula(formula_arc) => ctx
                    .resolved_graph
                    .resolution_details
                    .get(formula_arc.name())
                    .is_some_and(|res_dep_info| {
                        res_dep_info
                            .formula
                            .dependencies()
                            .unwrap_or_default()
                            .iter()
                            .any(|d| d.name == current_source_of_failure)
                    }),
                InstallTargetIdentifier::Cask(cask_arc) => {
                    cask_arc.depends_on.as_ref().is_some_and(|deps| {
                        deps.formula.contains(&current_source_of_failure)
                            || deps.cask.contains(&current_source_of_failure)
                    })
                }
            };

            if is_dependent {
                let mut states_guard = ctx.job_states.lock().unwrap();
                let current_state_of_dependent = states_guard.get(&job_to_check.target_id).cloned();

                if !matches!(
                    current_state_of_dependent,
                    Some(JobProcessingState::Succeeded) | Some(JobProcessingState::Failed(_))
                ) {
                    let propagated_error = Arc::new(SpsError::DependencyError(format!(
                        "Dependency '{}' failed: {}",
                        current_source_of_failure,
                        err_to_string(&failure_reason)
                    )));
                    states_guard.insert(
                        job_to_check.target_id.clone(),
                        JobProcessingState::Failed(propagated_error.clone()),
                    );

                    if newly_failed_dependents.insert(job_to_check.target_id.clone()) {
                        dependents_to_fail_queue.push(job_to_check.target_id.clone());
                        ctx.final_fail_count.fetch_add(1, Ordering::Relaxed);

                        if *jobs_pending_or_active > 0 {
                            *jobs_pending_or_active -= 1;
                            debug!("[{}] PropagateFailure: Decremented jobs_pending_or_active to {} for propagated failure.", job_to_check.target_id, *jobs_pending_or_active);
                        } else {
                            warn!("[{}] PropagateFailure: jobs_pending_or_active is already 0, cannot decrement for propagated failure.", job_to_check.target_id);
                        }

                        if let Some(ref tx) = ctx.event_tx {
                            tx.send(PipelineEvent::job_failed(
                                job_to_check.target_id.clone(),
                                job_to_check.action.clone(),
                                &propagated_error,
                            ))
                            .ok();
                        }
                    }
                }
                drop(states_guard);
            }
        }
    }
}
