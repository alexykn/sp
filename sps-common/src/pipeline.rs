// sps-common/src/pipeline.rs
use std::path::PathBuf;
use std::sync::Arc; // Required for Arc<SpsError> in JobProcessingState

use serde::{Deserialize, Serialize};

use crate::dependency::ResolvedGraph; // Needed for planner output
use crate::error::SpsError;
use crate::model::InstallTargetIdentifier;

// --- Shared Enums / Structs ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PipelinePackageType {
    Formula,
    Cask,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)] // Added PartialEq, Eq
pub enum JobAction {
    Install,
    Upgrade {
        from_version: String,
        old_install_path: PathBuf,
    },
    Reinstall {
        version: String,
        current_install_path: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct PlannedJob {
    pub target_id: String,
    pub target_definition: InstallTargetIdentifier,
    pub action: JobAction,
    pub is_source_build: bool,
    pub use_private_store_source: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct WorkerJob {
    pub request: PlannedJob,
    pub download_path: PathBuf,
    pub download_size_bytes: u64,
    pub is_source_from_private_store: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PipelineEvent {
    PipelineStarted {
        total_jobs: usize,
    },
    PipelineFinished {
        duration_secs: f64,
        success_count: usize,
        fail_count: usize,
    },
    PlanningStarted,
    DependencyResolutionStarted,
    DependencyResolutionFinished,
    PlanningFinished {
        job_count: usize,
        // Optionally, we can pass the ResolvedGraph here if the status handler needs it,
        // but it might be too large for a broadcast event.
        // resolved_graph: Option<Arc<ResolvedGraph>>, // Example
    },
    DownloadStarted {
        target_id: String,
        url: String,
    },
    DownloadFinished {
        target_id: String,
        path: PathBuf,
        size_bytes: u64,
    },
    DownloadFailed {
        target_id: String,
        url: String,
        error: String, // Keep as String for simplicity in events
    },
    JobProcessingStarted {
        // From core worker
        target_id: String,
    },
    JobDispatchedToCore {
        // New: From runner to UI when job sent to worker pool
        target_id: String,
    },
    UninstallStarted {
        target_id: String,
        version: String,
    },
    UninstallFinished {
        target_id: String,
        version: String,
    },
    BuildStarted {
        target_id: String,
    },
    InstallStarted {
        target_id: String,
        pkg_type: PipelinePackageType,
    },
    LinkStarted {
        target_id: String,
        pkg_type: PipelinePackageType,
    },
    JobSuccess {
        // From core worker
        target_id: String,
        action: JobAction,
        pkg_type: PipelinePackageType,
    },
    JobFailed {
        // From core worker or runner (propagated)
        target_id: String,
        action: JobAction, // Action that was attempted
        error: String,     // Keep as String
    },
    LogInfo {
        message: String,
    },
    LogWarn {
        message: String,
    },
    LogError {
        message: String,
    },
}

impl PipelineEvent {
    // SpsError kept for internal use, but events use String for error messages
    pub fn job_failed(target_id: String, action: JobAction, error: &SpsError) -> Self {
        PipelineEvent::JobFailed {
            target_id,
            action,
            error: error.to_string(),
        }
    }
    pub fn download_failed(target_id: String, url: String, error: &SpsError) -> Self {
        PipelineEvent::DownloadFailed {
            target_id,
            url,
            error: error.to_string(),
        }
    }
}

// --- New Structs and Enums for Refactored Runner ---

/// Represents the current processing state of a job in the pipeline.
#[derive(Debug, Clone)]
pub enum JobProcessingState {
    /// Waiting for download to be initiated.
    PendingDownload,
    /// Download is in progress (managed by DownloadCoordinator).
    Downloading,
    /// Download completed successfully, artifact at PathBuf.
    Downloaded(PathBuf),
    /// Downloaded, but waiting for dependencies to be in Succeeded state.
    WaitingForDependencies(PathBuf),
    /// Dispatched to the core worker pool for installation/processing.
    DispatchedToCore(PathBuf),
    /// Installation/processing is in progress by a core worker.
    Installing(PathBuf), // Path is still relevant
    /// Job completed successfully.
    Succeeded,
    /// Job failed. The String contains the error message. Arc for cheap cloning.
    Failed(Arc<SpsError>),
}

/// Outcome of a download attempt, sent from DownloadCoordinator to the main runner loop.
#[derive(Debug)] // Clone not strictly needed if moved
pub struct DownloadOutcome {
    pub planned_job: PlannedJob,           // The job this download was for
    pub result: Result<PathBuf, SpsError>, // Path to downloaded file or error
}

/// Structure returned by the planner, now including the ResolvedGraph.
#[derive(Debug, Default)]
pub struct PlannedOperations {
    pub jobs: Vec<PlannedJob>,           // Topologically sorted for formulae
    pub errors: Vec<(String, SpsError)>, // Errors from planning phase
    pub already_installed_or_up_to_date: std::collections::HashSet<String>,
    pub resolved_graph: Option<Arc<ResolvedGraph>>, // Graph for dependency checking in runner
}
