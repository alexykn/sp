// sps-common/src/pipeline.rs
// REMOVE: use sps_core::PackageType; // <<-- REMOVE THIS
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::SpsError;
use crate::model::InstallTargetIdentifier; // Add serde if serialization might be needed later

// --- Shared Enums / Structs ---

/// Simple enum to represent package type within pipeline events, independent of sps-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PipelinePackageType {
    Formula,
    Cask,
}

/// Defines the specific operation type for a pipeline job.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

// --- Job Planning DTO (Internal to CLI Runner Planning) ---
#[derive(Debug, Clone)] // Not necessarily needed across boundary if WorkerJob is used
pub struct PlannedJob {
    pub target_id: String,
    pub target_definition: InstallTargetIdentifier, // Keep Arc<...> inside
    pub action: JobAction,
    pub is_source_build: bool,
    pub use_private_store_source: Option<PathBuf>,
}

// --- Worker Job DTO (CLI Downloader -> Core Worker Pool via Crossbeam) ---
#[derive(Debug, Clone)] // Clone needed for dispatching
pub struct WorkerJob {
    pub request: PlannedJob,
    pub download_path: PathBuf,
    pub download_size_bytes: u64,
    pub is_source_from_private_store: bool,
}

// --- Pipeline Event (Core Worker Pool/CLI Runner -> CLI Status via Broadcast) ---
#[derive(Debug, Clone, Serialize, Deserialize)] // Add Serialize/Deserialize if needed
pub enum PipelineEvent {
    // Overall Status
    PipelineStarted {
        total_jobs: usize,
    },
    PipelineFinished {
        duration_secs: f64,
        success_count: usize,
        fail_count: usize,
    },

    // Planning Status (Emitted by CLI Runner)
    PlanningStarted,
    DependencyResolutionStarted,
    DependencyResolutionFinished,
    PlanningFinished {
        job_count: usize,
    },

    // Download Status (Emitted by CLI Runner)
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
        error: String,
    },

    // Core Worker Job Status (Emitted by Core Workers)
    JobProcessingStarted {
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
        pkg_type: PipelinePackageType, // Use the common enum
    },
    LinkStarted {
        target_id: String,
        pkg_type: PipelinePackageType, // Use the common enum
    },
    JobSuccess {
        target_id: String,
        action: JobAction,
        pkg_type: PipelinePackageType, // Use the common enum
    },
    JobFailed {
        target_id: String,
        action: JobAction,
        error: String,
    },

    // General Logs
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

// Helpers remain the same, using SpsError from sps_common
impl PipelineEvent {
    pub fn job_failed(target_id: String, action: JobAction, error: SpsError) -> Self {
        PipelineEvent::JobFailed {
            target_id,
            action,
            error: error.to_string(),
        }
    }
    pub fn download_failed(target_id: String, url: String, error: SpsError) -> Self {
        PipelineEvent::DownloadFailed {
            target_id,
            url,
            error: error.to_string(),
        }
    }
}

// --- Download Outcome ---
/// Represents the outcome of a download attempt for a planned job.
#[derive(Debug)]
pub struct DownloadOutcome {
    /// The job that was planned for download.
    pub planned_job: PlannedJob,
    /// The result of the download: Ok(PathBuf) on success, or Err(SpsError) on failure.
    pub result: Result<PathBuf, SpsError>,
}
