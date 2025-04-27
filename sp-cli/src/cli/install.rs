// FILE: sp-cli/src/cli/install/mod.rs
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use colored::Colorize;
use crossbeam_channel::{bounded, Receiver, Sender};
use futures::executor::block_on;
use num_cpus;
use serde_json::Value;
use sp_core::build;
use sp_core::dependency::{DependencyResolver, ResolutionContext, ResolutionStatus, ResolvedGraph};
use sp_core::fetch::api;
use sp_core::formulary::Formulary;
use sp_core::keg::KegRegistry;
use sp_core::model::formula::FormulaDependencies;
use sp_core::model::{Cask, Formula};
use sp_core::utils::cache::Cache;
use sp_core::utils::config::Config;
use sp_core::utils::error::{Result, SpError};
use threadpool::ThreadPool;
use tokio::task::JoinSet;
use tokio::try_join;
use tracing::{debug, error, instrument, warn, Instrument};

#[derive(Debug, Args)]
pub struct Install {
    #[arg(required = true)]
    names: Vec<String>,
    #[arg(long)]
    skip_deps: bool,
    #[arg(long, help = "Force install specified targets as casks")]
    cask: bool,
    #[arg(long, help = "Force install specified targets as formulas")]
    formula: bool,
    #[arg(long)]
    include_optional: bool,
    #[arg(long)]
    skip_recommended: bool,
    #[arg(long, value_name = "SP_WORKERS")]
    max_workers: Option<usize>,
    #[arg(long, value_name = "SP_QUEUE")]
    queue_size: Option<usize>,
    #[arg(
        long,
        help = "Force building the formula from source, even if a bottle is available"
    )]
    build_from_source: bool,
}

#[derive(Debug, Default)]
struct InstallPlanInput {
    formulae_names: HashSet<String>,
    cask_names: HashSet<String>,
    unknown_targets: Vec<String>,
    initial_errors: Vec<(String, SpError)>,
}

#[derive(Debug)]
pub enum InstallTarget {
    Formula(Arc<Formula>),
    Cask(Arc<Cask>),
}

#[derive(Debug)]
pub struct InstallJob {
    target: InstallTarget,
    download_path: PathBuf,
    resolved_graph: Option<Arc<ResolvedGraph>>,
    is_source_build: bool, // added flag to indicate source build
}

#[derive(Debug)]
pub enum JobResult {
    FormulaOk(String),
    CaskOk(String),
    FormulaErr(String, SpError),
    CaskErr(String, SpError),
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
                SpError::Cache(format!("Failed parse cached {filename}: {e}"))
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
            serde_json::from_str(&raw_data).map_err(|e| SpError::Json(Arc::new(e)))
        }
    }
}

// Simple green INFO logger for install actions
fn info_line(message: impl AsRef<str>) {
    // simple INFO logger: prints “INFO sp::install: <message>” in green
    println!("{} sp::install: {}", "INFO".green(), message.as_ref());
}

