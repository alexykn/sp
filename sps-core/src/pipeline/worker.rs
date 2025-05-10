use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::{Result as SpsResult, SpsError};
use sps_common::model::formula::FormulaDependencies;
use sps_common::model::InstallTargetIdentifier;
use sps_common::pipeline::{JobAction, PipelineEvent, PipelinePackageType, WorkerJob};
use tokio::sync::broadcast;
use tracing::{debug, instrument, warn};

use crate::installed::{InstalledPackageInfo, PackageType as CorePackageType};
use crate::{build, uninstall};

pub(super) fn execute_sync_job(
    worker_job: WorkerJob,
    config: &Config,
    cache: Arc<Cache>,
    event_tx: broadcast::Sender<PipelineEvent>,
) -> std::result::Result<(JobAction, PipelinePackageType), Box<(JobAction, SpsError)>> {
    let action = worker_job.request.action.clone();

    let result = do_execute_sync_steps(worker_job, config, cache, event_tx);

    result
        .map_err(|e| Box::new((action.clone(), e)))
        .map(|pkg_type| (action, pkg_type))
}

#[instrument(skip_all, fields(job_id = %worker_job.request.target_id, action = ?worker_job.request.action))]
fn do_execute_sync_steps(
    worker_job: WorkerJob,
    config: &Config,
    _cache: Arc<Cache>,
    event_tx: broadcast::Sender<PipelineEvent>,
) -> SpsResult<PipelinePackageType> {
    let job_request = worker_job.request;
    let download_path = worker_job.download_path;

    let (core_pkg_type, pipeline_pkg_type) = match &job_request.target_definition {
        InstallTargetIdentifier::Formula(_) => {
            (CorePackageType::Formula, PipelinePackageType::Formula)
        }
        InstallTargetIdentifier::Cask(_) => (CorePackageType::Cask, PipelinePackageType::Cask),
    };

    if let JobAction::Upgrade {
        from_version,
        old_install_path,
    }
    | JobAction::Reinstall {
        version: from_version,
        current_install_path: old_install_path,
    } = &job_request.action
    {
        debug!(
            "[{}] Removing existing version {}...",
            job_request.target_id, from_version
        );
        let _ = event_tx.send(PipelineEvent::UninstallStarted {
            target_id: job_request.target_id.clone(),
            version: from_version.clone(),
        });

        let old_info = InstalledPackageInfo {
            name: job_request.target_id.clone(),
            version: from_version.clone(),
            pkg_type: core_pkg_type.clone(),
            path: old_install_path.clone(),
        };
        let uninstall_opts = uninstall::UninstallOptions { skip_zap: true };

        match core_pkg_type {
            CorePackageType::Formula => {
                uninstall::uninstall_formula_artifacts(&old_info, config, &uninstall_opts)?
            }
            CorePackageType::Cask => uninstall::uninstall_cask_artifacts(&old_info, config)?,
        }

        debug!(
            "[{}] Removed existing version {}.",
            job_request.target_id, from_version
        );
        let _ = event_tx.send(PipelineEvent::UninstallFinished {
            target_id: job_request.target_id.clone(),
            version: from_version.clone(),
        });
    }

    let _ = event_tx.send(PipelineEvent::InstallStarted {
        target_id: job_request.target_id.clone(),
        pkg_type: pipeline_pkg_type,
    });

    let mut formula_installed_path: Option<PathBuf> = None;

    match &job_request.target_definition {
        InstallTargetIdentifier::Formula(formula) => {
            let install_dir_base = formula.install_prefix(&config.cellar)?;
            if let Some(parent_dir) = install_dir_base.parent() {
                fs::create_dir_all(parent_dir).map_err(|e| SpsError::Io(Arc::new(e)))?;
            }

            if job_request.is_source_build {
                debug!("[{}] Building from source...", job_request.target_id);
                let _ = event_tx.send(PipelineEvent::BuildStarted {
                    target_id: job_request.target_id.clone(),
                });
                let build_dep_paths: Vec<PathBuf> = vec![];

                let build_future = build::formula::source::build_from_source(
                    &download_path,
                    formula,
                    config,
                    &build_dep_paths,
                );
                let installed_dir = block_on(build_future)?;
                formula_installed_path = Some(installed_dir);
            } else {
                debug!("[{}] Installing bottle...", job_request.target_id);
                let installed_dir =
                    build::formula::bottle::install_bottle(&download_path, formula, config)?;
                formula_installed_path = Some(installed_dir);
            }
        }
        InstallTargetIdentifier::Cask(cask) => {
            debug!("[{}] Installing cask...", job_request.target_id);
            build::cask::install_cask(cask, &download_path, config)?;
        }
    };

    if let Some(ref installed_path) = formula_installed_path {
        debug!(
            "[{}] Formula Installed/Built to: {}",
            job_request.target_id,
            installed_path.display()
        );
    } else if core_pkg_type == CorePackageType::Cask {
        debug!("[{}] Cask installation completed.", job_request.target_id);
    }

    if let (InstallTargetIdentifier::Formula(formula), Some(installed_path)) =
        (&job_request.target_definition, &formula_installed_path)
    {
        debug!("[{}] Linking artifacts...", job_request.target_id);
        let _ = event_tx.send(PipelineEvent::LinkStarted {
            target_id: job_request.target_id.clone(),
            pkg_type: pipeline_pkg_type,
        });

        build::formula::link::link_formula_artifacts(formula, installed_path, config)?;

        debug!("[{}] Linking complete.", job_request.target_id);
    }

    Ok(pipeline_pkg_type)
}
