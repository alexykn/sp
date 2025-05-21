// sps/src/pipeline/runner.rs
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::{HashMap, HashSet}; // Ensure HashMap and HashSet are imported
use std::sync::{Arc, Mutex};
use std::time::Instant;

use colored::Colorize;
use crossbeam_channel::bounded;
use reqwest::Client as HttpClient;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::{Result as SpsResult, SpsError};
use sps_common::dependency::resolver::ResolvedGraph; // Added
use sps_common::model::{Cask, Formula, InstallTargetIdentifier};
use sps_common::pipeline::{
    DownloadOutcome, JobAction, PipelineEvent, PipelinePackageType, PlannedJob as SpsPlannedJob,
    WorkerJob,
};
use sps_net::api;
use std::collections::VecDeque; // Added
use std::fs;
use tokio::sync::{broadcast::{self, error::RecvError}, mpsc};
use tokio::task::JoinSet;
use tracing::{debug, error, instrument, warn}; // Added info
use std::path::PathBuf;

use self::downloader::DownloadCoordinator;
use self::planner::OperationPlanner;
use super::{downloader, planner};

const WORKER_JOB_CHANNEL_SIZE: usize = 100;
const EVENT_CHANNEL_SIZE: usize = 100;

/// The state of a job during processing.
#[derive(Debug, Clone)]
pub enum JobProcessingState {
    /// The job is waiting to be downloaded.
    PendingDownload,
    /// The job is currently being downloaded.
    Downloading,
    /// The job has been downloaded.
    Downloaded(PathBuf),
    /// The job is waiting for dependencies to be installed.
    WaitingForDependencies(PathBuf), // MODIFIED: now stores PathBuf
    /// The job is currently being installed.
    Installing,
    /// The job has been successfully installed.
    Succeeded,
    /// The job failed to install.
    Failed(String),
}

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
    let planned_jobs: Vec<SpsPlannedJob>; // Using alias SpsPlannedJob
    let mut overall_errors: Vec<(String, SpsError)>;
    let already_satisfied: HashSet<String>;
    let resolved_dependency_graph_arc: Arc<ResolvedGraph>; // Added to store the graph

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
                resolved_dependency_graph_arc = ops.resolved_graph; // Store the graph
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

    // --- Initialize Job Processing States ---
    let job_processing_states = Arc::new(Mutex::new(HashMap::<String, JobProcessingState>::new()));
    let planning_errors_map: HashMap<String, SpsError> = overall_errors.iter().cloned().collect();

    for pj in &planned_jobs {
        let mut states_guard = job_processing_states.lock().unwrap();
        if already_satisfied.contains(&pj.target_id) {
            states_guard.insert(pj.target_id.clone(), JobProcessingState::Succeeded);
            final_success_count.fetch_add(1, Ordering::Relaxed);
            runner_event_tx
                .send(PipelineEvent::JobSuccess {
                    target_id: pj.target_id.clone(),
                    action: pj.action.clone(), // Assuming JobAction is clonable
                    pkg_type: get_pipeline_package_type(&pj.target_definition),
                })
                .ok();
        } else if let Some(err) = planning_errors_map.get(&pj.target_id) {
            states_guard.insert(
                pj.target_id.clone(),
                JobProcessingState::Failed(err.to_string()),
            );
            final_fail_count.fetch_add(1, Ordering::Relaxed);
            runner_event_tx
                .send(PipelineEvent::JobFailed {
                    target_id: pj.target_id.clone(),
                    action: pj.action.clone(), // Assuming JobAction is clonable
                    error: err.to_string(),
                })
                .ok();
        } else if let Some(private_path) = &pj.use_private_store_source {
            states_guard.insert(
                pj.target_id.clone(),
                JobProcessingState::Downloaded(private_path.clone()),
            );
        } else {
            states_guard.insert(pj.target_id.clone(), JobProcessingState::PendingDownload);
        }
    }
    // Drop the guard explicitly before potential async operations or other complex logic
    // though in this sequential initialization, it's less critical right here.
    // drop(states_guard); // Not needed as it goes out of scope with the loop

    // --- Download Phase ---
    // Recalculate total_jobs to only include those not already handled (succeeded/failed in planning)
    // This is a conceptual note; the current logic for total_jobs might need adjustment later
    // if we want total_jobs to reflect only jobs proceeding to download/install.
    // For now, total_jobs reflects all initially planned jobs.
    // This count might be adjusted later if we only want to count jobs proceeding to actual execution.
    let total_jobs_in_plan = planned_jobs.len(); // Use a distinct name

    // --- Download Phase Refactor ---
    let (download_outcome_tx, mut download_outcome_rx) = mpsc::channel::<DownloadOutcome>(EVENT_CHANNEL_SIZE);
    let mut jobs_to_download: Vec<SpsPlannedJob> = Vec::new(); // Use SpsPlannedJob type

    { // Scope for job_processing_states lock
        let mut states_guard = job_processing_states.lock().unwrap();
        for p_job in &planned_jobs {
            // Check the current state of the job.
            // Jobs already Succeeded or Failed (e.g. due to planning errors or already_satisfied)
            // or already marked as Downloaded (e.g. private store) should not be downloaded again.
            if let Some(JobProcessingState::PendingDownload) = states_guard.get(&p_job.target_id) {
                jobs_to_download.push(p_job.clone());
                states_guard.insert(p_job.target_id.clone(), JobProcessingState::Downloading);
            }
        }
    } // states_guard is dropped here

    let download_coordinator_handle = if !jobs_to_download.is_empty() {
        runner_event_tx
            .send(PipelineEvent::PipelineStarted { total_jobs: jobs_to_download.len() }) // Event with actual download count
            .ok();

        let download_coordinator_event_tx_clone = runner_event_tx.clone();
        let http_client = Arc::new(HttpClient::new()); // Create new or ensure it's available
        let download_coordinator = DownloadCoordinator::new(
            config,
            cache.clone(),
            http_client,
            download_coordinator_event_tx_clone,
        );
        debug!("Spawning DownloadCoordinator for {} jobs...", jobs_to_download.len());
        let handle = tokio::spawn(
            download_coordinator.coordinate_downloads(jobs_to_download, download_outcome_tx),
        );
        Some(handle)
    } else {
        debug!("No jobs require downloading.");
        // If there were no jobs to download, but there were other jobs (e.g. all succeeded, failed, or private store),
        // the PipelineStarted event might have already been sent with total_jobs_in_plan.
        // Or, if all jobs were pre-processed, this might be the point to send PipelineStarted {0}
        // if it wasn't handled by the "total_jobs_in_plan == 0" block.
        // For now, let's assume if jobs_to_download is empty, and total_jobs_in_plan > 0,
        // those jobs are being handled by the core worker pool directly or are already done.
        // If total_jobs_in_plan was 0, that case is handled above.
        // If jobs_to_download is empty, we don't need to close worker_job_tx yet if other jobs might use it.
        // However, if ONLY downloads lead to worker jobs, and there are no downloads, then it can be closed.

        // If no downloads are needed, and other jobs might exist (e.g. already downloaded, private store)
        // we still need to ensure the worker_job_tx is managed correctly.
        // The current logic is that DownloadCoordinator was responsible for closing worker_job_tx.
        // This responsibility will shift. For now, if no downloads, worker_job_tx remains open
        // for potential direct enqueuing of already downloaded/private store items later.
        // If this assumption is wrong, and worker_job_tx is ONLY fed by downloads,
        // then it should be dropped here if jobs_to_download is empty.

        // Let's refine: if total_jobs_in_plan is 0, the earlier block handles it (and drops worker_job_tx).
        // If jobs_to_download is empty, but total_jobs_in_plan > 0, it means all jobs
        // were either: already satisfied, failed in planning, or are private store.
        // These private store or "already downloaded" (if that concept exists pre-core)
        // will need to be sent to worker_job_tx. This will be part of the next step.
        // For now, if no downloads, the coordinator isn't spawned.
        None
    };

    // The old "total_jobs == 0" block needs adjustment.
    // It should check if there are *any* operations at all after initial state processing.
    let active_jobs_exist = {
        let states_guard = job_processing_states.lock().unwrap();
        states_guard.values().any(|state| {
            matches!(
                state,
                JobProcessingState::PendingDownload
                    | JobProcessingState::Downloading
                    | JobProcessingState::Downloaded(_)
                    | JobProcessingState::WaitingForDependencies
                    | JobProcessingState::Installing
            )
        })
    };

    if !active_jobs_exist {
        let msg = if overall_errors.is_empty() && already_satisfied.len() == total_jobs_in_plan {
            "All packages already installed and up-to-date."
        } else if !overall_errors.is_empty() {
            "No operations possible due to planning errors for all targets."
        } else {
            "No packages require processing." // General catch-all
        };
        debug!("{}", msg);
        runner_event_tx.send(PipelineEvent::LogInfo { message: msg.to_string() }).ok();
        // If PipelineStarted was sent by planner, this might be redundant or needs specific context.
        // Let's ensure PipelineStarted is sent once, perhaps based on `active_jobs_exist` or `jobs_to_download.len()`.
        // The current PipelineStarted { total_jobs } refers to the number of jobs *sent to download coordinator*.
        // If no active jobs, it means the pipeline effectively started and finished.
        if download_coordinator_handle.is_none() { // No downloads were initiated
             runner_event_tx.send(PipelineEvent::PipelineStarted { total_jobs: 0 }).ok();
        }
        drop(worker_job_tx); // No jobs for workers if none are active
    }
    // The else part of the original "total_jobs == 0" block is now handled by the spawning of DownloadCoordinator.
    // The specific drop(worker_job_tx) from the old "total_jobs == 0" block is now conditional.
    // The general drop(worker_job_tx) after download phase will be handled by the main event loop
    // once all download outcomes and subsequent worker jobs are processed.

    // --- Prepare Planned Jobs Map for easy lookup ---
    let planned_jobs_map: HashMap<String, &SpsPlannedJob> =
        planned_jobs.iter().map(|job| (job.target_id.clone(), job)).collect();

    // --- Initialize Completed Jobs Counter ---
    let mut completed_jobs_count = {
        let states_guard = job_processing_states.lock().unwrap();
        states_guard.values().filter(|state| matches!(state, JobProcessingState::Succeeded | JobProcessingState::Failed(_))).count()
    };
    debug!("Initial completed_jobs_count: {}", completed_jobs_count);

    // --- Main Event and Dispatch Loop ---
    let mut core_event_rx = event_tx.subscribe(); // Receiver for core events

    loop {
        if completed_jobs_count >= total_jobs_in_plan {
            debug!("All {} jobs have reached a terminal state. Exiting main event loop.", total_jobs_in_plan);
            break;
        }

        tokio::select! {
            biased; // Prioritize download outcomes if many events are ready

            // Handle Download Outcomes
            maybe_download_outcome = download_outcome_rx.recv(), if download_coordinator_handle.is_some() => {
                match maybe_download_outcome {
                    Some(download_outcome) => {
                        let planned_job = download_outcome.planned_job; // Take ownership
                        let job_id = planned_job.target_id.clone();
                        let result = download_outcome.result;

                        let mut states_map_guard = job_processing_states.lock().unwrap();

                        match result {
                            Ok(path) => {
                                states_map_guard.insert(job_id.clone(), JobProcessingState::Downloaded(path.clone()));
                                debug!(
                                    "[EventLoop] Job {} downloaded to {:?}. Current state: Downloaded.",
                                    job_id, path
                                );
                                drop(states_map_guard); // Release lock before calling check_and_dispatch
                                check_and_dispatch_jobs(
                                    &job_processing_states,
                                    &planned_jobs,
                                    &resolved_dependency_graph_arc,
                                    &worker_job_tx,
                                );
                            }
                            Err(error) => {
                                error!(
                                    "[EventLoop] Download failed for job {}: {}. Current state: Failed.",
                                    job_id, error
                                );
                                states_map_guard.insert(job_id.clone(), JobProcessingState::Failed(error.to_string()));
                                drop(states_map_guard); // Release lock before calling check_and_dispatch or emitting events

                                runner_event_tx.send(PipelineEvent::JobFailed {
                                    target_id: job_id.clone(),
                                    action: planned_job.action.clone(),
                                    error: error.to_string(),
                                }).ok();

                                final_fail_count.fetch_add(1, Ordering::Relaxed);
                                completed_jobs_count += 1;

                                propagate_failure(
                                    &job_id,
                                    &job_processing_states,
                                    &resolved_dependency_graph_arc,
                                    &runner_event_tx,
                                    &final_fail_count,
                                    &mut completed_jobs_count,
                                    &planned_jobs_map,
                                );
                                check_and_dispatch_jobs(
                                    &job_processing_states,
                                    &planned_jobs,
                                    &resolved_dependency_graph_arc,
                                    &worker_job_tx,
                                );
                            }
                        }
                    }
                    None => {
                        // download_outcome_rx closed. This means DownloadCoordinator is done.
                        // No more download outcomes will arrive.
                        debug!("[EventLoop] Download outcome channel closed.");
                        // Set download_coordinator_handle to None to disable this select branch.
                        // However, the handle itself is awaited later.
                        // This logic is tricky; if the handle is awaited *after* this loop,
                        // then we just stop listening here.
                        // For now, we assume it being None means it won't be polled again.
                        // To truly disable, we'd need to shadow download_coordinator_handle or use a flag.
                        // The `if download_coordinator_handle.is_some()` guard on the branch handles this.
                    }
                }
            }

            // Handle Core Worker Events (JobSuccess, JobFailed from sps-core)
            core_message = core_event_rx.recv() => {
                match core_message {
                    Ok(pipeline_event) => {
                        debug!("[EventLoop] Received core event: {:?}", pipeline_event);
                        match pipeline_event {
                            PipelineEvent::JobSuccess { target_id, action, pkg_type } => {
                                let mut states_map_guard = job_processing_states.lock().unwrap();
                                // It's possible the job was already marked failed due to a parallel issue (e.g., dependency failure propagated)
                                // Only increment completed_jobs_count if it wasn't already terminal.
                                // However, for simplicity here, we assume core events are authoritative for their specific job's outcome if not already failed.
                                if !matches!(states_map_guard.get(&target_id), Some(JobProcessingState::Failed(_))) {
                                   // If it was already Succeeded (e.g. from planning phase), this is a duplicate but harmless to overwrite.
                                   // If it was in an intermediate state, this marks it complete.
                                   if !matches!(states_map_guard.get(&target_id), Some(JobProcessingState::Succeeded)) {
                                       completed_jobs_count += 1;
                                   }
                                   states_map_guard.insert(target_id.clone(), JobProcessingState::Succeeded);
                                }
                                // final_success_count is managed by sps-core based on JobReport

                                debug!(
                                    "[EventLoop] Job {} Succeeded. Current state: Succeeded. Action: {:?}, Type: {:?}.",
                                    target_id, action, pkg_type
                                );
                                drop(states_map_guard); // Release lock
                                check_and_dispatch_jobs(
                                    &job_processing_states,
                                    &planned_jobs,
                                    &resolved_dependency_graph_arc,
                                    &worker_job_tx,
                                );
                            }
                            PipelineEvent::JobFailed { target_id, action, error } => {
                                let mut states_map_guard = job_processing_states.lock().unwrap();
                                if !matches!(states_map_guard.get(&target_id), Some(JobProcessingState::Succeeded | JobProcessingState::Failed(_))) {
                                    completed_jobs_count += 1;
                                }
                                states_map_guard.insert(target_id.clone(), JobProcessingState::Failed(error.clone()));
                                drop(states_map_guard); // Release lock

                                error!(
                                    "[EventLoop] Job {} Failed during core processing: {}. Current state: Failed. Action: {:?}",
                                    target_id, error, action
                                );
                                propagate_failure(
                                    &target_id,
                                    &job_processing_states,
                                    &resolved_dependency_graph_arc,
                                    &runner_event_tx,
                                    &final_fail_count,
                                    &mut completed_jobs_count,
                                    &planned_jobs_map,
                                );
                                check_and_dispatch_jobs(
                                    &job_processing_states,
                                    &planned_jobs,
                                    &resolved_dependency_graph_arc,
                                    &worker_job_tx,
                                );
                            }
                            // might update state but don't necessarily mark the job as complete.
                            // These are primarily for detailed status reporting via handle_events.
                            _ => { /* Other pipeline events are ignored for job completion counting here */ }
                        }
                    }
                    Err(RecvError::Closed) => {
                        warn!("[EventLoop] Core event channel (event_tx) closed. Terminating loop.");
                        break; // Exit loop if the main event source is gone
                    }
                    Err(RecvError::Lagged(num_skipped)) => {
                        warn!("[EventLoop] Core event receiver lagged, skipped {} messages.", num_skipped);
                        // Continue processing, but be aware that some events were missed.
                    }
                }
            }

            // Ensure progress even if one channel is quiet or busy
            // else => {
            //     // This 'else' branch in select! runs if no other branch is ready immediately.
            //     // Useful for periodic checks or to prevent tight spinning if all futures are pending.
            //     // However, for event-driven loops, it might not be necessary if channels are always active or eventually yield.
            //     // Consider if needed for specific conditions like timeout or deadlock detection.
            // }
        }
        // Loop termination condition is checked at the start of the loop.
    }

    debug!("[Pipeline] Main event loop finished.");

    // --- Post-Loop Operations ---

    // Await DownloadCoordinator if it was spawned
    if let Some(handle) = download_coordinator_handle {
        debug!("[Pipeline] Awaiting DownloadCoordinator completion...");
        match handle.await {
            Ok(Ok(())) => {
                debug!("[Pipeline] DownloadCoordinator completed successfully.");
            }
            Ok(Err(e)) => {
                error!("[Pipeline] DownloadCoordinator failed: {}", e);
                overall_errors.push(("[DownloadCoordinator]".to_string(), e));
                // final_fail_count might need adjustment if not already counted by individual outcomes
            }
            Err(join_error) => {
                let panic_msg = get_panic_message(join_error.into_panic());
                error!("[Pipeline] DownloadCoordinator task panicked: {}", panic_msg);
                overall_errors.push((
                    "[DownloadCoordinatorPanic]".to_string(),
                    SpsError::Generic(format!("DownloadCoordinator panicked: {panic_msg}")),
                ));
                // final_fail_count might need adjustment
            }
        }
    } else {
        debug!("[Pipeline] No DownloadCoordinator was spawned.");
    }

    // All download outcomes processed (or coordinator finished/panicked),
    // and all jobs that *could* be sent to workers from downloads *have* been sent.
    // Now it's safe to close the worker_job_tx channel.
    debug!("[Pipeline] Closing worker job channel (worker_job_tx).");
    drop(worker_job_tx);

    // Await Core Worker Pool Manager
    debug!("[Pipeline] Awaiting core worker pool manager completion...");
    match core_handle.join() {
        Ok(Ok(())) => debug!("[Pipeline] Core worker pool manager thread completed successfully."),
        Ok(Err(e)) => {
            error!("[Pipeline] Core worker pool manager thread failed: {}", e);
            overall_errors.push(("[CoreManager]".to_string(), e));
        }
        Err(e) => {
            let panic_msg = get_panic_message(e);
            error!("[Pipeline] Core worker pool manager thread panicked: {}", panic_msg);
            overall_errors.push((
                "[CoreManagerPanic]".to_string(),
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
    let actual_succeeded_jobs_count = final_success_count.load(Ordering::Relaxed);
    let actual_failed_jobs_count = final_fail_count.load(Ordering::Relaxed);

    // overall_errors contains initial planning errors, and critical errors like DownloadCoordinator/CoreManager panics/failures.
    // Job-specific download/install failures are primarily tracked by final_fail_count.
    // A critical system error is one that makes the pipeline fail regardless of job counts.
    let has_critical_system_errors = overall_errors.iter().any(|(src, _err)| {
        src == "[DownloadCoordinator]" || src.starts_with("[DownloadCoordinatorPanic]") ||
        src == "[CoreManager]" || src.starts_with("[CoreManagerPanic]") ||
        src == "[Fatal Planning]" // Assuming a distinct marker for fatal planning errors if needed
    });

    // Log final counts
    info!(
        "Pipeline final counts: Succeeded jobs: {}, Failed jobs: {}, Total jobs in plan: {}. Critical system errors present: {}.",
        actual_succeeded_jobs_count, actual_failed_jobs_count, total_jobs_in_plan, has_critical_system_errors
    );
    if !overall_errors.is_empty() {
        debug!("Details of overall_errors collected during pipeline execution:");
        for (src, e) in &overall_errors { // Use the original overall_errors
            debug!("  Source: [{}], Error: {}", src, e);
        }
    }

    // Success Condition: No failed jobs AND no critical system errors.
    // Also, if there were no jobs to begin with, it's a success if no errors occurred.
    if actual_failed_jobs_count == 0 && !has_critical_system_errors {
        if total_jobs_in_plan == 0 && overall_errors.is_empty() {
            info!("Pipeline completed successfully: No packages needed processing.");
        } else if total_jobs_in_plan == 0 && !overall_errors.is_empty() {
            // This case should ideally be caught by has_critical_system_errors if overall_errors implies critical.
            // If overall_errors had non-critical planning messages for 0 jobs, this might be a nuanced case.
            // For now, if overall_errors is not empty, it's a failure.
            let error_message_summary = overall_errors
                .iter()
                .map(|(n, e)| format!("[Source: {}] {}", n, e))
                .collect::<Vec<_>>()
                .join("; ");
            error!(
                "Pipeline finished: No jobs were planned, but system errors were reported: {}",
                error_message_summary
            );
            return Err(SpsError::InstallError(format!(
                "Pipeline encountered system errors with no jobs planned: {}",
                 error_message_summary
            )));
        } else {
            info!(
                "Pipeline completed successfully. Succeeded jobs: {}, Total jobs in plan: {}.",
                actual_succeeded_jobs_count, total_jobs_in_plan
            );
        }
    {
        Ok(())
    } else {
        // Construct a comprehensive error message
        let mut error_messages_parts = Vec::new();
        if actual_failed_jobs_count > 0 {
            error_messages_parts.push(format!("{} job(s) failed", actual_failed_jobs_count));
        }

        // Add distinct system errors to the message if they haven't been implicitly covered by failed job counts
        // For now, list all `overall_errors` for clarity, as they provide context.
        // The user sees the count of failed jobs, and then a list of all errors that occurred.
        if !overall_errors.is_empty() {
             error_messages_parts.push(format!(
                "System/Setup errors encountered: [{}]",
                overall_errors
                    .iter()
                    .map(|(src, e)| format!("[{}] {}", src, e))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        
        let comprehensive_error_message = if error_messages_parts.is_empty() {
            // This case should ideally not be reached if we are in this 'else' block.
            // It implies actual_failed_jobs_count is 0 and overall_errors is empty, but has_critical_system_errors was true.
            // Or, total_jobs_in_plan == 0 but overall_errors was not empty (and not critical, which is a contradiction).
            // Add a generic message if somehow this state is reached.
            "Pipeline execution failed due to unspecified reasons.".to_string()
        } else {
            error_messages_parts.join(". ")
        };

        error!(
            "Pipeline execution completed with errors. Failed jobs: {}, Succeeded jobs: {}. Critical system errors: {}, Overall errors listed: {}.",
            actual_failed_jobs_count, actual_succeeded_jobs_count, has_critical_system_errors, overall_errors.len()
        );
        Err(SpsError::InstallError(comprehensive_error_message))
    }
}

#[allow(clippy::too_many_arguments)] // This function has many parameters, which is complex but necessary for its role.
fn check_and_dispatch_jobs(
    job_processing_states_arc: &Arc<Mutex<HashMap<String, JobProcessingState>>>,
    planned_jobs: &[SpsPlannedJob],
    resolved_graph: &Arc<ResolvedGraph>,
    worker_job_tx: &crossbeam_channel::Sender<WorkerJob>,
    // runner_event_tx: &broadcast::Sender<PipelineEvent>, // Removed for now
    // config: &Config, // Removed for now
) {
    debug!("[Dispatch] Checking jobs for dispatch readiness...");
    let mut states_map = job_processing_states_arc.lock().unwrap();

    for p_job in planned_jobs {
        let job_id = &p_job.target_id;

        // Clone the path *before* matching on the state, to avoid borrow checker issues if we need to modify the state.
        let path_if_downloaded_or_waiting = match states_map.get(job_id) {
            Some(JobProcessingState::Downloaded(p)) | Some(JobProcessingState::WaitingForDependencies(p)) => Some(p.clone()),
            _ => None,
        };

        if let Some(current_job_state_entry_for_match) = states_map.get(job_id) {
            match current_job_state_entry_for_match {
                JobProcessingState::Downloaded(original_path) | JobProcessingState::WaitingForDependencies(original_path) => {
                    let is_formula = matches!(p_job.target_definition, InstallTargetIdentifier::Formula(_));
                    let path_to_use = original_path.clone(); // path_if_downloaded_or_waiting should be equivalent

                    if is_formula {
                        let mut all_deps_succeeded = true;
                        if let Some(resolved_node) = resolved_graph.resolution_details.get(job_id) {
                            // Iterate direct dependencies from the formula definition, but check against resolved graph context
                            for dep_edge in resolved_node.formula.dependencies().unwrap_or_default() {
                                if resolved_graph.context.should_process_dependency_edge(
                                    &resolved_node.formula, // parent formula
                                    dep_edge.tags,
                                    resolved_node.determined_install_strategy, // parent strategy
                                ) {
                                    match states_map.get(&dep_edge.name) {
                                        Some(JobProcessingState::Succeeded) => {} // Good
                                        _ => {
                                            all_deps_succeeded = false;
                                            break;
                                        }
                                    }
                                }
                            }
                        } else {
                            all_deps_succeeded = false; // Should not happen if graph is consistent
                            warn!("[Dispatch] Job {} not found in resolved_graph.resolution_details.", job_id);
                        }

                        if all_deps_succeeded {
                            debug!("[Dispatch] All dependencies for formula {} are Succeeded. Dispatching for installation.", job_id);
                            if let Some(state_to_update) = states_map.get_mut(job_id) {
                                *state_to_update = JobProcessingState::Installing;
                            }

                            let download_size_bytes = fs::metadata(&path_to_use).map(|m| m.len()).unwrap_or(0);
                            let worker_job = WorkerJob {
                                request: p_job.clone(),
                                download_path: path_to_use,
                                download_size_bytes,
                                is_source_from_private_store: p_job.use_private_store_source.is_some(),
                            };
                            if let Err(e) = worker_job_tx.send(worker_job) {
                                error!("[Dispatch] Failed to send WorkerJob for {}: {}. Core pool might have shut down.", job_id, e);
                                // If sending fails, we might need to revert state or mark as failed.
                                // For now, just log. The job will remain 'Installing' but won't be processed.
                            }
                        } else if matches!(current_job_state_entry_for_match, JobProcessingState::Downloaded(_)) {
                            // Only transition from Downloaded to WaitingForDependencies if not already Waiting
                            debug!("[Dispatch] Dependencies for formula {} not yet Succeeded. Moving to WaitingForDependencies.", job_id);
                             if let Some(state_to_update) = states_map.get_mut(job_id) {
                                *state_to_update = JobProcessingState::WaitingForDependencies(path_to_use);
                            }
                        }
                    } else { // It's a Cask
                        debug!("[Dispatch] Cask {} is Downloaded. Dispatching for installation.", job_id);
                        if let Some(state_to_update) = states_map.get_mut(job_id) {
                            *state_to_update = JobProcessingState::Installing;
                        }

                        let download_size_bytes = fs::metadata(&path_to_use).map(|m| m.len()).unwrap_or(0);
                        let worker_job = WorkerJob {
                            request: p_job.clone(),
                            download_path: path_to_use,
                            download_size_bytes,
                            is_source_from_private_store: p_job.use_private_store_source.is_some(),
                        };
                        if let Err(e) = worker_job_tx.send(worker_job) {
                           error!("[Dispatch] Failed to send WorkerJob for cask {}: {}. Core pool might have shut down.", job_id, e);
                        }
                    }
                }
                _ => { /* Not in a state to be dispatched (e.g. PendingDownload, Installing, Succeeded, Failed) */ }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn propagate_failure(
    initial_failed_job_id: &str,
    job_processing_states_arc: &Arc<Mutex<HashMap<String, JobProcessingState>>>,
    resolved_graph: &Arc<ResolvedGraph>,
    runner_event_tx: &broadcast::Sender<PipelineEvent>,
    final_fail_count: &Arc<AtomicUsize>,
    completed_jobs_count: &mut usize,
    planned_jobs_map: &HashMap<String, &SpsPlannedJob>,
) {
    debug!("[FailurePropagation] Starting propagation for failure of: {}", initial_failed_job_id);
    let mut states_map = job_processing_states_arc.lock().unwrap();
    let mut queue = VecDeque::new();
    let mut visited_for_propagation = HashSet::new();

    queue.push_back(initial_failed_job_id.to_string());

    while let Some(current_failed_id) = queue.pop_front() {
        if !visited_for_propagation.insert(current_failed_id.clone()) {
            continue; // Already processed this failure source
        }

        debug!("[FailurePropagation] Processing failure source: {}", current_failed_id);

        // Find jobs that depend on current_failed_id
        for potential_dependent_job_detail in resolved_graph.resolution_details.values() {
            let dependent_id = &potential_dependent_job_detail.formula.name;

            if dependent_id == &current_failed_id { // Cannot depend on itself in this context
                continue;
            }

            // Check if potential_dependent_job_detail actually depends on current_failed_id
            let mut is_true_dependent = false;
            if let Ok(dependencies) = potential_dependent_job_detail.formula.dependencies() {
                for dep_edge in dependencies {
                    if dep_edge.name == current_failed_id &&
                       resolved_graph.context.should_process_dependency_edge(
                            &potential_dependent_job_detail.formula,
                            dep_edge.tags,
                            potential_dependent_job_detail.determined_install_strategy) {
                        is_true_dependent = true;
                        break;
                    }
                }
            }

            if is_true_dependent {
                // Check current state of the dependent job
                let dependent_current_state = states_map.get(dependent_id).cloned();

                if !matches!(dependent_current_state, Some(JobProcessingState::Succeeded) | Some(JobProcessingState::Failed(_))) {
                    let failure_reason = format!("Dependency '{}' failed.", current_failed_id);
                    info!(
                        "[FailurePropagation] Propagating failure from {} to dependent {}. Marking as Failed. Reason: {}",
                        current_failed_id, dependent_id, failure_reason
                    );

                    states_map.insert(dependent_id.clone(), JobProcessingState::Failed(failure_reason.clone()));
                    final_fail_count.fetch_add(1, Ordering::Relaxed);
                    *completed_jobs_count += 1; // This dependent job is now also terminal

                    if let Some(planned_job_for_dependent) = planned_jobs_map.get(dependent_id) {
                        runner_event_tx.send(PipelineEvent::JobFailed {
                            target_id: dependent_id.clone(),
                            action: planned_job_for_dependent.action.clone(),
                            error: failure_reason,
                        }).ok();
                    } else {
                        // This case should ideally not happen if planned_jobs covers all resolved dependencies.
                        // If it does, create a generic JobAction or handle appropriately.
                        warn!("[FailurePropagation] Could not find PlannedJob details for {} to emit JobFailed event.", dependent_id);
                        runner_event_tx.send(PipelineEvent::JobFailed {
                            target_id: dependent_id.clone(),
                            action: JobAction::Install, // Fallback action
                            error: failure_reason,
                        }).ok();
                    }
                    queue.push_back(dependent_id.clone()); // Propagate this new failure
                } else {
                     debug!("[FailurePropagation] Dependent {} of {} is already in a terminal state ({:?}). No propagation.", dependent_id, current_failed_id, dependent_current_state);
                }
            }
        }
    }
}


// Helper function to determine PipelinePackageType from InstallTargetIdentifier
fn get_pipeline_package_type(target_def: &InstallTargetIdentifier) -> PipelinePackageType {
    match target_def {
        InstallTargetIdentifier::Formula(_) => PipelinePackageType::Formula,
        InstallTargetIdentifier::Cask(_) => PipelinePackageType::Cask,
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