impl Install {
    #[instrument(skip(self, cfg, cache), fields(targets = ?self.names))]
    pub async fn run(&self, cfg: &Config, cache: Arc<Cache>) -> Result<()> {
        if self.skip_deps {
            warn!("--skip-deps is partially supported; mandatory dependencies are still processed for formulae.");
        }
        if self.formula && self.cask {
            return Err(SpError::Generic(
                "Cannot use --formula and --cask together.".to_string(),
            ));
        }

        let plan_input = self
            .gather_full_dependency_set(cfg, Arc::clone(&cache))
            .await?;
        let mut overall_errors: Vec<(String, SpError)> = plan_input.initial_errors.clone();

        for name in &plan_input.unknown_targets {
            let msg = format!("Target '{name}' not found as a formula or cask.");
            error!("✖ {msg}");
            if !overall_errors.iter().any(|(n, _)| n == name) {
                overall_errors.push((name.clone(), SpError::NotFound(msg)));
            }
        }
        for (name, err) in &plan_input.initial_errors {
            error!("✖ Error processing target '{}': {}", name.cyan(), err);
        }

        let mut resolved_formula_graph: Option<Arc<ResolvedGraph>> = None;
        let formula_list_to_resolve: Vec<String> =
            plan_input.formulae_names.iter().cloned().collect();

        if !formula_list_to_resolve.is_empty() {
            let formulary = Formulary::new(cfg.clone());
            let keg_registry = KegRegistry::new(cfg.clone());
            let ctx = ResolutionContext {
                formulary: &formulary,
                keg_registry: &keg_registry,
                sp_prefix: cfg.prefix(),
                include_optional: self.include_optional,
                include_test: false,
                skip_recommended: self.skip_recommended,
                force_build: self.build_from_source,
            };
            let mut resolver = DependencyResolver::new(ctx);

            match resolver.resolve_targets(&formula_list_to_resolve) {
                Ok(graph) => {
                    debug!("Dependency resolution successful.");
                    for target_name in &self.names {
                        if plan_input.formulae_names.contains(target_name) {
                            if let Some(details) = graph.resolution_details.get(target_name) {
                                if matches!(
                                    details.status,
                                    ResolutionStatus::NotFound | ResolutionStatus::Failed
                                ) {
                                    let reason =
                                        details.failure_reason.clone().unwrap_or_else(|| {
                                            format!(
                                                "Resolution failed with status {:?}",
                                                details.status
                                            )
                                        });
                                    let msg = format!("Failed to resolve formula target '{target_name}': {reason}");
                                    error!("✖ {msg}");
                                    if !overall_errors.iter().any(|(n, _)| n == target_name) {
                                        overall_errors.push((
                                            target_name.clone(),
                                            SpError::DependencyError(reason),
                                        ));
                                    }
                                }
                            } else if !graph
                                .install_plan
                                .iter()
                                .any(|d| d.formula.name() == target_name)
                                && keg_registry.get_installed_keg(target_name)?.is_none()
                            {
                                let msg = format!("Formula target '{target_name}' was unexpectedly missing from the resolution results.");
                                error!("✖ {msg}");
                                if !overall_errors.iter().any(|(n, _)| n == target_name) {
                                    overall_errors
                                        .push((target_name.clone(), SpError::Generic(msg)));
                                }
                            }
                        }
                    }
                    resolved_formula_graph = Some(Arc::new(graph));
                }
                Err(e) => {
                    let msg = format!("Fatal dependency resolution error: {e}");
                    error!("✖ {msg}");
                    for name in formula_list_to_resolve {
                        if !overall_errors.iter().any(|(n, _)| n == &name) {
                            overall_errors
                                .push((name.clone(), SpError::DependencyError(msg.clone())));
                        }
                    }
                    return Err(SpError::InstallError(msg));
                }
            }
        } else {
            debug!("No formulae identified for resolution.");
        }

        let mut tasks_to_download: Vec<(String, InstallTargetIdentifier)> = Vec::new();
        let mut already_installed: HashSet<String> = HashSet::new();

        if let Some(graph_arc) = &resolved_formula_graph {
            let keg_registry = KegRegistry::new(cfg.clone());
            for dep in &graph_arc.install_plan {
                let name = dep.formula.name();
                if overall_errors.iter().any(|(n, _)| n == name) {
                    debug!("Skipping formula {} due to previous error.", name);
                    continue;
                }
                if matches!(
                    dep.status,
                    ResolutionStatus::Missing | ResolutionStatus::Requested
                ) {
                    tasks_to_download.push((
                        name.to_string(),
                        InstallTargetIdentifier::Formula(dep.formula.clone()),
                    ));
                } else if matches!(dep.status, ResolutionStatus::Installed)
                    && self.names.contains(&name.to_string())
                {
                    already_installed.insert(name.to_string());
                }
            }
            for name in plan_input.formulae_names.iter() {
                if self.names.contains(name)
                    && !tasks_to_download.iter().any(|(n, _)| n == name)
                    && !already_installed.contains(name)
                {
                    if let Some(keg) = keg_registry.get_installed_keg(name)? {
                        if let Some(details) = graph_arc.resolution_details.get(name) {
                            if keg.version == *details.formula.version()
                                && keg.revision == details.formula.revision
                            {
                                already_installed.insert(name.to_string());
                            }
                        }
                    }
                }
            }
        }

        for name in &plan_input.cask_names {
            if overall_errors.iter().any(|(n, _)| n == name) {
                debug!("Skipping cask {} due to previous error.", name);
                continue;
            }
            match api::get_cask(name).await {
                Ok(cask_model) => {
                    if cask_model.is_installed(cfg) {
                        if self.names.contains(name) {
                            already_installed.insert(name.to_string());
                        }
                        continue;
                    }
                    if !already_installed.contains(name)
                        && !tasks_to_download.iter().any(|(n, _)| n == name)
                    {
                        tasks_to_download.push((
                            name.clone(),
                            InstallTargetIdentifier::Cask(Arc::new(cask_model)),
                        ));
                    }
                }
                Err(e) => {
                    warn!(
                        "Could not get cask info for {}: {}. Assuming not installed for now.",
                        name, e
                    );
                    if !already_installed.contains(name)
                        && !tasks_to_download.iter().any(|(n, _)| n == name)
                    {
                        let msg = format!("Failed to get info for cask '{name}'");
                        if !overall_errors.iter().any(|(n, _)| n == name) {
                            overall_errors.push((name.clone(), SpError::NotFound(msg)));
                        }
                    }
                }
            }
        }

        if tasks_to_download.is_empty() && overall_errors.is_empty() {
            return Ok(());
        }
        if tasks_to_download.is_empty() && !overall_errors.is_empty() {
            error!("No packages to install due to previous errors.");
            let final_error_msg = overall_errors
                .into_iter()
                .map(|(name, err)| format!("'{name}': {err}"))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(SpError::InstallError(format!(
                "Installation failed during planning: {final_error_msg}"
            )));
        }

        let worker_count = self
            .max_workers
            .unwrap_or_else(|| std::cmp::max(1, num_cpus::get_physical().saturating_sub(1)).min(6));
        let queue_size = self.queue_size.unwrap_or(worker_count * 2);

        let (job_tx, job_rx): (Sender<InstallJob>, Receiver<InstallJob>) = bounded(queue_size);
        let (result_tx, result_rx): (Sender<JobResult>, Receiver<JobResult>) = bounded(queue_size);
        let pool = ThreadPool::new(worker_count);

        let client = Arc::new(reqwest::Client::new());

        let mut download_join_set: JoinSet<Result<(InstallJob, String)>> = JoinSet::new();

        for (name, target_type) in tasks_to_download {
            let cfg_clone = cfg.clone();
            let cache_clone = Arc::clone(&cache);
            let client_clone = Arc::clone(&client);
            let graph_clone = resolved_formula_graph.clone();
            let build_from_source = self.build_from_source;
            download_join_set.spawn(
                download_target(
                    name,
                    target_type,
                    cfg_clone,
                    cache_clone,
                    client_clone,
                    graph_clone,
                    build_from_source,
                )
                .in_current_span(),
            );
        }

        let pump_handle = tokio::spawn({
            let pool_clone = pool.clone();
            let cfg_clone = cfg.clone();
            let result_tx_clone = result_tx.clone();
            let cache_clone = Arc::clone(&cache);
            async move {
                while let Ok(job) = job_rx.recv() {
                    let pkg_name = match &job.target {
                        InstallTarget::Formula(f) => f.name().to_string(),
                        InstallTarget::Cask(c) => c.token.clone(),
                    };
                    let res_tx = result_tx_clone.clone();
                    let worker_cfg = cfg_clone.clone();
                    let worker_cache = Arc::clone(&cache_clone);
                    let install_span = tracing::info_span!("install_worker", pkg = %pkg_name);
                    pool_clone.execute(move || {
                        install_span.in_scope(|| {
                            let result = run_install(job, &worker_cfg, worker_cache);
                            if res_tx.send(result).is_err() {
                                warn!(
                                    "Result channel closed, could not send install result for {}.",
                                    pkg_name
                                );
                            }
                        });
                    });
                }
                debug!("Job channel closed and drained, queue pump task finishing.");
            }
            .in_current_span()
        });

        let mut download_errors = 0;
        while let Some(result) = download_join_set.join_next().await {
            match result {
                Ok(Ok((install_job, name))) => {
                    // send the job without blocking the Tokio worker thread
                    let tx_res = tokio::task::spawn_blocking({
                        let job_tx = job_tx.clone();
                        move || job_tx.send(install_job)
                    })
                    .await
                    .expect("spawn_blocking panicked");

                    if tx_res.is_err() {
                        error!(
                            "Job channel closed unexpectedly while sending job for {}.",
                            name
                        );
                        download_errors += 1;
                    }
                }
                Ok(Err(e)) => {
                    let name = match &e {
                        SpError::DownloadError(n, _, _) => n.clone(),
                        _ => "[unknown]".to_string(),
                    };
                    error!(
                        "✖ Failed to prepare install job for '{}': {}",
                        name.cyan(),
                        e
                    );
                    if name != "[unknown]" && !overall_errors.iter().any(|(n, _)| n == &name) {
                        overall_errors.push((name.clone(), e));
                    }
                    download_errors += 1;
                }
                Err(join_error) => {
                    error!("✖ Download task panicked: {}", join_error);
                    download_errors += 1;
                }
            }
        }
        debug!(
            "All download tasks completed. {} download errors.",
            download_errors
        );
        drop(job_tx);

        drop(result_tx);
        let mut _install_success_count = 0usize;
        let mut _install_error_count = 0usize;
        while let Ok(result) = result_rx.recv() {
            let (_name, was_success, message) = match result {
                JobResult::FormulaOk(name) => {
                    _install_success_count += 1;
                    let message = format!("Installed Formula {}", name.green());
                    (name, true, message)
                }
                JobResult::CaskOk(token) => {
                    _install_success_count += 1;
                    let message = format!("Installed Cask {}", token.green());
                    (token, true, message)
                }
                JobResult::FormulaErr(name, e) => {
                    _install_error_count += 1;
                    let err_msg = format!("Failed Formula '{}': {}", name.red(), e);
                    if !overall_errors.iter().any(|(n, _)| n == &name) {
                        overall_errors.push((name.clone(), e));
                    }
                    (name, false, err_msg)
                }
                JobResult::CaskErr(token, e) => {
                    _install_error_count += 1;
                    let err_msg = format!("Failed Cask '{}': {}", token.red(), e);
                    if !overall_errors.iter().any(|(n, _)| n == &token) {
                        overall_errors.push((token.clone(), e));
                    }
                    (token, false, err_msg)
                }
            };
            if !was_success {
                error!("{}", message);
            } else {
                debug!("{}", message);
            }
        }
        debug!("Result channel closed, finished collecting install results.");

        if let Err(e) = pump_handle.await {
            error!("Queue pump task panicked: {}", e);
        }

        if overall_errors.is_empty() {
            Ok(())
        } else {
            error!(
                "Installation completed with {} error(s).",
                overall_errors.len()
            );
            let final_error_msg = overall_errors
                .into_iter()
                .map(|(name, err)| format!("'{name}': {err}"))
                .collect::<Vec<_>>()
                .join("; ");
            Err(SpError::InstallError(format!(
                "Installation failed for one or more targets: {final_error_msg}"
            )))
        }
    }

