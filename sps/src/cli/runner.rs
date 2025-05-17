use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::slice; // added for slice::from_ref usage
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use colored::Colorize;
use crossbeam_channel::{bounded, Sender as CrossbeamSender};
use reqwest::Client;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::dependency::{
    DependencyResolver, ResolutionContext, ResolutionStatus, ResolvedGraph,
};
use sps_common::error::{Result, SpsError};
use sps_common::formulary::Formulary;
use sps_common::keg::KegRegistry;
use sps_common::model::formula::Formula;
use sps_common::model::{Cask, InstallTargetIdentifier};
use sps_common::pipeline::{JobAction, PipelineEvent, PlannedJob, WorkerJob};
use sps_core::build;
use sps_core::installed::{self};
use sps_core::update_check::{self, UpdateInfo};
use sps_net::{api, UrlField};
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tracing::{debug, error, instrument, warn};

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
) -> Result<()> {
    let start_time = Instant::now();
    let final_success_count = Arc::new(AtomicUsize::new(0));
    let final_fail_count = Arc::new(AtomicUsize::new(0)); // Tracks download + worker failures

    let (worker_job_tx, worker_job_rx) = bounded::<WorkerJob>(WORKER_JOB_CHANNEL_SIZE);
    let (event_tx, _event_rx) = broadcast::channel::<PipelineEvent>(EVENT_CHANNEL_SIZE);
    debug!("event_tx created.");
    let runner_event_tx = event_tx.clone();
    debug!("runner_event_tx cloned from event_tx.");

    let core_config = config.clone();
    let core_cache = cache.clone();
    let core_event_tx = event_tx.clone();
    debug!("core_event_tx cloned from event_tx.");
    let core_success_count = Arc::clone(&final_success_count);
    let core_fail_count = Arc::clone(&final_fail_count);
    let core_handle = std::thread::spawn(move || {
        sps_core::pipeline::engine::start_worker_pool_manager(
            core_config,
            core_cache,
            worker_job_rx,
            core_event_tx,
            core_success_count,
            core_fail_count,
        )
    });
    debug!("Core worker pool manager thread spawned.");

    let status_config = config.clone();
    let status_event_rx = event_tx.subscribe();
    let status_handle = tokio::spawn(super::status::handle_events(status_config, status_event_rx));
    debug!("CLI status handler task spawned.");

    runner_event_tx.send(PipelineEvent::PlanningStarted).ok();
    debug!("Planning package operations...");
    let plan_result = plan_operations(
        initial_targets,
        command_type.clone(),
        config,
        cache.clone(),
        flags,
        runner_event_tx.clone(),
    )
    .await;
    let (planned_jobs, mut overall_errors, already_installed) = match plan_result {
        Ok(res) => res,
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
                    join_err
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
            if let Err(join_err) = status_handle.await {
                error!(
                    "Status task join error after planning failure: {}",
                    join_err
                );
            }
            return Err(e);
        }
    };
    runner_event_tx
        .send(PipelineEvent::PlanningFinished {
            job_count: planned_jobs.len(),
        })
        .ok();
    for name in already_installed {
        let msg = format!("{} {} is already installed.", "✓".green(), name.cyan());
        debug!("{}", msg);
    }
    for (name, err) in &overall_errors {
        let msg = format!("Error during planning for '{}': {}", name.cyan(), err);
        error!("✖ {}", msg);
        runner_event_tx
            .send(PipelineEvent::LogError { message: msg })
            .ok();
    }

    let total_jobs = planned_jobs.len();
    if total_jobs == 0 {
        if overall_errors.is_empty() {
            let msg = "No packages need to be installed, upgraded, or reinstalled.";
            debug!("{}", msg);
            runner_event_tx
                .send(PipelineEvent::LogInfo {
                    message: msg.into(),
                })
                .ok();
        } else {
            let msg = "No operations possible due to planning errors.";
            error!("{}", msg);
            runner_event_tx
                .send(PipelineEvent::LogError {
                    message: msg.into(),
                })
                .ok();
        }
        runner_event_tx
            .send(PipelineEvent::PipelineStarted { total_jobs: 0 })
            .ok();
        drop(worker_job_tx);
    } else {
        debug!("Starting download phase for {} jobs...", total_jobs);
        runner_event_tx
            .send(PipelineEvent::PipelineStarted { total_jobs })
            .ok();

        let http_client = Arc::new(Client::new());

        let download_errors = coordinate_downloads(
            planned_jobs,
            config,
            cache.clone(),
            http_client,
            worker_job_tx.clone(),
            runner_event_tx.clone(),
        )
        .await;

        let download_fail_count = download_errors.len();
        final_fail_count.fetch_add(download_fail_count, Ordering::Relaxed);
        overall_errors.extend(download_errors);
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

    debug!("Waiting for core worker pool manager thread to finish...");
    match core_handle.join() {
        Ok(Ok(())) => {
            debug!("Core worker pool manager thread completed.");
        }
        Ok(Err(e)) => {
            error!("Core worker pool manager thread failed: {}", e);
            overall_errors.push(("[Core Manager]".to_string(), e));
        }
        Err(e) => {
            error!("Core worker pool manager thread panicked: {:?}", e);
            let panic_msg = match e.downcast_ref::<&'static str>() {
                Some(s) => *s,
                None => match e.downcast_ref::<String>() {
                    Some(s) => s.as_str(),
                    None => "Unknown panic payload",
                },
            };
            overall_errors.push((
                "[Core Manager]".to_string(),
                SpsError::Generic(format!("Core thread panicked: {panic_msg}")),
            ));
        }
    }

    debug!("Waiting for status handler task to finish...");
    debug!("Dropping runner_event_tx.");
    drop(runner_event_tx);
    debug!(
        "event_tx receiver_count after dropping runner_event_tx: {}",
        event_tx.receiver_count()
    );
    debug!(
        "event_tx len after dropping runner_event_tx: {}",
        event_tx.len()
    );
    debug!("Dropping event_tx explicitly.");
    drop(event_tx);
    debug!("event_tx dropped explicitly.");
    debug!("runner_event_tx dropped.");
    if let Err(e) = status_handle.await {
        warn!("Status handler task failed or panicked: {}", e);
        debug!("status_handle.await returned with error.");
    } else {
        debug!("Status handler task completed.");
        debug!("status_handle.await returned successfully.");
    }

    let duration = start_time.elapsed();
    let final_success = final_success_count.load(Ordering::Relaxed);
    let final_fail = final_fail_count.load(Ordering::Relaxed);
    debug!(
        "Pipeline duration: {:.3}s. Success: {}, Fail: {}",
        duration.as_secs_f64(),
        final_success,
        final_fail
    );

    let total_failures = final_fail + overall_errors.len();
    if total_failures == 0 {
        debug!(
            "{}",
            format!("Pipeline execution completed successfully ({final_success} tasks).").green()
        );
        debug!("run_pipeline function returning Ok(())");
        Ok(())
    } else {
        error!(
            "Pipeline execution completed with {} failure(s).",
            total_failures
        );
        let specific_error_msg = overall_errors
            .into_iter()
            .map(|(n, e)| format!("'{n}': {e}"))
            .collect::<Vec<_>>()
            .join("; ");
        debug!("run_pipeline function returning Err(SpsError::InstallError)");
        Err(SpsError::InstallError(format!(
            "Operation failed with {total_failures} total failure(s). Specific errors: [{specific_error_msg}]"
        )))
    }
}

