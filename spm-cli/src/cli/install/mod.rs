// FILE: spm-cli/src/cli/install/mod.rs
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use colored::Colorize;
use serde_json::Value; // Needed for parsing cached JSON
use spm_core::dependency::{
    DependencyResolver, ResolutionContext, ResolutionStatus, ResolvedGraph,
};
use spm_core::fetch::api;
use spm_core::formulary::Formulary;
use spm_core::keg::KegRegistry;
use spm_core::utils::cache::Cache;
use spm_core::utils::config::Config;
use spm_core::utils::error::{Result, SpmError};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::try_join; // Needed for concurrent cache loading
use tracing::{debug, error, info, warn};

mod cask;
mod formula;

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
    #[arg(long, default_value_t = 4)]
    max_concurrent_installs: usize,
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
    initial_errors: Vec<(String, SpmError)>,
}

enum TaskType {
    Bottle(formula::FormulaInstallInfo),
    Source(formula::FormulaInstallInfo),
    Cask(String),
}

// Helper function to load and parse cached JSON, falling back to API
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
                SpmError::Cache(format!("Failed parse cached {filename}: {e}"))
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
            serde_json::from_str(&raw_data).map_err(|e| {
                error!("Failed to parse API response for {}: {}", filename, e);
                SpmError::Json(Arc::new(e))
            })
        }
    }
}