    #[instrument(skip(self, _cfg, cache))]
    async fn gather_full_dependency_set(
        &self,
        _cfg: &Config,
        cache: Arc<Cache>,
    ) -> Result<InstallPlanInput> {
        let mut plan_input = InstallPlanInput::default();
        let targets_to_check: HashSet<String> = self.names.iter().cloned().collect();

        if self.formula {
            plan_input.formulae_names = targets_to_check;
            return Ok(plan_input);
        }
        if self.cask {
            plan_input.cask_names = targets_to_check;
        } else {
            let formula_data_future =
                load_or_fetch_json(&cache, "formula.json", api::fetch_all_formulas());
            let cask_data_future = load_or_fetch_json(&cache, "cask.json", api::fetch_all_casks());
            let (formula_values, cask_values) =
                match try_join!(formula_data_future, cask_data_future) {
                    Ok((f, c)) => (f, c),
                    Err(e) => {
                        error!("Failed to load core package data: {}", e);
                        return Err(SpError::InstallError(format!(
                            "Failed to load required package lists: {e}"
                        )));
                    }
                };
            let known_formulae: HashSet<String> = formula_values
                .iter()
                .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect();
            let known_casks: HashSet<String> = cask_values
                .iter()
                .filter_map(|c| c.get("token").and_then(|t| t.as_str()).map(String::from))
                .collect();
            for name in &self.names {
                if known_formulae.contains(name) {
                    plan_input.formulae_names.insert(name.clone());
                } else if known_casks.contains(name) {
                    plan_input.cask_names.insert(name.clone());
                } else {
                    plan_input.unknown_targets.push(name.clone());
                    let msg =
                        format!("Target '{name}' not found as a formula or cask in local index.");
                    if !plan_input.initial_errors.iter().any(|(n, _)| n == name) {
                        plan_input
                            .initial_errors
                            .push((name.clone(), SpError::NotFound(msg)));
                    }
                }
            }
        }

        if !plan_input.cask_names.is_empty() {
            let mut cask_queue: VecDeque<String> = plan_input.cask_names.iter().cloned().collect();
            let mut processed_casks: HashSet<String> = plan_input.cask_names.clone();
            let mut cask_fetch_errors: Vec<(String, SpError)> = Vec::new();
            while let Some(token) = cask_queue.pop_front() {
                if plan_input.initial_errors.iter().any(|(n, _)| n == &token) {
                    continue;
                }
                match api::get_cask(&token).await {
                    Ok(cask) => {
                        if let Some(deps) = &cask.depends_on {
                            for formula_dep in &deps.formula {
                                if plan_input.formulae_names.insert(formula_dep.clone()) {
                                    debug!(
                                        "Added formula dependency from cask '{}': {}",
                                        token, formula_dep
                                    );
                                }
                            }
                            for cask_dep in &deps.cask {
                                if processed_casks.insert(cask_dep.clone()) {
                                    debug!(
                                        "Added cask dependency from cask '{}': {}",
                                        token, cask_dep
                                    );
                                    plan_input.cask_names.insert(cask_dep.clone());
                                    cask_queue.push_back(cask_dep.clone());
                                }
                            }
                        }
                    }
                    Err(e @ SpError::NotFound(_))
                    | Err(e @ SpError::Json(_))
                    | Err(e @ SpError::Api(_)) => {
                        let msg = format!("Failed to get info for dependency cask '{token}': {e}");
                        if !cask_fetch_errors.iter().any(|(n, _)| n == &token) {
                            warn!("✖ {}", msg);
                            cask_fetch_errors.push((token.clone(), e));
                        }
                    }
                    Err(e) => {
                        let msg = format!("Error fetching dependency info for cask '{token}': {e}");
                        if !cask_fetch_errors.iter().any(|(n, _)| n == &token) {
                            error!("✖ {}", msg);
                            cask_fetch_errors.push((token.clone(), e));
                        }
                    }
                }
            }
            plan_input.initial_errors.extend(cask_fetch_errors);
        }

        debug!(
            "Final plan input: Formulae={:?}, Casks={:?}, Unknown={:?}, Errors={:?}",
            plan_input.formulae_names,
            plan_input.cask_names,
            plan_input.unknown_targets,
            plan_input.initial_errors.len()
        );
        Ok(plan_input)
    }
}

