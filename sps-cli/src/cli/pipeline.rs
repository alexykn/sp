// sps-cli/src/cli/pipeline.rs

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

// use tokio::sync::Mutex; // For async-aware locking if needed later
use colored::Colorize;
use crossbeam_channel::{Receiver, Sender, bounded};
use futures::executor::block_on;
use num_cpus;
use serde_json::Value;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::dependency::{
    DependencyResolver, ResolutionContext, ResolutionStatus, ResolvedGraph,
};
use sps_common::error::{Result, SpsError};
use sps_common::formulary::Formulary;
use sps_common::keg::KegRegistry;
use sps_common::model::Cask;
// --- Shared Data Structures ---

// Reusable enum to identify target type, potentially moved from sps-core if made public
// Or defined locally here if InstallTargetIdentifier from core isn't suitable.
// Assuming we use the one from core for now:
use sps_common::model::InstallTargetIdentifier;
use sps_common::model::formula::{Formula, FormulaDependencies};
use sps_core::build::{self};
use sps_core::installed::{InstalledPackageInfo, PackageType}; /* Needs implementing in
                                                                * sps-core */
use sps_core::uninstall as core_uninstall; // Alias for the new module
use sps_core::uninstall::UninstallOptions; // Needs implementing in sps-core
use sps_core::update_check::{self, UpdateInfo}; // Needs implementing in sps-core
use sps_net::fetch::api;
use threadpool::ThreadPool;
use tokio::task::JoinSet;
use tracing::{Instrument, debug, error, instrument, warn}; // Placeholder: Ensure this is accessible

// Represents the specific action for a pipeline job
#[derive(Debug, Clone)]
pub enum PipelineActionType {
    Install,
    Upgrade {
        from_version: String,
        old_install_path: PathBuf, // Path to the version being replaced
    },
    Reinstall {
        version: String,
        current_install_path: PathBuf, // Path to the version being reinstalled
    },
}

// Represents a unit of work for the pipeline
#[derive(Debug)]
pub struct PipelineJob {
    pub target: InstallTargetIdentifier, // Arc<Formula> or Arc<Cask>
    pub download_path: PathBuf,          // Path to the downloaded file (bottle, source, cask)
    pub action: PipelineActionType,
    // Graph needed for source builds to know dependencies
    pub resolved_graph: Option<Arc<ResolvedGraph>>,
    pub is_source_build: bool,
}

// Represents the outcome of processing a PipelineJob
#[derive(Debug)]
pub enum PipelineJobResult {
    InstallOk(String, PackageType),
    UpgradeOk(String, PackageType, String), // Name, Type, OldVersion
    ReinstallOk(String, PackageType),       // Name, Type
    InstallErr(String, PackageType, SpsError),
    UpgradeErr(String, PackageType, String, SpsError), // Include old version
    ReinstallErr(String, PackageType, SpsError),
}

// Represents the type of command triggering the pipeline
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandType {
    Install,
    Reinstall,
    Upgrade { all: bool },
}

// Flags affecting pipeline behavior
#[derive(Debug, Clone)]
pub struct PipelineFlags {
    pub build_from_source: bool,
    pub include_optional: bool,
    pub skip_recommended: bool,
    // Add other common flags like --force if needed
}

// Add this after the PipelineFlags struct, before PipelineExecutor
type PlanResult = Result<(Vec<PipelineJob>, Vec<(String, SpsError)>, HashSet<String>)>;

// The main orchestrator struct
pub struct PipelineExecutor;

impl PipelineExecutor {
    /// Main entry point to run install, reinstall, or upgrade.
    #[instrument(skip(config, cache, flags), fields(cmd = ?command_type, targets = ?initial_targets))]
    pub async fn execute_pipeline(
        initial_targets: &[String],
        command_type: CommandType,
        config: &Config,
        cache: Arc<Cache>,
        flags: &PipelineFlags,
    ) -> Result<()> {
        // Define worker/queue size (same logic as before)
        let worker_count = std::cmp::max(1, num_cpus::get_physical().saturating_sub(1)).min(6); // Example sizing
        let queue_size = worker_count * 2;

        // --- 1. Plan Operations ---
        debug!("Planning package operations...");
        let (planned_jobs, mut overall_errors, already_installed) = Self::plan_package_operations(
            initial_targets,
            command_type.clone(),
            config,
            cache.clone(),
            flags,
        )
        .await?;

        // Report planning errors and already installed packages
        for name in already_installed {
            info_line(format!(
                "{} {} is already installed.",
                "✓".green(),
                name.cyan()
            ));
        }
        for (name, err) in &overall_errors {
            error!("✖ Error during planning for '{}': {}", name.cyan(), err);
        }

        if planned_jobs.is_empty() {
            if overall_errors.is_empty() {
                info_line("No packages need to be installed, upgraded, or reinstalled.");
                return Ok(());
            } else {
                error!("No operations possible due to planning errors.");
                // Combine errors into a single message for returning
                let final_error_msg = overall_errors
                    .into_iter()
                    .map(|(name, err)| format!("'{name}': {err}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(SpsError::InstallError(format!(
                    "Operation failed during planning: {final_error_msg}"
                )));
            }
        }
        debug!("Planning complete. {} jobs generated.", planned_jobs.len());