impl Install {
    pub async fn run(&self, cfg: &Config, cache: Arc<Cache>) -> Result<()> {
        if self.skip_deps {
            warn!("--skip-deps is partially supported; mandatory dependencies are still processed for formulae.");
        }
        if self.formula && self.cask {
            return Err(SpmError::Generic(
                "Cannot use --formula and --cask together.".to_string(),
            ));
        }

        let plan_input = self
            .gather_full_dependency_set(cfg, Arc::clone(&cache))
            .await?;
        let mut overall_errors: Vec<String> = Vec::new();

        for (name, err) in &plan_input.initial_errors {
            let msg = format!("Error processing target '{name}': {err}");
            error!("✖ {msg}");
            overall_errors.push(msg);
        }
        for name in &plan_input.unknown_targets {
            let msg = format!("Target '{name}' not found as a formula or cask.");
            error!("✖ {msg}");
            overall_errors.push(msg);
        }

        let mut resolved_formula_graph: Option<ResolvedGraph> = None;
        let formula_list_to_resolve: Vec<String> =
            plan_input.formulae_names.iter().cloned().collect();

        if !formula_list_to_resolve.is_empty() {
            debug!(
                "Resolving dependencies for {} potential formula(e)",
                formula_list_to_resolve.len()
            );
            let formulary = Formulary::new(cfg.clone());
            let keg_registry = KegRegistry::new(cfg.clone());
            let ctx = ResolutionContext {
                formulary: &formulary,
                keg_registry: &keg_registry,
                spm_prefix: cfg.prefix(),
                include_optional: self.include_optional,
                include_test: false,
                skip_recommended: self.skip_recommended,
                force_build: self.build_from_source,
            };
            let mut resolver = DependencyResolver::new(ctx);

            match resolver.resolve_targets(&formula_list_to_resolve) {
                Ok(graph) => {
                    debug!(
                        "Dependency resolution successful. Install plan includes {} formulae.",
                        graph.install_plan.len()
                    );
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
                                    overall_errors.push(msg);
                                }
                            } else if !graph
                                .install_plan
                                .iter()
                                .any(|d| d.formula.name() == target_name)
                                && keg_registry.get_installed_keg(target_name)?.is_none()
                            {
                                let msg = format!("Formula target '{target_name}' was unexpectedly missing from the resolution results.");
                                error!("✖ {msg}");
                                overall_errors.push(msg);
                            }
                        }
                    }
                    resolved_formula_graph = Some(graph);
                }
                Err(e) => {
                    let msg = format!("Fatal dependency resolution error: {e}");
                    error!("✖ {msg}");
                    overall_errors.push(msg.clone());
                    return Err(SpmError::InstallError(msg));
                }
            }
        } else {
            debug!("No formulae identified for resolution.");
        }

        let mut tasks_to_run: HashMap<String, TaskType> = HashMap::new();

        if let Some(graph) = &resolved_formula_graph {
            let graph_arc = Arc::new(graph.clone());
            for dep in &graph.install_plan {
                if matches!(
                    dep.status,
                    ResolutionStatus::Missing | ResolutionStatus::Requested
                ) && !overall_errors
                    .iter()
                    .any(|e| e.contains(&format!("'{}'", dep.formula.name())))
                {
                    let needs_source_build = self.build_from_source
                        || !spm_core::build::formula::has_bottle_for_current_platform(&dep.formula);

                    let task_info = formula::FormulaInstallInfo {
                        formula: dep.formula.clone(),
                        resolved_graph: Arc::clone(&graph_arc),
                    };
                    let task_type = if needs_source_build {
                        TaskType::Source(task_info)
                    } else {
                        TaskType::Bottle(task_info)
                    };
                    if !tasks_to_run.contains_key(dep.formula.name()) {
                        tasks_to_run.insert(dep.formula.name().to_string(), task_type);
                    } else {
                        debug!("Formula {} already scheduled from previous step, skipping duplicate add.", dep.formula.name());
                    }
                }
            }
        }

        for name in &plan_input.cask_names {
            if self.cask || !tasks_to_run.contains_key(name) {
                if !plan_input.initial_errors.iter().any(|(n, _)| n == name)
                    && !plan_input.unknown_targets.contains(name)
                {
                    tasks_to_run.insert(name.clone(), TaskType::Cask(name.clone()));
                } else {
                    debug!("Skipping adding cask task for '{}' due to previous error or unknown status.", name);
                }
            } else if tasks_to_run.contains_key(name) && !self.cask {
                info!("Target '{}' identified as both Formula and Cask. Prioritizing Formula install.", name);
            }
        }

        let task_list: Vec<(String, TaskType)> = tasks_to_run.into_iter().collect();
        if task_list.is_empty() && overall_errors.is_empty() {
            info!(
                "{}",
                "All requested packages are already installed and up-to-date.".yellow()
            );
            return Ok(());
        }
        if task_list.is_empty() && !overall_errors.is_empty() {
            error!("No packages to install due to initial errors.");
            return Err(SpmError::InstallError(format!(
                "Installation failed during planning: {}",
                overall_errors.join("; ")
            )));
        }

        info!(
            "Target with dependencies added: {:?}",
            task_list.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );

        let sem = Arc::new(Semaphore::new(self.max_concurrent_installs));
        let mut js: JoinSet<(String, Result<PathBuf>)> = JoinSet::new();
        let client = Arc::new(reqwest::Client::new());

        let mut pending_tasks = task_list.len();
        let mut task_results: HashMap<String, Result<PathBuf>> = HashMap::new();

        let mut task_iter = task_list.into_iter();

        while pending_tasks > 0 {
            while let Ok(permit) = sem.clone().try_acquire_owned() {
                if let Some((task_name, task)) = task_iter.next() {
                    let task_cfg = cfg.clone();
                    let task_cache = Arc::clone(&cache);
                    let task_client = Arc::clone(&client);

                    js.spawn(async move {
                        let (name, result) = match task {
                            TaskType::Bottle(info) => (
                                task_name,
                                formula::bottle::run_bottle_install(info, task_cfg, task_client)
                                    .await,
                            ),
                            TaskType::Source(info) => (
                                task_name,
                                formula::source::run_source_install(info, task_cfg).await,
                            ),
                            TaskType::Cask(name) => {
                                let cask_result =
                                    cask::run_cask_install(&name, task_cache, &task_cfg)
                                        .await
                                        .map(|_| PathBuf::new());
                                (name, cask_result)
                            }
                        };
                        drop(permit);
                        (name, result)
                    });
                } else {
                    drop(permit);
                    break;
                }
            }

            if js.is_empty() && pending_tasks > 0 {
                error!(
                    "Installation stalled: No running tasks but {} tasks pending.",
                    pending_tasks
                );
                break;
            } else if !js.is_empty() {
                match js.join_next().await {
                    Some(Ok((name, result))) => {
                        debug!("Task completed for '{}': {:?}", name, result.is_ok());
                        if let Err(SpmError::InstallError(ref msg)) = result {
                            if msg.contains("already installed") {
                                info!("Skipping installed cask: {}", name.yellow());
                                task_results.insert(name, Ok(PathBuf::new()));
                            } else {
                                task_results.insert(name, result);
                            }
                        } else {
                            task_results.insert(name, result);
                        }
                        pending_tasks -= 1;
                    }
                    Some(Err(e)) => {
                        let msg = format!("Installation task panicked: {e}");
                        error!("✖ {msg}");
                        overall_errors.push(msg);
                        pending_tasks -= 1;
                    }
                    None => {
                        if pending_tasks > 0 {
                            warn!(
                                "JoinSet finished but {} tasks were still marked as pending.",
                                pending_tasks
                            );
                            pending_tasks = 0;
                        }
                    }
                }
            } else {
                break;
            }
        }

        let mut final_success = true;

        for (name, result) in task_results {
            match result {
                Ok(path) => {
                    if path.as_os_str().is_empty() {
                        info!("Successfully installed: {}", name.green());
                    } else {
                        info!(
                            "Successfully installed: {} ({})",
                            name.green(),
                            path.display()
                        );
                    }
                }
                Err(e) => {
                    if !matches!(&e, SpmError::InstallError(msg) if msg.contains("already installed"))
                    {
                        let msg = format!("Installation failed for '{}': {e}", name.red());
                        error!("✖ {msg}");
                        if !overall_errors.iter().any(|e_str| e_str.contains(&name)) {
                            overall_errors.push(msg);
                        }
                        final_success = false;
                    }
                }
            }
        }

        if final_success && overall_errors.is_empty() {
            info!(
                "{}",
                "All installations completed successfully.".green().bold()
            );
            Ok(())
        } else {
            error!("Installation completed with errors.");
            Err(SpmError::InstallError(format!(
                "Installation failed for one or more targets: {}",
                overall_errors.join("; ")
            )))
        }
    }

    // ** MODIFIED FUNCTION USING CACHE FOR CLASSIFICATION **
    async fn gather_full_dependency_set(
        &self,
        _cfg: &Config, // Keep cfg if needed for cask dep resolution later
        cache: Arc<Cache>,
    ) -> Result<InstallPlanInput> {
        let mut plan_input = InstallPlanInput::default();
        let targets_to_check: HashSet<String> = self.names.iter().cloned().collect();

        if self.formula {
            info!("--formula: Treating all targets as formulae");
            plan_input.formulae_names = targets_to_check;
            return Ok(plan_input);
        }
        if self.cask {
            info!("--cask: Treating all targets as casks.");
            plan_input.cask_names = targets_to_check;
            // Proceed to fetch cask deps below
        } else {
            info!("Initial installation target(s): {:?}", self.names);

            // Load formula and cask data concurrently from cache/API
            let formula_data_future =
                load_or_fetch_json(&cache, "formula.json", api::fetch_all_formulas());
            let cask_data_future = load_or_fetch_json(&cache, "cask.json", api::fetch_all_casks());

            let (formula_values, cask_values) =
                match try_join!(formula_data_future, cask_data_future) {
                    Ok((f, c)) => (f, c),
                    Err(e) => {
                        error!("Failed to load core package data: {}", e);
                        // Cannot proceed without the lists for classification
                        return Err(SpmError::InstallError(format!(
                            "Failed to load required package lists: {e}"
                        )));
                    }
                };

            // Build lookup sets from cached data
            let known_formulae: HashSet<String> = formula_values
                .iter()
                .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect();
            let known_casks: HashSet<String> = cask_values
                .iter()
                .filter_map(|c| c.get("token").and_then(|t| t.as_str()).map(String::from))
                .collect();

            // Classify targets using the local sets
            for name in &self.names {
                if known_formulae.contains(name) {
                    plan_input.formulae_names.insert(name.clone());
                } else if known_casks.contains(name) {
                    plan_input.cask_names.insert(name.clone());
                } else {
                    plan_input.unknown_targets.push(name.clone());
                    // Add error immediately for unknown initial targets
                    let msg =
                        format!("Target '{name}' not found as a formula or cask in local index.");
                    plan_input
                        .initial_errors
                        .push((name.clone(), SpmError::NotFound(msg)));
                }
            }
        }

        // --- Recursively Gather Cask Dependencies (Still requires API for now) ---
        // This part still needs network calls to get dependency info for *specific* casks
        // We could optimize this further by parsing dependencies from the cached cask.json too,
        // but let's keep the API call here for now as it handles complex cases.
        let mut cask_queue: VecDeque<String> = plan_input.cask_names.iter().cloned().collect();
        let mut processed_casks: HashSet<String> = plan_input.cask_names.clone();
        let mut cask_fetch_errors: Vec<(String, SpmError)> = Vec::new();

        while let Some(token) = cask_queue.pop_front() {
            debug!("Fetching dependencies for cask: {}", token);
            match api::get_cask(&token).await {
                // This still uses the API
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
                                debug!("Added cask dependency from cask '{}': {}", token, cask_dep);
                                plan_input.cask_names.insert(cask_dep.clone());
                                cask_queue.push_back(cask_dep.clone());
                            }
                        }
                    }
                }
                Err(SpmError::NotFound(_)) => {
                    let msg = format!(
                        "Dependency cask '{token}' (required by another cask) not found via API."
                    );
                    if !cask_fetch_errors.iter().any(|(n, _)| n == &token) {
                        debug!("✖ {msg}"); // Use debug level for dependency errors during planning
                        cask_fetch_errors.push((token.clone(), SpmError::NotFound(msg.clone())));
                    }
                }
                Err(e) => {
                    let msg = format!("Failed to fetch dependency cask info for '{token}': {e}");
                    if !cask_fetch_errors.iter().any(|(n, _)| n == &token) {
                        debug!("✖ {msg}");
                        cask_fetch_errors.push((token, e));
                    }
                }
            }
        }
        // Add cask dependency fetch errors to the main error list
        plan_input.initial_errors.extend(cask_fetch_errors);

        debug!(
            "Final plan input: Formulae={:?}, Casks={:?}, Unknown={:?}, Errors={:?}",
            plan_input.formulae_names,
            plan_input.cask_names,
            plan_input.unknown_targets,
            plan_input.initial_errors.len() // Log count instead of full errors for brevity
        );

        Ok(plan_input)
    }
}