#[derive(Debug, Clone)]
enum InstallTargetIdentifier {
    Formula(Arc<Formula>),
    Cask(Arc<Cask>),
}

#[instrument(skip(cfg, cache, client, resolved_graph_option), fields(name=%target_name))]
async fn download_target(
    target_name: String,
    target_type: InstallTargetIdentifier,
    cfg: Config,
    cache: Arc<Cache>,
    client: Arc<reqwest::Client>,
    resolved_graph_option: Option<Arc<ResolvedGraph>>,
    build_from_source: bool,
) -> Result<(InstallJob, String)> {
    debug!("Starting download process");
    match target_type {
        InstallTargetIdentifier::Formula(formula) => {
            let needs_source_build =
                build_from_source || !build::formula::has_bottle_for_current_platform(&formula);
            let download_path_result = if needs_source_build {
                info_line(format!(
                    "{} requires source build, downloading source",
                    formula.name
                ));
                build::formula::source::download_source(&formula, &cfg).await
            } else {
                info_line(format!("Downloading bottle {}", formula.name));
                build::formula::bottle::download_bottle(&formula, &cfg, client.as_ref()).await
            };
            match download_path_result {
                Ok(download_path) => {
                    debug!("Download successful: {}", download_path.display());
                    Ok((
                        InstallJob {
                            target: InstallTarget::Formula(formula.clone()),
                            download_path,
                            resolved_graph: if needs_source_build {
                                resolved_graph_option.clone()
                            } else {
                                None
                            },
                            is_source_build: needs_source_build,
                        },
                        target_name,
                    ))
                }
                Err(e) => {
                    error!("Download failed: {}", e);
                    Err(SpError::DownloadError(
                        target_name.clone(),
                        formula.url.clone(),
                        e.to_string(),
                    ))
                }
            }
        }
        InstallTargetIdentifier::Cask(cask) => {
            info_line(format!("Downloading cask {}", cask.token));
            match build::cask::download_cask(&cask, cache.as_ref()).await {
                Ok(download_path) => {
                    debug!("Download successful: {}", download_path.display());
                    Ok((
                        InstallJob {
                            target: InstallTarget::Cask(cask.clone()),
                            download_path,
                            resolved_graph: None,
                            is_source_build: false,
                        },
                        target_name,
                    ))
                }
                Err(e) => {
                    error!("Download failed: {}", e);
                    Err(e)
                }
            }
        }
    }
}