        // --- 2. Setup Channels & Worker Pool ---
        let (job_tx, job_rx): (Sender<PipelineJob>, Receiver<PipelineJob>) = bounded(queue_size);
        let (result_tx, result_rx): (Sender<PipelineJobResult>, Receiver<PipelineJobResult>) =
            bounded(queue_size);
        let pool = ThreadPool::new(worker_count);
        let client = Arc::new(reqwest::Client::new()); // HTTP client for downloads

        // --- 3. Coordinate Downloads ---
        debug!("Coordinating downloads...");
        let download_errors_count = Self::coordinate_downloads(
            planned_jobs, // Pass the Vec directly
            config,
            cache.clone(),
            client,
            job_tx.clone(), // Clone Sender for the download coordinator
            flags,
        )
        .await?;
        drop(job_tx); // Signal that no more download jobs will be sent
        debug!(
            "Download coordination finished. {} errors.",
            download_errors_count
        );
        if download_errors_count > 0 {
            // Add generic error if specific ones weren't captured during download
            overall_errors.push((
                "[Download Phase]".to_string(),
                SpsError::Generic(format!(
                    "Encountered {download_errors_count} download errors."
                )),
            ));
        }

        // --- 4. Coordinate Workers & Collect Results ---
        debug!("Coordinating workers...");
        let pump_handle = Self::coordinate_workers(
            pool,              // Pass the pool
            job_rx,            // Pass the Receiver
            result_tx.clone(), // Clone Sender for workers
            config,
            cache.clone(),
            // No flags needed directly by worker coordinator? Flags are in job.
        );
        drop(result_tx); // Drop the original Sender for results
        debug!("Collecting results...");
        let install_errors = Self::collect_results(result_rx); // Collect results from the Receiver

        if let Err(e) = pump_handle.await {
            error!("Worker coordination task panicked: {}", e);
            overall_errors.push((
                "[Worker Pool]".to_string(),
                SpsError::Generic(format!("Worker coordination failed: {e}")),
            ));
        }
        debug!("Result collection finished.");

        // --- 5. Combine and Report Final Status ---
        overall_errors.extend(install_errors); // Add errors collected from workers