type PlanResult = Result<(Vec<PlannedJob>, Vec<(String, SpsError)>, HashSet<String>)>;

#[instrument(skip_all, fields(cmd = ?command_type))]
async fn plan_operations(
    initial_targets: &[String],
    command_type: CommandType,
    config: &Config,
    cache: Arc<Cache>, // Accepts Arc
    flags: &PipelineFlags,
    event_tx: broadcast::Sender<PipelineEvent>,
) -> PlanResult {
    let mut errors: Vec<(String, SpsError)> = Vec::new();
    let mut already_installed: HashSet<String> = HashSet::new();
    let mut processed: HashSet<String> = HashSet::new();
    let mut initial_ops: HashMap<String, (JobAction, Option<InstallTargetIdentifier>)> =
        HashMap::new();
    let mut private_store_sources: HashMap<String, PathBuf> = HashMap::<String, PathBuf>::new();

    match command_type {
        CommandType::Install => {
            debug!("Planning for INSTALL command");
            for name in initial_targets {
                if processed.contains(name) {
                    continue;
                }
                match installed::get_installed_package(name, config).await {
                    Ok(Some(_installed_info)) => {
                        already_installed.insert(name.clone());
                        processed.insert(name.clone());
                    }
                    Ok(None) => {
                        // --- CASK PRIVATE STORE REUSE LOGIC ---
                        // Try to find a private store bundle for this cask/version
                        // (This logic is similar to the Reinstall branch)
                        let cask_token_path = config.cask_room_token_path(name);
                        if cask_token_path.is_dir() {
                            if let Ok(version_entries_iter) = fs::read_dir(&cask_token_path) {
                                for version_entry in version_entries_iter.flatten() {
                                    let version_path = version_entry.path();
                                    if version_path.is_dir()
                                        && version_path.join("CASK_INSTALL_MANIFEST.json").is_file()
                                    {
                                        // Read manifest to get app name and check is_installed
                                        if let Ok(manifest_str) = fs::read_to_string(
                                            version_path.join("CASK_INSTALL_MANIFEST.json"),
                                        ) {
                                            if let Ok::<build::cask::CaskInstallManifest, _>(
                                                manifest,
                                            ) = serde_json::from_str(&manifest_str)
                                            {
                                                // Always reuse private store if manifest/app exist,
                                                // regardless of is_installed
                                                if let Some(app_name) =
                                                    manifest.primary_app_file_name
                                                {
                                                    let private_path = config.cask_store_app_path(
                                                        name,
                                                        &manifest.version,
                                                        &app_name,
                                                    );
                                                    if std::fs::metadata(&private_path).is_ok() {
                                                        tracing::debug!("(plan_operations) [INSTALL] Found app in private store for install: {}", private_path.display());
                                                        private_store_sources
                                                            .insert(name.clone(), private_path);
                                                    }
                                                }
                                                // Only treat as installed if is_installed is true
                                                if manifest.is_installed {
                                                    break; // Only check the first version found if
                                                           // installed
                                                }
                                            }
                                        }
                                        // If is_installed is false, do not break; allow install
                                    }
                                }
                            }
                        }
                        initial_ops.insert(name.clone(), (JobAction::Install, None));
                    }
                    Err(e) => {
                        errors.push((
                            name.clone(),
                            SpsError::Generic(format!(
                                "Failed to check installed status for {name}: {e}"
                            )),
                        ));
                        processed.insert(name.clone());
                    }
                }
            }
        }
        CommandType::Reinstall => {
            debug!("Planning for REINSTALL command");
            for name in initial_targets {
                if processed.contains(name) {
                    continue;
                }
                match installed::get_installed_package(name, config).await {
                    Ok(Some(installed_info)) => {
                        // For Casks, check if app exists in private store
                        if installed_info.pkg_type == installed::PackageType::Cask {
                            tracing::debug!(
                                "(plan_operations) [CASK] Checking for manifest at: {}",
                                installed_info
                                    .path
                                    .join("CASK_INSTALL_MANIFEST.json")
                                    .display()
                            );
                            // Try to read manifest to get app name
                            let manifest_path =
                                installed_info.path.join("CASK_INSTALL_MANIFEST.json");
                            if manifest_path.exists() {
                                tracing::debug!(
                                    "(plan_operations) Manifest exists at: {}",
                                    manifest_path.display()
                                );
                                match fs::read_to_string(&manifest_path) {
                                    Ok(manifest_str) => {
                                        tracing::debug!(
                                            "(plan_operations) Manifest read OK for {}: {} bytes",
                                            name,
                                            manifest_str.len()
                                        );
                                        match serde_json::from_str::<build::cask::CaskInstallManifest>(
                                            &manifest_str,
                                        ) {
                                            Ok(manifest) => {
                                                tracing::debug!("(plan_operations) Manifest JSON parsed OK for {}", name);
                                                if let Some(app_name) =
                                                    manifest.primary_app_file_name
                                                {
                                                    tracing::debug!("(plan_operations) Manifest primary_app_file_name: '{}'", app_name);
                                                    let private_path = config.cask_store_app_path(
                                                        &installed_info.name,
                                                        &installed_info.version,
                                                        &app_name,
                                                    );
                                                    tracing::debug!("(plan_operations) Built private store path: {}", private_path.display());
                                                    // Accept path even if it differs only by case
                                                    // or NFC/NFD.
                                                    // Use metadata().is_ok() for compatibility.
                                                    if std::fs::metadata(&private_path).is_ok() {
                                                        tracing::debug!("(plan_operations) Found app in private store for reinstall: {}", private_path.display());
                                                        private_store_sources
                                                            .insert(name.clone(), private_path);
                                                    } else {
                                                        tracing::warn!("(plan_operations) Cask {}: private store bundle not found at {}", name, private_path.display());
                                                        // Extra: enumerate directory contents for
                                                        // debugging
                                                        if let Some(parent) = private_path.parent()
                                                        {
                                                            match std::fs::read_dir(parent) {
                                                                Ok(entries) => {
                                                                    tracing::debug!("(plan_operations) Directory listing for {}:", parent.display());
                                                                    for entry in entries.flatten() {
                                                                        tracing::debug!("(plan_operations)   - {}", entry.path().display());
                                                                    }
                                                                }
                                                                Err(e) => {
                                                                    tracing::warn!("(plan_operations) Could not list directory {}: {}", parent.display(), e);
                                                                }
                                                            }
                                                        }
                                                    }
                                                } else {
                                                    tracing::warn!("(plan_operations) Manifest for {} has no primary_app_file_name", name);
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!("(plan_operations) Failed to parse manifest JSON for {}: {}", name, e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "(plan_operations) Failed to read manifest for {}: {}",
                                            name,
                                            e
                                        );
                                    }
                                }
                            } else {
                                tracing::warn!(
                                    "(plan_operations) Manifest does not exist at: {}",
                                    manifest_path.display()
                                );
                            }
                        }

                        initial_ops.insert(
                            name.clone(),
                            (
                                JobAction::Reinstall {
                                    version: installed_info.version.clone(),
                                    current_install_path: installed_info.path.clone(),
                                },
                                None,
                            ),
                        );
                        // Note: We've already added the path to private_store_sources if found
                        // It will be used during job creation after the target definition is
                        // resolved
                    }
                    Ok(None) => {
                        let msg = format!("Cannot reinstall '{name}': not installed.");
                        errors.push((name.clone(), SpsError::NotFound(msg)));
                        processed.insert(name.clone());
                    }
                    Err(e) => {
                        errors.push((
                            name.clone(),
                            SpsError::Generic(format!(
                                "Failed to check installed status for {name}: {e}"
                            )),
                        ));
                        processed.insert(name.clone());
                    }
                }
            }
        }
        CommandType::Upgrade { all } => {
            debug!("Planning for UPGRADE command");
            let packages_to_check = match if all {
                installed::get_installed_packages(config).await
            } else {
                let mut specific = Vec::new();
                for name in initial_targets {
                    match installed::get_installed_package(name, config).await {
                        Ok(Some(info)) => {
                            // For casks, skip upgrades if is_installed is false
                            if info.pkg_type == installed::PackageType::Cask {
                                let manifest_path = info.path.join("CASK_INSTALL_MANIFEST.json");
                                let mut skip_upgrade = false;
                                if manifest_path.is_file() {
                                    if let Ok(manifest_str) =
                                        std::fs::read_to_string(&manifest_path)
                                    {
                                        if let Ok(manifest_json) =
                                            serde_json::from_str::<serde_json::Value>(&manifest_str)
                                        {
                                            if let Some(is_installed) = manifest_json
                                                .get("is_installed")
                                                .and_then(|v| v.as_bool())
                                            {
                                                if !is_installed {
                                                    skip_upgrade = true;
                                                }
                                            }
                                        }
                                    }
                                }
                                if skip_upgrade {
                                    let msg = format!("Skipping upgrade for '{name}': not installed (is_installed=false).");
                                    warn!("! {}", msg);
                                    processed.insert(name.clone());
                                    continue;
                                }
                            }
                            specific.push(info)
                        }
                        Ok(None) => {
                            let msg = format!("Cannot upgrade '{name}': not installed.");
                            warn!("! {}", msg);
                            processed.insert(name.clone());
                        }
                        Err(e) => {
                            errors.push((
                                name.clone(),
                                SpsError::Generic(format!(
                                    "Failed to check installed status for {name}: {e}"
                                )),
                            ));
                            processed.insert(name.clone());
                        }
                    }
                }
                Ok(specific)
            } {
                Ok(pkgs) => pkgs,
                Err(e) => {
                    errors.push((
                        "<all>".to_string(),
                        SpsError::Generic(format!("Failed to get installed packages: {e}")),
                    ));
                    Vec::new()
                }
            };

            if packages_to_check.is_empty() {
                if all {
                    debug!("No installed packages found to check for upgrades.");
                }
                return Ok((vec![], errors, already_installed)); // Return empty jobs vec
            }

            match update_check::check_for_updates(&packages_to_check, &cache).await {
                Ok(updates) => {
                    let update_map: HashMap<String, UpdateInfo> =
                        updates.into_iter().map(|u| (u.name.clone(), u)).collect();
                    for installed in packages_to_check {
                        if processed.contains(&installed.name) {
                            continue;
                        }
                        if let Some(update_info) = update_map.get(&installed.name) {
                            initial_ops.insert(
                                installed.name.clone(),
                                (
                                    JobAction::Upgrade {
                                        from_version: installed.version.clone(),
                                        old_install_path: installed.path.clone(),
                                    },
                                    Some(update_info.target_definition.clone()),
                                ),
                            );
                            processed.insert(installed.name.clone());
                        } else {
                            already_installed.insert(installed.name.clone());
                            processed.insert(installed.name.clone());
                        }
                    }
                }
                Err(e) => {
                    errors.push((
                        "[Update Check]".to_string(),
                        SpsError::Generic(format!("Failed to check for updates: {e}")),
                    ));
                }
            }
        }
    }

    let definitions_to_fetch: Vec<String> = initial_ops
        .iter()
        .filter(|(_, (_, def))| def.is_none())
        .map(|(name, _)| name.clone())
        .collect();
    if !definitions_to_fetch.is_empty() {
        debug!(
            "Fetching definitions for {} initial targets...",
            definitions_to_fetch.len()
        );
        let fetched_defs = fetch_target_definitions(&definitions_to_fetch, cache.clone()).await; // Pass Arc
        for (name, result) in fetched_defs {
            match result {
                Ok(target_def) => {
                    if let Some((_, opt)) = initial_ops.get_mut(&name) {
                        *opt = Some(target_def);
                    }
                }
                Err(e) => {
                    let err_msg = format!(
                        "Failed to get definition for target '{}': {}",
                        name.cyan(),
                        e
                    );
                    error!("✖ {}", err_msg);
                    errors.push((name.clone(), SpsError::Generic(err_msg)));
                    initial_ops.remove(&name);
                    processed.insert(name.clone());
                }
            }
        }
    }

    event_tx
        .send(PipelineEvent::DependencyResolutionStarted)
        .ok();
    let mut formulae_for_resolution: HashMap<String, InstallTargetIdentifier> = HashMap::new();
    let mut cask_queue: VecDeque<String> = VecDeque::new();
    let mut cask_deps_map: HashMap<String, Arc<Cask>> = HashMap::new();
    for (name, (_action, opt_def)) in &initial_ops {
        match opt_def {
            Some(target @ InstallTargetIdentifier::Formula(_)) => {
                formulae_for_resolution.insert(name.clone(), target.clone());
            }
            Some(InstallTargetIdentifier::Cask(c_arc)) => {
                if !processed.contains(name) {
                    cask_queue.push_back(name.clone());
                    cask_deps_map.insert(name.clone(), c_arc.clone());
                }
            }
            None => {
                if !errors.iter().any(|(n, _)| n == name) {
                    let msg =
                        format!("Definition missing for target '{name}' after fetch attempt.");
                    error!("✖ {}", msg);
                    errors.push((name.clone(), SpsError::Generic(msg)));
                    processed.insert(name.clone());
                }
            }
        }
    }

    let mut processed_casks: HashSet<String> = initial_ops
        .keys()
        .filter(|k| {
            initial_ops.get(*k).is_some_and(|(_, d)| {
                d.as_ref()
                    .is_some_and(|def| matches!(def, InstallTargetIdentifier::Cask(_)))
            })
        })
        .cloned()
        .collect();
    processed_casks.extend(processed.iter().cloned());
    while let Some(token) = cask_queue.pop_front() {
        if processed_casks.contains(&token) {
            continue;
        }
        let cask_result = if let Some(c) = cask_deps_map.get(&token) {
            Ok(c.clone())
        } else {
            fetch_target_definitions(slice::from_ref(&token), cache.clone())
                .await
                .remove(&token)
                .map_or(
                    Err(SpsError::NotFound(format!(
                        "Cask def fetch failed for {token}"
                    ))),
                    |r| {
                        r.and_then(|def| match def {
                            InstallTargetIdentifier::Cask(c) => Ok(c),
                            _ => Err(SpsError::NotFound(format!("{token} is not a cask"))),
                        })
                    },
                )
        }; // Pass Arc
        let cask = match cask_result {
            Ok(c) => c,
            Err(e) => {
                if !errors.iter().any(|(n, _)| n == &token) {
                    errors.push((token.clone(), e));
                }
                processed_casks.insert(token.clone());
                continue;
            }
        };
        processed_casks.insert(token.clone());
        if let Some(deps) = &cask.depends_on {
            for formula_dep in &deps.formula {
                if !formulae_for_resolution.contains_key(formula_dep)
                    && !errors.iter().any(|(n, _)| n == formula_dep)
                    && !processed_casks.contains(formula_dep)
                {
                    match fetch_target_definitions(slice::from_ref(formula_dep), cache.clone())
                        .await
                        .remove(formula_dep)
                    {
                        // Pass Arc
                        Some(Ok(target_def @ InstallTargetIdentifier::Formula(_))) => {
                            debug!(
                                "Adding formula dependency '{}' from cask '{}'",
                                formula_dep, token
                            );
                            formulae_for_resolution.insert(formula_dep.clone(), target_def);
                        }
                        Some(Err(e)) => {
                            let msg = format!(
                                "Failed def fetch for formula dep '{formula_dep}' of cask '{token}': {e}"
                            );
                            if !errors.iter().any(|(n, _)| n == formula_dep) {
                                errors.push((formula_dep.clone(), SpsError::Generic(msg)));
                            }
                            processed_casks.insert(formula_dep.clone());
                        }
                        _ => {
                            let msg = format!(
                                "Formula dep '{formula_dep}' for cask '{token}' not found."
                            );
                            if !errors.iter().any(|(n, _)| n == formula_dep) {
                                errors.push((formula_dep.clone(), SpsError::NotFound(msg)));
                            }
                            processed_casks.insert(formula_dep.clone());
                        }
                    }
                }
            }
            for cask_dep in &deps.cask {
                if !processed_casks.contains(cask_dep) {
                    debug!("Queueing cask dep '{}' from cask '{}'", cask_dep, token);
                    cask_queue.push_back(cask_dep.clone());
                }
            }
        }
    }

    let mut resolved_formula_graph: Option<Arc<ResolvedGraph>> = None;
    if !formulae_for_resolution.is_empty() {
        let targets: Vec<_> = formulae_for_resolution.keys().cloned().collect();
        debug!("Resolving dependencies for formulae: {:?}", targets);
        let formulary = Formulary::new(config.clone());
        let keg = KegRegistry::new(config.clone());
        let ctx = ResolutionContext {
            formulary: &formulary,
            keg_registry: &keg,
            sps_prefix: config.sps_root(),
            include_optional: flags.include_optional,
            include_test: false,
            skip_recommended: flags.skip_recommended,
            force_build: flags.build_from_source,
        };
        let mut resolver = DependencyResolver::new(ctx);
        match resolver.resolve_targets(&targets) {
            Ok(g) => {
                debug!("Dependency resolution successful.");
                resolved_formula_graph = Some(Arc::new(g));
            }
            Err(e) => {
                error!(
                    "✖ Fatal dependency resolution error: {}. Aborting operation.",
                    e
                );
                for n in targets {
                    if !errors.iter().any(|(err_n, _)| err_n == &n) {
                        errors.push((n.clone(), SpsError::DependencyError(e.to_string())));
                    }
                }
                event_tx
                    .send(PipelineEvent::DependencyResolutionFinished)
                    .ok();
                return Ok((vec![], errors, already_installed));
            }
        }
    }
    event_tx
        .send(PipelineEvent::DependencyResolutionFinished)
        .ok();

    let mut final_planned_jobs: Vec<PlannedJob> = Vec::new();
    // Add initial ops first
    for (name, (action, opt_def)) in initial_ops {
        if errors.iter().any(|(n, _)| n == &name) {
            continue;
        }
        match opt_def {
            Some(target_def) => {
                let is_source_build = match &target_def {
                    InstallTargetIdentifier::Formula(f) => {
                        flags.build_from_source || !has_bottle(f)
                    }
                    InstallTargetIdentifier::Cask(_) => false,
                };
                final_planned_jobs.push(PlannedJob {
                    target_id: name.clone(),
                    target_definition: target_def.clone(),
                    action: action.clone(),
                    is_source_build,
                    use_private_store_source: private_store_sources.get(&name).cloned(),
                });
            }
            None => {
                if !errors.iter().any(|(n, _)| n == &name) {
                    errors.push((
                        name.clone(),
                        SpsError::Generic("Definition missing unexpectedly".into()),
                    ));
                }
            }
        }
    }
    // Add dependencies
    if let Some(graph) = resolved_formula_graph.as_ref() {
        // Borrow graph
        let mut dependency_jobs = Vec::new();
        for dep in &graph.install_plan {
            let name = dep.formula.name();
            if errors.iter().any(|(n, _)| n == name) || processed.contains(name) {
                continue;
            }
            if !final_planned_jobs.iter().any(|j| j.target_id == name) {
                if matches!(
                    dep.status,
                    ResolutionStatus::Missing | ResolutionStatus::Requested
                ) {
                    let is_source_build = flags.build_from_source || !has_bottle(&dep.formula);
                    dependency_jobs.push(PlannedJob {
                        target_id: name.to_string(),
                        target_definition: InstallTargetIdentifier::Formula(dep.formula.clone()),
                        action: JobAction::Install,
                        is_source_build,
                        use_private_store_source: None,
                    });
                }
            } else if let Some(initial_job) =
                final_planned_jobs.iter_mut().find(|j| j.target_id == name)
            {
                initial_job.is_source_build = flags.build_from_source || !has_bottle(&dep.formula);
                initial_job.use_private_store_source = None;
            }
        }
        final_planned_jobs.extend(dependency_jobs);

        for (token, cask_arc) in cask_deps_map {
            if errors.iter().any(|(n, _)| n == &token) || processed.contains(&token) {
                continue;
            }
            if !final_planned_jobs.iter().any(|j| j.target_id == token) {
                match installed::get_installed_package(&token, config).await {
                    Ok(None) => {
                        final_planned_jobs.push(PlannedJob {
                            target_id: token.clone(),
                            target_definition: InstallTargetIdentifier::Cask(cask_arc.clone()),
                            action: JobAction::Install,
                            is_source_build: false,
                            use_private_store_source: private_store_sources.get(&token).cloned(),
                        });
                    }
                    Ok(Some(_)) => {
                        already_installed.insert(token);
                    }
                    Err(e) => {
                        errors.push((
                            token.clone(),
                            SpsError::Generic(format!("Failed check install status {token}: {e}")),
                        ));
                    }
                }
            }
        }

        if !final_planned_jobs.is_empty() {
            debug!(
                "Sorting {} final planned jobs by dependency order",
                final_planned_jobs.len()
            );
            sort_planned_jobs_by_dependency_order(&mut final_planned_jobs, graph);
            // Pass borrowed
            // graph
        }
    }
    Ok((final_planned_jobs, errors, already_installed))
}

#[instrument(skip_all)]
async fn coordinate_downloads(
    planned_jobs: Vec<PlannedJob>,
    config: &Config,
    cache: Arc<Cache>,
    http_client: Arc<Client>,
    worker_job_tx: CrossbeamSender<WorkerJob>,
    event_tx: broadcast::Sender<PipelineEvent>,
) -> Vec<(String, SpsError)> {
    let mut download_tasks = JoinSet::new();
    let mut download_errors: Vec<(String, SpsError)> = Vec::new();

    for planned_job in planned_jobs {
        let config_clone = config.clone();
        let cache_clone = Arc::clone(&cache);
        let event_tx_clone = event_tx.clone();
        let job_id = planned_job.target_id.clone();
        let client_clone = Arc::clone(&http_client);

        tracing::debug!("(coordinate_downloads) Processing planned_job: {}", job_id);
        tracing::debug!(
            "(coordinate_downloads) use_private_store_source: {:?}",
            planned_job.use_private_store_source
        );
        tracing::debug!(
            "(coordinate_downloads) target_definition: {:?}",
            planned_job.target_definition
        );
        tracing::debug!(
            "(coordinate_downloads) is_source_from_private_store: {}",
            planned_job.use_private_store_source.is_some()
        );

        // If using private store source, skip download and send directly to worker
        if let Some(private_path) = planned_job.use_private_store_source.clone() {
            tracing::debug!(
                "(coordinate_downloads) [{}] Using app from private store for WorkerJob: {}",
                job_id,
                private_path.display()
            );
            debug!(
                "[{}] Using app from private store for WorkerJob: {}",
                job_id,
                private_path.display()
            );
            let size = fs::metadata(&private_path).map(|m| m.len()).unwrap_or(0);
            tracing::debug!(
                "(coordinate_downloads) [{}] private_path metadata size: {}",
                job_id,
                size
            );
            let worker_job = WorkerJob {
                request: planned_job,
                download_path: private_path.clone(),
                download_size_bytes: size,
                is_source_from_private_store: true,
            };
            tracing::debug!(
                "(coordinate_downloads) [{}] Sending WorkerJob to worker_job_tx (private store)",
                job_id
            );
            if let Err(e) = worker_job_tx.send(worker_job) {
                error!(
                    "[{}] Failed to queue worker job (private store): {}",
                    job_id, e
                );
                tracing::error!(
                    "(coordinate_downloads) [{}] Failed to queue worker job (private store): {}",
                    job_id,
                    e
                );
                download_errors.push((
                    job_id,
                    SpsError::Generic(format!("Failed to queue worker job (private store): {e}")),
                ));
            }
            continue;
        }

        tracing::debug!(
            "(coordinate_downloads) [{}] No private store source, proceeding to download phase",
            job_id
        );

        download_tasks.spawn(async move {
            tracing::debug!("(coordinate_downloads) [{}] Spawning download task", job_id);
            let tentative_url = match &planned_job.target_definition {
                InstallTargetIdentifier::Formula(f) => f.url.clone(),
                InstallTargetIdentifier::Cask(c) => match c.url.clone() {
                    Some(UrlField::Simple(s)) => s,
                    Some(UrlField::WithSpec { url, .. }) => url,
                    None => "unknown_cask_url".to_string(),
                },
            };
            tracing::debug!(
                "(coordinate_downloads) [{}] Download URL: {}",
                job_id,
                tentative_url
            );
            event_tx_clone
                .send(PipelineEvent::DownloadStarted {
                    target_id: job_id.clone(),
                    url: tentative_url.clone(),
                })
                .ok();

            let download_result: Result<PathBuf> = match &planned_job.target_definition {
                InstallTargetIdentifier::Formula(f) => {
                    if planned_job.is_source_build {
                        tracing::debug!(
                            "(coordinate_downloads) [{}] Downloading formula source",
                            job_id
                        );
                        build::formula::source::download_source(f, &config_clone).await
                    } else {
                        tracing::debug!(
                            "(coordinate_downloads) [{}] Downloading formula bottle",
                            job_id
                        );
                        build::formula::bottle::download_bottle(
                            f,
                            &config_clone,
                            client_clone.as_ref(),
                        )
                        .await
                    }
                }
                InstallTargetIdentifier::Cask(c) => {
                    tracing::debug!("(coordinate_downloads) [{}] Downloading cask", job_id);
                    build::cask::download_cask(c, cache_clone.as_ref()).await
                }
            };

            match download_result {
                Ok(download_path) => {
                    let size_bytes = fs::metadata(&download_path).map(|m| m.len()).unwrap_or(0);
                    tracing::debug!(
                        "(coordinate_downloads) [{}] Downloaded to {}, size: {}",
                        job_id,
                        download_path.display(),
                        size_bytes
                    );
                    debug!(
                        "[{}] Downloaded to: {} ({} bytes)",
                        job_id,
                        download_path.display(),
                        size_bytes
                    );
                    event_tx_clone
                        .send(PipelineEvent::DownloadFinished {
                            target_id: job_id.clone(),
                            path: download_path.clone(),
                            size_bytes,
                        })
                        .ok();
                    let worker_job = WorkerJob {
                        request: planned_job.clone(),
                        download_path,
                        download_size_bytes: size_bytes,
                        is_source_from_private_store: false,
                    };
                    Ok(worker_job) // Return WorkerJob
                }
                Err(e) => {
                    error!("[{}] Download failed: {}", job_id, e);

                    let url_for_error = match &planned_job.target_definition {
                        InstallTargetIdentifier::Formula(f) => f.url.clone(),
                        InstallTargetIdentifier::Cask(c) => match c.url.clone() {
                            Some(UrlField::Simple(s)) => s,
                            Some(UrlField::WithSpec { url, .. }) => url,
                            None => String::new(),
                        },
                    };
                    event_tx_clone
                        .send(PipelineEvent::download_failed(
                            job_id.clone(),
                            url_for_error,
                            e.clone(),
                        ))
                        .ok();
                    Err((planned_job, e)) // Return error
                }
            }
        });
    }

    while let Some(result) = download_tasks.join_next().await {
        match result {
            Ok(Ok(worker_job)) => {
                if worker_job_tx.send(worker_job).is_err() {
                    error!("Worker job channel closed while sending download result. Core likely shut down.");
                    // Add error? Maybe not necessary as core thread exit will be caught.
                    break; // Stop processing downloads if core is gone
                }
            }
            Ok(Err((failed_job, e))) => {
                download_errors.push((failed_job.target_id, e)); // Use ID from request
            }
            Err(join_error) => {
                error!("✖ Download task panicked: {}", join_error);
                download_errors.push((
                    "[Download Coordinator]".to_string(),
                    SpsError::Generic(format!("Download task panicked: {join_error}")),
                ));
            }
        }
    }
    download_errors
}

#[instrument(skip_all)]
async fn fetch_target_definitions(
    names: &[String],
    cache: Arc<Cache>,
) -> HashMap<String, Result<InstallTargetIdentifier>> {
    let mut results = HashMap::new();
    let mut futures = JoinSet::new();

    let formulae_map_handle = tokio::spawn(load_or_fetch_formulae_map(Arc::clone(&cache)));
    let casks_map_handle = tokio::spawn(load_or_fetch_casks_map(Arc::clone(&cache)));

    let formulae_map = match formulae_map_handle.await {
        Ok(Ok(map)) => Some(map),
        Ok(Err(e)) => {
            warn!("Failed to load/fetch full formulae list: {}", e);
            None
        }
        Err(e) => {
            warn!("Formulae map loading task failed: {}", e);
            None
        }
    };
    let casks_map = match casks_map_handle.await {
        Ok(Ok(map)) => Some(map),
        Ok(Err(e)) => {
            warn!("Failed to load/fetch full casks list: {}", e);
            None
        }
        Err(e) => {
            warn!("Casks map loading task failed: {}", e);
            None
        }
    };

    for name_str in names {
        let name = name_str.clone();
        let formulae_map_clone = formulae_map.clone();
        let casks_map_clone = casks_map.clone();

        futures.spawn(async move {
            if let Some(map) = formulae_map_clone {
                if let Some(f_arc) = map.get(&name) {
                    return (name, Ok(InstallTargetIdentifier::Formula(f_arc.clone())));
                }
            }

            if let Some(map) = casks_map_clone {
                if let Some(c_arc) = map.get(&name) {
                    return (name, Ok(InstallTargetIdentifier::Cask(c_arc.clone())));
                }
            }

            warn!(
                "Definition for '{}' not found in cached lists, fetching directly from API...",
                name
            );
            match api::get_formula(&name).await {
                Ok(formula) => {
                    return (
                        name,
                        Ok(InstallTargetIdentifier::Formula(Arc::new(formula))),
                    );
                }
                Err(SpsError::NotFound(_)) => {}
                Err(e) => return (name, Err(e)),
            }
            match api::get_cask(&name).await {
                Ok(cask) => (name, Ok(InstallTargetIdentifier::Cask(Arc::new(cask)))),
                Err(SpsError::NotFound(_)) => (
                    name.clone(),
                    Err(SpsError::NotFound(format!(
                        "Formula or Cask '{name}' not found"
                    ))),
                ),
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
                error!("Task join error during definition fetch: {}", e);
            }
        }
    }
    results
}

async fn load_or_fetch_formulae_map(cache: Arc<Cache>) -> Result<HashMap<String, Arc<Formula>>> {
    match cache.load_raw("formula.json") {
        Ok(data) => {
            debug!("Loaded formula.json from cache.");
            let formulas: Vec<Formula> = serde_json::from_str(&data)
                .map_err(|e| SpsError::Cache(format!("Parse cached formula.json failed: {e}")))?;
            Ok(formulas
                .into_iter()
                .map(|f| (f.name.clone(), Arc::new(f)))
                .collect())
        }
        Err(e) => {
            debug!(
                "Cache miss/error for formula.json ({}), fetching from API...",
                e
            );
            let raw_data = api::fetch_all_formulas().await?;
            if let Err(cache_err) = cache.store_raw("formula.json", &raw_data) {
                warn!("Failed cache formula.json: {}", cache_err);
            } else {
                debug!("Cached formula.json.");
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

async fn load_or_fetch_casks_map(cache: Arc<Cache>) -> Result<HashMap<String, Arc<Cask>>> {
    match cache.load_raw("cask.json") {
        Ok(data) => {
            debug!("Loaded cask.json from cache.");
            let casks: Vec<Cask> = serde_json::from_str(&data)
                .map_err(|e| SpsError::Cache(format!("Parse cached cask.json failed: {e}")))?;
            Ok(casks
                .into_iter()
                .map(|c| (c.token.clone(), Arc::new(c)))
                .collect())
        }
        Err(e) => {
            debug!(
                "Cache miss/error for cask.json ({}), fetching from API...",
                e
            );
            let raw_data = api::fetch_all_casks().await?;
            if let Err(cache_err) = cache.store_raw("cask.json", &raw_data) {
                warn!("Failed cache cask.json: {}", cache_err);
            } else {
                debug!("Cached cask.json.");
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

fn has_bottle(formula: &Formula) -> bool {
    build::formula::has_bottle_for_current_platform(formula)
}

fn sort_planned_jobs_by_dependency_order(jobs: &mut [PlannedJob], graph: &ResolvedGraph) {
    let formula_order: HashMap<String, usize> = graph
        .install_plan
        .iter()
        .enumerate()
        .map(|(idx, dep)| (dep.formula.name().to_string(), idx))
        .collect();

    jobs.sort_by_key(|job| {
        match &job.target_definition {
            InstallTargetIdentifier::Formula(_f) => {
                // Use target_id which should match the formula name
                formula_order
                    .get(&job.target_id)
                    .copied()
                    .unwrap_or(usize::MAX)
            }
            InstallTargetIdentifier::Cask(_) => usize::MAX,
        }
    });
}
