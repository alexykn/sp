use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_channel::Receiver as CrossbeamReceiver;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::Result as SpsResult;
use sps_common::pipeline::{PipelineEvent, WorkerJob};
use threadpool::ThreadPool;
use tokio::sync::broadcast;
use tracing::{debug, instrument};

use super::worker;

#[instrument(skip_all, name = "core_worker_manager")]
pub fn start_worker_pool_manager(
    config: Config,
    cache: Arc<Cache>,
    worker_job_rx: CrossbeamReceiver<WorkerJob>,
    event_tx: broadcast::Sender<PipelineEvent>,
    success_count: Arc<AtomicUsize>,
    fail_count: Arc<AtomicUsize>,
) -> SpsResult<()> {
    let num_workers = std::cmp::max(1, num_cpus::get_physical().saturating_sub(1)).min(6);
    let pool = ThreadPool::new(num_workers);
    debug!(
        "Core worker pool manager started with {} workers.",
        num_workers
    );
    debug!("Worker pool created.");

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

        let _ = event_tx_clone.send(PipelineEvent::JobProcessingStarted {
            target_id: job_id.clone(),
        });

        debug!("[{}] Submitting job to worker pool.", job_id);

        pool.execute(move || {
            let job_result = worker::execute_sync_job(
                worker_job,
                &config_clone,
                cache_clone,
                event_tx_clone.clone(),
            );
            let job_id_for_log = job_id.clone();
            debug!(
                "[{}] Worker job execution finished (execute_sync_job returned), result ok: {}",
                job_id_for_log,
                job_result.is_ok()
            );

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
                    // Error is sent via JobFailed event and displayed in status.rs
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
    pool.join();
    Ok(())
}