        if overall_errors.is_empty() {
            info_line("Pipeline execution completed successfully.");
            Ok(())
        } else {
            error!(
                "Pipeline execution completed with {} error(s).",
                overall_errors.len()
            );
            let final_error_msg = overall_errors
                .into_iter()
                .map(|(name, err)| format!("'{name}': {err}"))
                .collect::<Vec<_>>()
                .join("; ");
            Err(SpsError::InstallError(format!(
                "Operation failed: {final_error_msg}"
            )))
        }
    }

    /// Determines the set of operations (Install, Upgrade, Reinstall) needed.
    #[instrument(skip(config, cache, flags), fields(cmd = ?command_type))]
    async fn plan_package_operations(
        initial_targets: &[String],
        command_type: CommandType,
        config: &Config,
        cache: Arc<Cache>,
        flags: &PipelineFlags,
    ) -> PlanResult {
        let mut jobs: Vec<PipelineJob> = Vec::new();
        let mut errors: Vec<(String, SpsError)> = Vec::new();
        let mut already_installed: HashSet<String> = HashSet::new();
        let _needs_resolution: HashMap<String, InstallTargetIdentifier> = HashMap::new(); // name -> target def for resolution
        let mut processed: HashSet<String> = HashSet::new(); // Track names already decided upon

        // --- Identify Initial Targets and Action Type ---
        let mut initial_ops: HashMap<
            String,
            (PipelineActionType, Option<InstallTargetIdentifier>),
        > = HashMap::new();

        match command_type {
            CommandType::Install => {
                debug!("Planning for INSTALL command");
                for name in initial_targets {
                    if processed.contains(name) {
                        continue;
                    }
                    match sps_core::installed::get_installed_package(name, config).await? {
                        Some(_installed_info) => {
                            already_installed.insert(name.clone());
                            processed.insert(name.clone());
                        }
                        None => {
                            // Mark for install, need to fetch definition later
                            initial_ops.insert(name.clone(), (PipelineActionType::Install, None));
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
                    match sps_core::installed::get_installed_package(name, config).await? {
                        Some(installed_info) => {
                            initial_ops.insert(
                                name.clone(),
                                (
                                    PipelineActionType::Reinstall {
                                        version: installed_info.version.clone(),
                                        current_install_path: installed_info.path.clone(),
                                    },
                                    None,
                                ),
                            ); // Need to fetch definition
                        }
                        None => {
                            let msg = format!("Cannot reinstall '{name}': not installed.");
                            error!("✖ {msg}");
                            errors.push((name.clone(), SpsError::NotFound(msg)));
                            processed.insert(name.clone());
                        }
                    }
                }
            }
            CommandType::Upgrade { all } => {
                debug!("Planning for UPGRADE command (all={})", all);
                let packages_to_check = if all {
                    sps_core::installed::get_installed_packages(config).await?
                } else {
                    let mut specific = Vec::new();
                    for name in initial_targets {
                        match sps_core::installed::get_installed_package(name, config).await? {
                            Some(info) => specific.push(info),
                            None => {
                                let msg = format!("Cannot upgrade '{name}': not installed.");
                                warn!("! {msg}"); // Warn, maybe user meant install?
                                // Don't add error here, let install handle it if they meant install
                                processed.insert(name.clone());
                            }
                        }
                    }
                    specific
                };

                if packages_to_check.is_empty() {
                    if all {
                        info_line("No installed packages found to check for upgrades.");
                    }
                    // else: warnings about specific packages already printed
                    return Ok((jobs, errors, already_installed)); // No ops needed
                }

                let updates = update_check::check_for_updates(&packages_to_check, &cache).await?;
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
                                PipelineActionType::Upgrade {
                                    from_version: installed.version.clone(),
                                    old_install_path: installed.path.clone(),
                                },
                                Some(update_info.target_definition.clone()),
                            ),
                        ); // Have target def!
                        processed.insert(installed.name.clone());
                    } else {
                        // Already up-to-date, mark as already installed for reporting
                        already_installed.insert(installed.name.clone());
                        processed.insert(installed.name.clone());
                    }
                }
            }
        }

        // --- Fetch Definitions for Install/Reinstall targets ---
        let definitions_to_fetch: Vec<String> = initial_ops
            .iter()
            .filter(|(_, (_, def))| def.is_none())
            .map(|(name, _)| name.clone())
            .collect();

        if !definitions_to_fetch.is_empty() {
            debug!(
                "Fetching definitions for initial targets: {:?}",
                definitions_to_fetch
            );
            let fetched_defs = Self::fetch_target_definitions(&definitions_to_fetch, &cache).await;

            for (name, result) in fetched_defs {
                match result {
                    Ok(target_def) => {
                        if let Some((_, existing_def_opt)) = initial_ops.get_mut(&name) {
                            *existing_def_opt = Some(target_def);
                        }
                    }
                    Err(e) => {
                        error!(
                            "✖ Failed to get definition for target '{}': {}",
                            name.cyan(),
                            e
                        );
                        errors.push((name.clone(), e));
                        initial_ops.remove(&name); // Remove from ops if def fetch fails
                        processed.insert(name.clone());
                    }
                }
            }
        }

        // --- Initial Dependency Resolution Setup ---
        let mut formulae_for_resolution: HashMap<String, InstallTargetIdentifier> = HashMap::new();
        let mut cask_queue: VecDeque<String> = VecDeque::new();
        let mut cask_deps_map: HashMap<String, Arc<Cask>> = HashMap::new(); // Cache fetched cask defs

        for (name, (_action, opt_def)) in &initial_ops {
            match opt_def {
                Some(InstallTargetIdentifier::Formula(f_arc)) => {
                    formulae_for_resolution.insert(
                        name.clone(),
                        InstallTargetIdentifier::Formula(f_arc.clone()),
                    );
                }
                Some(InstallTargetIdentifier::Cask(c_arc)) => {
                    if !processed.contains(name) {
                        // Only queue if not already handled/errored
                        cask_queue.push_back(name.clone());
                        cask_deps_map.insert(name.clone(), c_arc.clone());
                    }
                }
                None => {
                    // Should not happen if fetch logic is correct, but handle defensively
                    if !errors.iter().any(|(n, _)| n == name) {
                        // Avoid duplicate errors
                        let msg =
                            format!("Definition missing for target '{name}' after fetch attempt.");
                        error!("✖ {msg}");
                        errors.push((name.clone(), SpsError::Generic(msg)));
                    }
                    processed.insert(name.clone());
                }
            }
        }

        // --- Resolve Cask Dependencies (Iterative) ---
        // Similar to logic in old gather_full_dependency_set, but adds formula deps to
        // formulae_for_resolution
        let mut processed_casks: HashSet<String> = initial_ops.keys().cloned().collect();

        while let Some(token) = cask_queue.pop_front() {
            let cask_ref = cask_deps_map.entry(token.clone()).or_insert_with(|| {
                // Fetch cask definition if not already cached in our map
                // This part needs to be async, might require restructuring or
                // pre-fetching all needed cask defs. For simplicity sketch, assume pre-fetched.
                // In reality, you might need another async fetch loop here or integrate into the
                // initial fetch.
                match block_on(api::get_cask(&token)) {
                    // block_on is suboptimal here
                    Ok(c) => Arc::new(c),
                    Err(e) => {
                        if !errors.iter().any(|(n, _)| n == &token) {
                            errors.push((token.clone(), e));
                        }
                        // Return a dummy Arc or handle differently
                        Arc::new(Cask {
                            token: token.clone(),
                            ..Default::default()
                        }) // Dummy
                    }
                }
            });
            let cask = cask_ref.clone();

            if errors.iter().any(|(n, _)| n == &token) {
                continue;
            } // Skip if fetch failed

            if let Some(deps) = &cask.depends_on {
                for formula_dep in &deps.formula {
                    if !formulae_for_resolution.contains_key(formula_dep) {
                        // Need to fetch formula definition before adding
                        match Self::fetch_target_definitions(&[formula_dep.clone()], &cache)
                            .await
                            .remove(formula_dep)
                        {
                            Some(Ok(target_def @ InstallTargetIdentifier::Formula(_))) => {
                                debug!(
                                    "Adding formula dependency from cask '{}': {}",
                                    token, formula_dep
                                );
                                formulae_for_resolution.insert(formula_dep.clone(), target_def);
                            }
                            Some(Err(e)) => {
                                if !errors.iter().any(|(n, _)| n == formula_dep) {
                                    errors.push((formula_dep.clone(), e));
                                }
                            }
                            _ => {
                                // Not found or not a formula
                                let msg = format!(
                                    "Dependency '{formula_dep}' for cask '{token}' not found or not a formula."
                                );
                                if !errors.iter().any(|(n, _)| n == formula_dep) {
                                    errors.push((formula_dep.clone(), SpsError::NotFound(msg)));
                                }
                            }
                        }
                    }
                }
                for cask_dep in &deps.cask {
                    if processed_casks.insert(cask_dep.clone()) {
                        debug!(
                            "Queueing cask dependency from cask '{}': {}",
                            token, cask_dep
                        );
                        cask_queue.push_back(cask_dep.clone());
                        // Definition will be fetched when popped if not in cask_deps_map
                    }
                }
            }
        }

        // --- Resolve Formula Dependencies ---
        let mut resolved_formula_graph: Option<Arc<ResolvedGraph>> = None;
        if !formulae_for_resolution.is_empty() {
            let resolution_target_names: Vec<String> =
                formulae_for_resolution.keys().cloned().collect();
            debug!(
                "Resolving dependencies for formulae: {:?}",
                resolution_target_names
            );
            let formulary = Formulary::new(config.clone());
            let keg_registry = KegRegistry::new(config.clone());
            let ctx = ResolutionContext {
                formulary: &formulary,
                keg_registry: &keg_registry,
                sps_prefix: config.prefix(),
                include_optional: flags.include_optional,
                include_test: false, // Typically false for install/upgrade
                skip_recommended: flags.skip_recommended,
                force_build: flags.build_from_source, // Pass build flag here
            };
            let mut resolver = DependencyResolver::new(ctx);

            match resolver.resolve_targets(&resolution_target_names) {
                Ok(graph) => {
                    debug!("Dependency resolution successful.");
                    resolved_formula_graph = Some(Arc::new(graph));
                }
                Err(e) => {
                    error!(
                        "✖ Fatal dependency resolution error: {}. Aborting operation.",
                        e
                    );
                    // Add error for all requested formulae
                    for name in resolution_target_names {
                        if !errors.iter().any(|(n, _)| n == &name) {
                            errors.push((name.clone(), SpsError::DependencyError(e.to_string())));
                        }
                    }
                    // Return early as resolution is fundamental
                    return Ok((jobs, errors, already_installed));
                }
            }
        }

        // --- Construct Final Job List ---
        let final_graph = resolved_formula_graph.clone(); // Arc clone

        // Add initial ops first (Install, Upgrade, Reinstall)
        for (name, (action, opt_def)) in initial_ops {
            if errors.iter().any(|(n, _)| n == &name) {
                continue;
            } // Skip errored targets
            if let Some(target_def) = opt_def {
                jobs.push(PipelineJob {
                    target: target_def.clone(),    // Clone here
                    download_path: PathBuf::new(), // Will be filled by download coordinator
                    action: action.clone(),
                    resolved_graph: final_graph.clone(), // Pass graph if formula
                    is_source_build: match action {
                        // Determine if source build is needed based on flags and bottle
                        // availability
                        PipelineActionType::Install | PipelineActionType::Upgrade { .. } => {
                            if let InstallTargetIdentifier::Formula(f) = &target_def {
                                flags.build_from_source
                                    || !build::formula::has_bottle_for_current_platform(f)
                            } else {
                                false
                            }
                        }
                        PipelineActionType::Reinstall { .. } => {
                            // Reinstall might need source build if forced or original bottle
                            // missing?
                            if let InstallTargetIdentifier::Formula(f) = &target_def {
                                flags.build_from_source
                                    || !build::formula::has_bottle_for_current_platform(f)
                            // Check availability for the *current* version
                            } else {
                                false
                            }
                        }
                    },
                });
            }
        }

        // Add dependency installs from the graph
        if let Some(graph) = resolved_formula_graph {
            for dep in &graph.install_plan {
                let name = dep.formula.name();
                if errors.iter().any(|(n, _)| n == name) {
                    continue;
                } // Skip errored deps
                // Add only if it wasn't an initial target already added
                if !jobs.iter().any(|j| match &j.target {
                    InstallTargetIdentifier::Formula(f) => f.name() == name,
                    _ => false,
                }) {
                    if matches!(
                        dep.status,
                        ResolutionStatus::Missing | ResolutionStatus::Requested
                    ) {
                        jobs.push(PipelineJob {
                            target: InstallTargetIdentifier::Formula(dep.formula.clone()),
                            download_path: PathBuf::new(),
                            action: PipelineActionType::Install, // Dependencies are always installs
                            resolved_graph: Some(graph.clone()),
                            is_source_build: flags.build_from_source
                                || !build::formula::has_bottle_for_current_platform(&dep.formula),
                        });
                    }
                } else {
                    // If it *was* an initial target, update its source build status based on
                    // resolution
                    if let Some(initial_job) = jobs.iter_mut().find(|j| match &j.target {
                        InstallTargetIdentifier::Formula(f) => f.name() == name,
                        _ => false,
                    }) {
                        initial_job.is_source_build = flags.build_from_source
                            || !build::formula::has_bottle_for_current_platform(&dep.formula);
                    }
                }
            }
            // Add cask dependencies identified earlier (if they need installing)
            for (token, cask_arc) in cask_deps_map {
                if errors.iter().any(|(n, _)| n == &token) {
                    continue;
                }
                if !jobs.iter().any(|j| match &j.target {
                    InstallTargetIdentifier::Cask(c) => c.token == token,
                    _ => false,
                }) {
                    // Check if cask is actually installed before adding install job
                    if sps_core::installed::get_installed_package(&token, config)
                        .await?
                        .is_none()
                    {
                        jobs.push(PipelineJob {
                            target: InstallTargetIdentifier::Cask(cask_arc.clone()),
                            download_path: PathBuf::new(),
                            action: PipelineActionType::Install,
                            resolved_graph: None,
                            is_source_build: false,
                        });
                    } else {
                        // Mark as already installed if it's just a dependency and present
                        already_installed.insert(token);
                    }
                }
            }

            // Sort all jobs by dependency order before returning
            if !jobs.is_empty() {
                debug!("Sorting {} jobs by dependency order", jobs.len());
                sort_jobs_by_dependency_order(&mut jobs, &graph);
            }
        }

        Ok((jobs, errors, already_installed))
    }

    /// Fetches Formula or Cask definitions for a list of names.
    async fn fetch_target_definitions(
        names: &[String],
        cache: &Cache,
    ) -> HashMap<String, Result<InstallTargetIdentifier>> {
        let mut results = HashMap::new();
        let mut futures = JoinSet::new();

        // Attempt to load full lists first to minimize API calls
        let formulae_map_res = load_or_fetch_json(cache, "formula.json", api::fetch_all_formulas())
            .await
            .map(|values| {
                values
                    .into_iter()
                    .filter_map(|v| serde_json::from_value::<Formula>(v).ok())
                    .map(|f| (f.name.clone(), Arc::new(f)))
                    .collect::<HashMap<_, _>>()
            });

        let casks_map_res = load_or_fetch_json(cache, "cask.json", api::fetch_all_casks())
            .await
            .map(|values| {
                values
                    .into_iter()
                    .filter_map(|v| serde_json::from_value::<Cask>(v).ok())
                    .map(|c| (c.token.clone(), Arc::new(c)))
                    .collect::<HashMap<_, _>>()
            });

        for name in names {
            let name = name.clone();
            let formulae_map_clone = formulae_map_res.as_ref().ok().cloned();
            let casks_map_clone = casks_map_res.as_ref().ok().cloned();

            futures.spawn(async move {
                let formulae_map = formulae_map_clone; // Use the cloned map
                let casks_map = casks_map_clone; // Use the cloned map
                // Check formulae map first
                if let Some(map) = formulae_map {
                    if let Some(f_arc) = map.get(&name) {
                        return (name, Ok(InstallTargetIdentifier::Formula(f_arc.clone())));
                    }
                }
                // Check casks map next
                if let Some(map) = casks_map {
                    if let Some(c_arc) = map.get(&name) {
                        return (name, Ok(InstallTargetIdentifier::Cask(c_arc.clone())));
                    }
                }
                // If not found in maps (maybe maps failed to load, or item is obscure), try direct
                // API fetch This adds redundancy but makes it more robust if full
                // list fetch fails
                match api::get_formula(&name).await {
                    // Using get_formula which returns Formula
                    Ok(formula) => {
                        return (
                            name,
                            Ok(InstallTargetIdentifier::Formula(Arc::new(formula))),
                        );
                    }
                    Err(SpsError::NotFound(_)) | Err(SpsError::Api(_)) | Err(SpsError::Http(_)) => {
                        // Formula fetch failed, try cask
                    }
                    Err(e) => return (name, Err(e)), // Propagate other formula errors
                }
                match api::get_cask(&name).await {
                    // Using get_cask which returns Cask
                    Ok(cask) => (name, Ok(InstallTargetIdentifier::Cask(Arc::new(cask)))),
                    Err(e) => (name, Err(e)), // Return cask error (could be NotFound)
                }
            });
        }

        while let Some(res) = futures.join_next().await {
            match res {
                Ok((name, result)) => {
                    results.insert(name, result);
                }
                Err(e) => {
                    // Log join error, but difficult to associate with a name here
                    error!("Task join error during definition fetch: {}", e);
                }
            }
        }
        results
    }

    /// Coordinates the download phase.
    #[instrument(skip(planned_jobs, config, cache, client, job_tx, flags))]
    async fn coordinate_downloads(
        planned_jobs: Vec<PipelineJob>, // Takes ownership of the jobs Vec
        config: &Config,
        cache: Arc<Cache>,
        client: Arc<reqwest::Client>,
        job_tx: Sender<PipelineJob>, // Sender for jobs ready to be installed
        flags: &PipelineFlags,
    ) -> Result<usize> {
        // Returns count of download errors
        let mut download_join_set: JoinSet<Result<(PipelineJob, String)>> = JoinSet::new();
        let mut download_errors_count = 0;

        // Spawn download tasks
        for mut job in planned_jobs {
            // Mutate job to set is_source_build
            let name = match &job.target {
                InstallTargetIdentifier::Formula(f) => f.name().to_string(),
                InstallTargetIdentifier::Cask(c) => c.token.clone(),
            };
            let name_clone = name.clone();
            let target_type = job.target.clone(); // Clone Arc for the task
            let cfg_clone = config.clone();
            let cache_clone = Arc::clone(&cache);
            let client_clone = Arc::clone(&client);
            // Determine source build requirement *before* spawning download task
            job.is_source_build = match &target_type {
                InstallTargetIdentifier::Formula(f) => {
                    flags.build_from_source || !build::formula::has_bottle_for_current_platform(f)
                }
                InstallTargetIdentifier::Cask(_) => false,
            };
            let is_source_build = job.is_source_build; // Copy bool for task

            download_join_set.spawn(
                async move {
                    // Now call download_target with the pre-determined is_source_build flag
                    let download_path = download_target_file(
                        &name,
                        &target_type,
                        &cfg_clone,
                        cache_clone,
                        client_clone,
                        is_source_build,
                    )
                    .await?;
                    job.download_path = download_path; // Update job with download path
                    Ok((job, name)) // Return the modified job
                }
                .instrument(tracing::info_span!("download_task", pkg = %name_clone)), // Use name_clone here
            );
        }

        // Process download results
        while let Some(result) = download_join_set.join_next().await {
            match result {
                Ok(Ok((install_job, _name))) => {
                    // Send the job with download_path populated
                    if job_tx.send(install_job).is_err() {
                        error!(
                            "Job channel closed while sending download result for {}",
                            _name
                        );
                        download_errors_count += 1; // Treat send error as a download phase error
                    }
                }
                Ok(Err(e)) => {
                    // Log error, name extraction might be needed if not DownloadError
                    let name = match &e {
                        SpsError::DownloadError(n, _, _) => n.clone(),
                        _ => "[unknown]".to_string(),
                    };
                    error!("✖ Download failed for '{}': {}", name.cyan(), e);
                    download_errors_count += 1;
                }
                Err(join_error) => {
                    error!("✖ Download task panicked: {}", join_error);
                    download_errors_count += 1;
                }
            }
        }

        Ok(download_errors_count)
    }

    /// Spawns a task to coordinate worker threads.
    fn coordinate_workers(
        pool: ThreadPool,
        job_rx: Receiver<PipelineJob>,
        result_tx: Sender<PipelineJobResult>,
        config: &Config,
        cache: Arc<Cache>,
        // flags are passed within the PipelineJob
    ) -> tokio::task::JoinHandle<()> {
        let cfg_clone = config.clone(); // Clone config once for the coordinator task
        tokio::spawn(
            async move {
                while let Ok(job) = job_rx.recv() {
                    let pkg_name = match &job.target {
                        InstallTargetIdentifier::Formula(f) => f.name().to_string(),
                        InstallTargetIdentifier::Cask(c) => c.token.clone(),
                    };
                    let res_tx = result_tx.clone();
                    let worker_cfg = cfg_clone.clone(); // Clone config again for the worker thread
                    let worker_cache = Arc::clone(&cache);
                    let install_span = tracing::info_span!("install_worker", pkg = %pkg_name);

                    pool.execute(move || {
                        // Run the potentially blocking install logic in the thread pool
                        let result = install_span
                            .in_scope(|| Self::run_pipeline_job(job, &worker_cfg, worker_cache));

                        if res_tx.send(result).is_err() {
                            warn!(
                                "Result channel closed, could not send install result for {}.",
                                pkg_name
                            );
                        }
                    });
                }
                debug!("Job channel closed, worker coordinator task finishing.");
            }
            .in_current_span(), // Inherit span context
        )
    }

    /// Collects results from worker threads.
    fn collect_results(result_rx: Receiver<PipelineJobResult>) -> Vec<(String, SpsError)> {
        let mut install_errors: Vec<(String, SpsError)> = Vec::new();
        for result in result_rx {
            // Drains the channel
            let (_result, was_success, message) = match result {
                PipelineJobResult::InstallOk(name, pkg_type) => {
                    let pkg_type_str = match pkg_type {
                        PackageType::Formula => "Formula",
                        PackageType::Cask => "Cask",
                    };
                    (
                        name.clone(),
                        true,
                        format!("Installed {} {}", pkg_type_str, name.green()),
                    )
                }
                PipelineJobResult::UpgradeOk(name, pkg_type, old_v) => {
                    let pkg_type_str = match pkg_type {
                        PackageType::Formula => "Formula",
                        PackageType::Cask => "Cask",
                    };
                    (
                        name.clone(),
                        true,
                        format!(
                            "Upgraded {} {} (from {})",
                            pkg_type_str,
                            name.green(),
                            old_v
                        ),
                    )
                }
                PipelineJobResult::ReinstallOk(name, pkg_type) => {
                    let pkg_type_str = match pkg_type {
                        PackageType::Formula => "Formula",
                        PackageType::Cask => "Cask",
                    };
                    (
                        name.clone(),
                        true,
                        format!("Reinstalled {} {}", pkg_type_str, name.green()),
                    )
                }
                PipelineJobResult::InstallErr(name, pkg_type, e) => {
                    let pkg_type_str = match pkg_type {
                        PackageType::Formula => "Formula",
                        PackageType::Cask => "Cask",
                    };
                    let err_msg = format!("Failed {} '{}': {}", pkg_type_str, name.red(), e);
                    install_errors.push((name.clone(), e));
                    (name.clone(), false, err_msg)
                }
                PipelineJobResult::UpgradeErr(name, pkg_type, old_v, e) => {
                    let pkg_type_str = match pkg_type {
                        PackageType::Formula => "Formula",
                        PackageType::Cask => "Cask",
                    };
                    let err_msg = format!(
                        "Failed {} upgrade '{}' (from {}): {}",
                        pkg_type_str,
                        name.red(),
                        old_v,
                        e
                    );
                    install_errors.push((name.clone(), e));
                    (name.clone(), false, err_msg)
                }
                PipelineJobResult::ReinstallErr(name, pkg_type, e) => {
                    let pkg_type_str = match pkg_type {
                        PackageType::Formula => "Formula",
                        PackageType::Cask => "Cask",
                    };
                    let err_msg =
                        format!("Failed {} reinstall '{}': {}", pkg_type_str, name.red(), e);
                    install_errors.push((name.clone(), e));
                    (name.clone(), false, err_msg)
                }
            };

            if !was_success {
                error!("✖ {}", message);
            } else {
                info_line(message);
            }
        }
        install_errors
    }

    /// The actual worker function performing pre-uninstall and installation.
    #[instrument(skip(job, config, cache), fields(pkg = %match &job.target {
        InstallTargetIdentifier::Formula(f) => f.name().to_string(),
        InstallTargetIdentifier::Cask(c) => c.token.clone(),
    }, action = ?job.action))]
    fn run_pipeline_job(job: PipelineJob, config: &Config, cache: Arc<Cache>) -> PipelineJobResult {
        let (name, pkg_type) = match &job.target {
            InstallTargetIdentifier::Formula(f) => (f.name().to_string(), PackageType::Formula),
            InstallTargetIdentifier::Cask(c) => (c.token.clone(), PackageType::Cask),
        };

        // --- 1. Pre-Install Step (Uninstall for Upgrade/Reinstall) ---
        let pre_install_result = match &job.action {
            PipelineActionType::Upgrade {
                from_version,
                old_install_path,
            }
            | PipelineActionType::Reinstall {
                version: from_version,
                current_install_path: old_install_path,
            } => {
                info_line(format!(
                    "Removing existing {name} version {from_version}..."
                ));
                // Construct the InstalledPackageInfo for the *old* version
                let old_info = InstalledPackageInfo {
                    name: name.clone(),
                    version: from_version.clone(),
                    pkg_type: pkg_type.clone(),
                    path: old_install_path.clone(),
                };
                let uninstall_opts = UninstallOptions { skip_zap: true }; // CRUCIAL

                // Call the appropriate core uninstall function
                match pkg_type {
                    PackageType::Formula => core_uninstall::uninstall_formula_artifacts(
                        &old_info,
                        config,
                        &uninstall_opts,
                    ),
                    PackageType::Cask => {
                        core_uninstall::uninstall_cask_artifacts(&old_info, config, &uninstall_opts)
                    }
                }
            }
            PipelineActionType::Install => Ok(()), // No pre-install step needed
        };

        if let Err(e) = pre_install_result {
            let old_version_str = match &job.action {
                PipelineActionType::Upgrade { from_version, .. } => from_version.clone(),
                PipelineActionType::Reinstall { version, .. } => version.clone(),
                _ => "[N/A]".to_string(),
            };
            error!(
                "Failed to remove old version {} for {}: {}",
                old_version_str, name, e
            );
            // Return specific error based on action type
            return match job.action {
                PipelineActionType::Upgrade { from_version, .. } => {
                    PipelineJobResult::UpgradeErr(name, pkg_type, from_version, e)
                }
                PipelineActionType::Reinstall { .. } => {
                    PipelineJobResult::ReinstallErr(name, pkg_type, e)
                }
                PipelineActionType::Install => PipelineJobResult::InstallErr(name, pkg_type, e), /* Should ideally not happen here */
            };
        }

        // --- 2. Perform Installation ---
        info_line(format!(
            "Installing {} {}...",
            pkg_type_str(pkg_type.clone()),
            name
        ));
        let install_result = Self::perform_actual_installation(&job, config, cache); // Pass job by ref

        // --- 3. Return result based on action type and install outcome ---
        match (job.action, install_result) {
            (PipelineActionType::Install, Ok(_)) => PipelineJobResult::InstallOk(name, pkg_type),
            (PipelineActionType::Install, Err(e)) => {
                PipelineJobResult::InstallErr(name, pkg_type, e)
            }
            (PipelineActionType::Upgrade { from_version, .. }, Ok(_)) => {
                PipelineJobResult::UpgradeOk(name, pkg_type, from_version)
            }
            (PipelineActionType::Upgrade { from_version, .. }, Err(e)) => {
                PipelineJobResult::UpgradeErr(name, pkg_type, from_version, e)
            }
            (PipelineActionType::Reinstall { .. }, Ok(_)) => {
                PipelineJobResult::ReinstallOk(name, pkg_type)
            }
            (PipelineActionType::Reinstall { .. }, Err(e)) => {
                PipelineJobResult::ReinstallErr(name, pkg_type, e)
            }
        }
    }

    /// Extracted core install logic (previously part of run_install).
    #[instrument(skip(job, config, _cache), fields(pkg = %match &job.target {
        InstallTargetIdentifier::Formula(f) => f.name().to_string(),
        InstallTargetIdentifier::Cask(c) => c.token.clone(),
    }))]
    fn perform_actual_installation(
        job: &PipelineJob,
        config: &Config,
        _cache: Arc<Cache>,
    ) -> Result<()> {
        match &job.target {
            InstallTargetIdentifier::Formula(formula) => {
                let install_dir = formula.install_prefix(&config.cellar)?;
                // Ensure parent exists (needed after potential uninstall)
                if let Some(parent_dir) = install_dir.parent() {
                    fs::create_dir_all(parent_dir).map_err(|e| SpsError::Io(Arc::new(e)))?;
                }

                if job.is_source_build {
                    // Source Build Logic
                    info_line(format!("Building {} from source", formula.name()));
                    let resolved_graph = job.resolved_graph.as_ref().ok_or_else(|| {
                        SpsError::Generic("Missing resolved graph for source build".to_string())
                    })?;
                    let build_dep_paths = resolved_graph.build_dependency_opt_paths.clone();
                    let runtime_dep_paths = resolved_graph.runtime_dependency_opt_paths.clone();
                    let all_dep_paths = [build_dep_paths, runtime_dep_paths].concat();

                    let build_result = block_on(build::formula::source::build_from_source(
                        &job.download_path,
                        formula, // Pass the Arc<Formula> by ref
                        config,
                        &all_dep_paths,
                    ));
                    match build_result {
                        Ok(installed_dir) => build::formula::link::link_formula_artifacts(
                            formula,
                            &installed_dir,
                            config,
                        ),
                        Err(e) => Err(e),
                    }
                } else {
                    // Bottle Install Logic
                    info_line(format!("Installing bottle for {}", formula.name()));
                    let installed_dir = build::formula::bottle::install_bottle(
                        &job.download_path,
                        formula, // Pass the Arc<Formula> by ref
                        config,
                    )?;
                    build::formula::link::link_formula_artifacts(formula, &installed_dir, config)
                }
            }
            InstallTargetIdentifier::Cask(cask) => {
                // Cask Install Logic
                info_line(format!("Installing cask {}", cask.token));
                build::cask::install_cask(cask, &job.download_path, config)
            }
        }
    }
}