#[instrument(skip(job, config, _cache), fields(pkg = %match &job.target {
	InstallTarget::Formula(f) => f.name().to_string(),
	InstallTarget::Cask(c) => c.token.clone(),
}))]
fn run_install(job: InstallJob, config: &Config, _cache: Arc<Cache>) -> JobResult {
    match job.target {
        InstallTarget::Formula(formula) => {
            let formula_name = formula.name().to_string();
            let result = || -> Result<()> {
                let needs_source_build = job.is_source_build; // use the new flag
                let install_dir = formula.install_prefix(&config.cellar)?;
                if install_dir.exists() {
                    debug!(
                        "Removing existing installation at {}",
                        install_dir.display()
                    );
                    fs::remove_dir_all(&install_dir)?;
                }
                if let Some(parent_dir) = install_dir.parent() {
                    fs::create_dir_all(parent_dir).map_err(|e| SpError::Io(Arc::new(e)))?;
                }
                let install_res = if needs_source_build {
                    info_line(format!("Building {} from source", formula.name));
                    let source_path = job.download_path;
                    let resolved_graph = job.resolved_graph.ok_or_else(|| {
                        SpError::Generic("Missing resolved graph for source build".to_string())
                    })?;
                    let build_dep_paths = resolved_graph.build_dependency_opt_paths.clone();
                    let runtime_dep_paths = resolved_graph.runtime_dependency_opt_paths.clone();
                    let all_dep_paths = [build_dep_paths, runtime_dep_paths].concat();
                    match block_on(build::formula::source::build_from_source(
                        &source_path,
                        &formula,
                        config,
                        &all_dep_paths,
                    )) {
                        Ok(dir) => {
                            build::formula::link::link_formula_artifacts(&formula, &dir, config)
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    info_line(format!("Installing bottle for {}", formula.name));
                    let install_dir = build::formula::bottle::install_bottle(
                        &job.download_path,
                        &formula,
                        config,
                    )?;
                    build::formula::link::link_formula_artifacts(&formula, &install_dir, config)
                };
                install_res
            }();
            match result {
                Ok(_) => {
                    info_line(format!(
                        "Successfully installed: {} ({})",
                        formula_name,
                        formula
                            .install_prefix(&config.cellar)
                            .unwrap_or_default()
                            .display()
                    ));
                    JobResult::FormulaOk(formula_name)
                }
                Err(e) => {
                    error!("Installation failed: {}", e);
                    JobResult::FormulaErr(formula_name, e)
                }
            }
        }
        InstallTarget::Cask(cask) => {
            let cask_token = cask.token.clone();
            info_line(format!("Installing cask {}", cask.token));
            let install_result = build::cask::install_cask(&cask, &job.download_path, config);
            match install_result {
                Ok(()) => {
                    info_line(format!("Successfully installed: {cask_token}"));
                    JobResult::CaskOk(cask_token)
                }
                Err(e) => {
                    error!("Cask installation failed: {}", e);
                    if matches!(&e, SpError::InstallError(msg) if msg.contains("already installed"))
                    {
                        JobResult::CaskOk(cask_token)
                    } else {
                        JobResult::CaskErr(cask_token, e)
                    }
                }
            }
        }
    }
}
