// sps-core/src/pipeline/engine.rs
//! Manages the worker thread pool and dispatches jobs received via crossbeam channel.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_channel::Receiver as CrossbeamReceiver; // Receive WorkerJobs
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::Result as SpsResult;
// Use shared types, including WorkerJob now
use sps_common::pipeline::{PipelineEvent, WorkerJob};
use threadpool::ThreadPool; // Use the threadpool crate as requested
use tokio::sync::broadcast; // Still use broadcast Sender for events
use tracing::{debug, error, instrument, warn};

use super::worker; // The module doing the actual work

/// Starts the core worker pool manager. This function is synchronous
/// and designed to be run in its own dedicated thread.
/// It blocks listening for jobs on the crossbeam channel.
/// Returns Ok(()) on graceful shutdown, Err on internal error.
#[instrument(skip_all, name = "core_worker_manager")]
pub fn start_worker_pool_manager(
    config: Config,
    cache: Arc<Cache>,
    worker_job_rx: CrossbeamReceiver<WorkerJob>, // Receive WorkerJobs synchronously
    event_tx: broadcast::Sender<PipelineEvent>,  // Send events asynchronously
    success_count: Arc<AtomicUsize>,             // Track success across workers
    fail_count: Arc<AtomicUsize>,                // Track failure across workers
) -> SpsResult<()> {
    // Indicate success/failure of the manager itself
    let num_workers = std::cmp::max(1, num_cpus::get_physical().saturating_sub(1)).min(6);
    let pool = ThreadPool::new(num_workers);
    debug!(
        "Core worker pool manager started with {} workers.",
        num_workers
    );
    debug!("Worker pool created.");

    // Loop receiving jobs from the CLI downloader via crossbeam channel
    // This loop blocks the dedicated thread until the channel is closed and empty.
    for worker_job in worker_job_rx {
        let job_id = worker_job.request.target_id.clone();
        debug!(
            "[{}] Received job from channel, preparing to submit to pool.",
            job_id
        );

        let config_clone = config.clone();
        let cache_clone = Arc::clone(&cache);
        let event_tx_clone = event_tx.clone();
        let success_count_clone = Arc::clone(&success_count);
        let fail_count_clone = Arc::clone(&fail_count);

        // Send JobProcessingStarted event before handing off to pool
        let _ = event_tx_clone.send(PipelineEvent::JobProcessingStarted {
            target_id: job_id.clone(),
        });

        debug!("[{}] Submitting job to worker pool.", job_id);

        pool.execute(move || {
            // This closure runs in a worker thread from the pool

            // --- Execute Synchronous Job Steps ---
            let job_result = worker::execute_sync_job(
                worker_job, // Pass the WorkerJob DTO
                &config_clone,
                cache_clone,
                event_tx_clone.clone(), // Pass broadcast sender for worker events
            );
            let job_id_for_log = job_id.clone();
            debug!(
                "[{}] Worker job execution finished (execute_sync_job returned), result ok: {}",
                job_id_for_log,
                job_result.is_ok()
            );

            // --- Report Final Job Status ---
            let job_id_for_log = job_id.clone();
            match job_result {
                Ok((final_action, pkg_type)) => {
                    success_count_clone.fetch_add(1, Ordering::Relaxed);
                    debug!("[{}] Worker finished successfully.", job_id);
                    let _ = event_tx_clone.send(PipelineEvent::JobSuccess {
                        target_id: job_id,
                        action: final_action,
                        pkg_type,
                    });
                    debug!("[{}] Worker result event sent.", job_id_for_log);
                }
                Err(boxed) => {
                    let (final_action, err) = *boxed;
                    fail_count_clone.fetch_add(1, Ordering::Relaxed);
                    error!("[{}] Worker failed: {}", job_id, err);
                    let _ = event_tx_clone.send(PipelineEvent::JobFailed {
                        target_id: job_id,
                        action: final_action,
                        error: err.to_string(),
                    });
                    debug!("[{}] Worker result event sent.", job_id_for_log);
                }
            }
            debug!("[{}] Worker closure scope ending.", job_id_for_log);
        });
    }

    // Receiver loop finished, means the channel was closed by the sender (CLI runner)
    debug!("Worker job channel closed. Waiting for active workers to finish...");
    debug!("Calling pool.join() to wait for all worker threads to finish...");
    pool.join(); // Wait for all threads in the pool to complete their current task
    debug!("pool.join() returned, all worker threads should be finished.");
    debug!("Core worker pool joined. Engine thread finishing.");
    debug!("Core worker pool manager finished.");

    // Dropping the original event_tx sender happens when the start_core_pipeline task ends (in CLI
    // runner). Clones held by workers are dropped when they finish.

    Ok(()) // Manager thread finished gracefully
}