// --- Helper Functions (Moved from old install.rs or new) ---

/// Downloads the target file (bottle, source, cask archive).
#[instrument(skip(cfg, cache, client), fields(name=%target_name))]
async fn download_target_file(
    target_name: &str,
    target_type: &InstallTargetIdentifier, // Borrow instead of consume
    cfg: &Config,
    cache: Arc<Cache>,
    client: Arc<reqwest::Client>,
    is_source_build: bool,
) -> Result<PathBuf> {
    debug!(
        "Starting download process for {} (source_build={})",
        target_name, is_source_build
    );
    match target_type {
        InstallTargetIdentifier::Formula(formula) => {
            if is_source_build {
                info_line(format!("Downloading source for {}", formula.name));
                build::formula::source::download_source(formula, cfg).await
            } else {
                info_line(format!("Downloading bottle {}", formula.name));
                build::formula::bottle::download_bottle(formula, cfg, client.as_ref()).await
            }
        }
        InstallTargetIdentifier::Cask(cask) => {
            info_line(format!("Downloading cask {}", cask.token));
            build::cask::download_cask(cask, cache.as_ref()).await
        }
    }
    .map_err(|e| {
        // Wrap errors nicely
        error!("Download failed for {}: {}", target_name, e);
        // Add more context if it's not already a DownloadError
        if matches!(e, SpsError::DownloadError(_, _, _)) {
            e
        } else {
            SpsError::DownloadError(
                target_name.to_string(),
                "[unknown URL]".to_string(),
                e.to_string(),
            )
        }
    })
}

// Simple green INFO logger for install actions (copied from old install.rs)
fn info_line(message: impl AsRef<str>) {
    println!("{} sp::pipeline: {}", "INFO".green(), message.as_ref()); // Indicate pipeline source
}

// Helper to get string representation of PackageType
fn pkg_type_str(pkg_type: PackageType) -> &'static str {
    match pkg_type {
        PackageType::Formula => "Formula",
        PackageType::Cask => "Cask",
    }
}

async fn load_or_fetch_json(
    cache: &Cache,
    filename: &str,
    api_fetcher: impl std::future::Future<Output = Result<String>>,
) -> Result<Vec<Value>> {
    match cache.load_raw(filename) {
        Ok(data) => {
            debug!("Loaded {} from cache.", filename);
            serde_json::from_str(&data).map_err(|e| {
                error!("Failed to parse cached {}: {}", filename, e);
                SpsError::Cache(format!("Failed parse cached {filename}: {e}"))
            })
        }
        Err(_) => {
            debug!("Cache miss for {}, fetching from API...", filename);
            let raw_data = api_fetcher.await?;
            if let Err(cache_err) = cache.store_raw(filename, &raw_data) {
                warn!(
                    "Failed to cache {} data after fetching: {}",
                    filename, cache_err
                );
            } else {
                debug!("Successfully cached {} after fetching.", filename);
            }
            serde_json::from_str(&raw_data).map_err(|e| SpsError::Json(Arc::new(e)))
        }
    }
}

// Add helper function for sorting jobs by dependency order
fn sort_jobs_by_dependency_order(jobs: &mut [PipelineJob], graph: &ResolvedGraph) {
    let formula_order: HashMap<String, usize> = graph
        .install_plan
        .iter()
        .enumerate()
        .map(|(idx, dep)| (dep.formula.name().to_string(), idx))
        .collect();

    jobs.sort_by_key(|job| match &job.target {
        InstallTargetIdentifier::Formula(f) => {
            formula_order.get(f.name()).copied().unwrap_or(usize::MAX)
        }
        InstallTargetIdentifier::Cask(_) => usize::MAX, // Install casks after formulae
    });
}
