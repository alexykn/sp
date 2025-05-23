// sps/src/pipeline/planner.rs
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use colored::Colorize;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::dependency::resolver::{
    DependencyResolver, NodeInstallStrategy, PerTargetInstallPreferences, ResolutionContext,
    ResolutionStatus, ResolvedGraph,
};
use sps_common::error::{Result as SpsResult, SpsError};
use sps_common::formulary::Formulary;
use sps_common::keg::KegRegistry;
use sps_common::model::{Cask, Formula, InstallTargetIdentifier};
use sps_common::pipeline::{JobAction, PipelineEvent, PlannedJob, PlannedOperations};
use sps_core::check::installed::{self, InstalledPackageInfo, PackageType as CorePackageType};
use sps_core::check::update::{self, UpdateInfo};
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tracing::{debug, error as trace_error, instrument, warn};

use super::runner::{get_panic_message, CommandType, PipelineFlags};

pub(crate) type PlanResult<T> = SpsResult<T>;

#[derive(Debug, Default)]
struct IntermediatePlan {
    initial_ops: HashMap<String, (JobAction, Option<InstallTargetIdentifier>)>,
    errors: Vec<(String, SpsError)>,
    already_satisfied: HashSet<String>,
    processed_globally: HashSet<String>,
    private_store_sources: HashMap<String, PathBuf>,
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

    let formulae_map_handle = tokio::spawn(load_or_fetch_formulae_map(cache.clone()));
    let casks_map_handle = tokio::spawn(load_or_fetch_casks_map(cache.clone()));

    let formulae_map = match formulae_map_handle.await {
        Ok(Ok(map)) => Some(map),
        Ok(Err(e)) => {
            warn!("[FetchDefs] Failed to load/fetch full formulae list: {}", e);
            None
        }
        Err(e) => {
            warn!(
                "[FetchDefs] Formulae map loading task panicked: {}",
                get_panic_message(e.into_panic()) // Ensure get_panic_message is accessible
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
                get_panic_message(e.into_panic()) // Ensure get_panic_message is accessible
            );
            None
        }
    };

    for name_str in names {
        let name_owned = name_str.to_string();
        let local_formulae_map = formulae_map.clone();
        let local_casks_map = casks_map.clone();

        futures.spawn(async move {
            if let Some(ref map) = local_formulae_map {
                if let Some(f_arc) = map.get(&name_owned) {
                    return (name_owned, Ok(InstallTargetIdentifier::Formula(f_arc.clone())));
                }
            }
            if let Some(ref map) = local_casks_map {
                if let Some(c_arc) = map.get(&name_owned) {
                    return (name_owned, Ok(InstallTargetIdentifier::Cask(c_arc.clone())));
                }
            }
            warn!("[FetchDefs] Definition for '{}' not found in cached lists, fetching directly from API...", name_owned);
            match sps_net::api::get_formula(&name_owned).await {
                Ok(formula_obj) => return (name_owned, Ok(InstallTargetIdentifier::Formula(Arc::new(formula_obj)))),
                Err(SpsError::NotFound(_)) => {}
                Err(e) => return (name_owned, Err(e)),
            }
            match sps_net::api::get_cask(&name_owned).await {
                Ok(cask_obj) => (name_owned, Ok(InstallTargetIdentifier::Cask(Arc::new(cask_obj)))),
                Err(SpsError::NotFound(_)) => (name_owned.clone(), Err(SpsError::NotFound(format!("Formula or Cask '{name_owned}' not found")))),
                Err(e) => (name_owned, Err(e)),
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
                trace_error!(
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
            let raw_data = sps_net::api::fetch_all_formulas().await?;
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
            let raw_data = sps_net::api::fetch_all_casks().await?;
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

pub(crate) struct OperationPlanner<'a> {
    config: &'a Config,
    cache: Arc<Cache>,
    flags: &'a PipelineFlags,
    event_tx: broadcast::Sender<PipelineEvent>,
}

impl<'a> OperationPlanner<'a> {
    pub fn new(
        config: &'a Config,
        cache: Arc<Cache>,
        flags: &'a PipelineFlags,
        event_tx: broadcast::Sender<PipelineEvent>,
    ) -> Self {
        Self {
            config,
            cache,
            flags,
            event_tx,
        }
    }

    fn get_previous_installation_type(&self, old_keg_path: &Path) -> Option<String> {
        let receipt_path = old_keg_path.join("INSTALL_RECEIPT.json");
        if !receipt_path.is_file() {
            tracing::debug!(
                "No INSTALL_RECEIPT.json found at {} for previous version.",
                receipt_path.display()
            );
            return None;
        }

        match std::fs::read_to_string(&receipt_path) {
            Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(json_value) => {
                    let inst_type = json_value
                        .get("installation_type")
                        .and_then(|it| it.as_str())
                        .map(String::from);
                    tracing::debug!(
                        "Previous installation type for {}: {:?}",
                        old_keg_path.display(),
                        inst_type
                    );
                    inst_type
                }
                Err(e) => {
                    tracing::warn!("Failed to parse INSTALL_RECEIPT.json at {}: {}. Cannot determine previous installation type.", receipt_path.display(), e);
                    None
                }
            },
            Err(e) => {
                tracing::warn!("Failed to read INSTALL_RECEIPT.json at {}: {}. Cannot determine previous installation type.", receipt_path.display(), e);
                None
            }
        }
    }

    async fn check_installed_status(&self, name: &str) -> PlanResult<Option<InstalledPackageInfo>> {
        installed::get_installed_package(name, self.config).await
    }

    async fn determine_cask_private_store_source(
        &self,
        name: &str,
        version_for_path: &str,
    ) -> Option<PathBuf> {
        let cask_def_res = fetch_target_definitions(&[name.to_string()], self.cache.clone())
            .await
            .remove(name);

        if let Some(Ok(InstallTargetIdentifier::Cask(cask_arc))) = cask_def_res {
            if let Some(artifacts) = &cask_arc.artifacts {
                for artifact_entry in artifacts {
                    if let Some(app_array) = artifact_entry.get("app").and_then(|v| v.as_array()) {
                        if let Some(app_name_val) = app_array.first() {
                            if let Some(app_name_str) = app_name_val.as_str() {
                                let private_path = self.config.cask_store_app_path(
                                    name,
                                    version_for_path,
                                    app_name_str,
                                );
                                if private_path.exists() && private_path.is_dir() {
                                    debug!("[Planner] Found reusable Cask private store bundle for {} version {}: {}", name, version_for_path, private_path.display());
                                    return Some(private_path);
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }
        None
    }

    async fn plan_for_install(&self, targets: &[String]) -> PlanResult<IntermediatePlan> {
        let mut plan = IntermediatePlan::default();
        for name in targets {
            if plan.processed_globally.contains(name) {
                continue;
            }
            match self.check_installed_status(name).await {
                Ok(Some(installed_info)) => {
                    let mut proceed_with_install = false;
                    if installed_info.pkg_type == CorePackageType::Cask {
                        let manifest_path = installed_info.path.join("CASK_INSTALL_MANIFEST.json");
                        if manifest_path.is_file() {
                            if let Ok(manifest_str) = std::fs::read_to_string(&manifest_path) {
                                if let Ok(manifest_json) =
                                    serde_json::from_str::<serde_json::Value>(&manifest_str)
                                {
                                    if let Some(is_installed_flag) =
                                        manifest_json.get("is_installed").and_then(|v| v.as_bool())
                                    {
                                        if !is_installed_flag {
                                            debug!("Cask '{}' found but marked as not installed in manifest. Proceeding with install.", name);
                                            proceed_with_install = true;
                                        }
                                    }
                                }
                            }
                        } else {
                            debug!(
                                "Cask '{}' found but manifest missing. Assuming needs install.",
                                name
                            );
                            proceed_with_install = true;
                        }
                    }
                    if proceed_with_install {
                        if let Some(private_path) = self
                            .determine_cask_private_store_source(name, &installed_info.version)
                            .await
                        {
                            plan.private_store_sources
                                .insert(name.clone(), private_path);
                        }
                        plan.initial_ops
                            .insert(name.clone(), (JobAction::Install, None));
                    } else {
                        debug!("Target '{}' already installed and manifest indicates it. Marking as satisfied.", name);
                        plan.already_satisfied.insert(name.clone());
                        plan.processed_globally.insert(name.clone());
                    }
                }
                Ok(None) => {
                    if let Some(private_path) = self
                        .determine_cask_private_store_source(name, "latest")
                        .await
                    {
                        plan.private_store_sources
                            .insert(name.clone(), private_path);
                    }
                    plan.initial_ops
                        .insert(name.clone(), (JobAction::Install, None));
                }
                Err(e) => {
                    plan.errors.push((
                        name.clone(),
                        SpsError::Generic(format!(
                            "Failed to check installed status for {name}: {e}"
                        )),
                    ));
                    plan.processed_globally.insert(name.clone());
                }
            }
        }
        Ok(plan)
    }

    async fn plan_for_reinstall(&self, targets: &[String]) -> PlanResult<IntermediatePlan> {
        let mut plan = IntermediatePlan::default();
        for name in targets {
            if plan.processed_globally.contains(name) {
                continue;
            }
            match self.check_installed_status(name).await {
                Ok(Some(installed_info)) => {
                    if installed_info.pkg_type == CorePackageType::Cask {
                        if let Some(private_path) = self
                            .determine_cask_private_store_source(name, &installed_info.version)
                            .await
                        {
                            plan.private_store_sources
                                .insert(name.clone(), private_path);
                        }
                    }
                    plan.initial_ops.insert(
                        name.clone(),
                        (
                            JobAction::Reinstall {
                                version: installed_info.version.clone(),
                                current_install_path: installed_info.path.clone(),
                            },
                            None,
                        ),
                    );
                }
                Ok(None) => {
                    plan.errors.push((
                        name.clone(),
                        SpsError::NotFound(format!("Cannot reinstall '{name}': not installed.")),
                    ));
                    plan.processed_globally.insert(name.clone());
                }
                Err(e) => {
                    plan.errors.push((
                        name.clone(),
                        SpsError::Generic(format!("Failed to check status for '{name}': {e}")),
                    ));
                    plan.processed_globally.insert(name.clone());
                }
            }
        }
        Ok(plan)
    }

    async fn plan_for_upgrade(
        &self,
        targets: &[String],
        all: bool,
    ) -> PlanResult<IntermediatePlan> {
        let mut plan = IntermediatePlan::default();
        let packages_to_check = if all {
            installed::get_installed_packages(self.config)
                .await
                .map_err(|e| {
                    plan.errors.push((
                        "<all>".to_string(),
                        SpsError::Generic(format!("Failed to get installed packages: {e}")),
                    ));
                    e
                })?
        } else {
            let mut specific = Vec::new();
            for name in targets {
                match self.check_installed_status(name).await {
                    Ok(Some(info)) => {
                        if info.pkg_type == CorePackageType::Cask {
                            let manifest_path = info.path.join("CASK_INSTALL_MANIFEST.json");
                            if manifest_path.is_file() {
                                if let Ok(manifest_str) = std::fs::read_to_string(&manifest_path) {
                                    if let Ok(manifest_json) =
                                        serde_json::from_str::<serde_json::Value>(&manifest_str)
                                    {
                                        if !manifest_json
                                            .get("is_installed")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(true)
                                        {
                                            debug!("Skipping upgrade for Cask '{}' as its manifest indicates it's not fully installed.", name);
                                            plan.processed_globally.insert(name.clone());
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                        specific.push(info);
                    }
                    Ok(None) => {
                        plan.errors.push((
                            name.to_string(),
                            SpsError::NotFound(format!("Cannot upgrade '{name}': not installed.")),
                        ));
                        plan.processed_globally.insert(name.clone());
                    }
                    Err(e) => {
                        plan.errors.push((
                            name.to_string(),
                            SpsError::Generic(format!("Failed to check status for '{name}': {e}")),
                        ));
                        plan.processed_globally.insert(name.clone());
                    }
                }
            }
            specific
        };

        if packages_to_check.is_empty() {
            return Ok(plan);
        }

        match update::check_for_updates(&packages_to_check, &self.cache, self.config).await {
            Ok(updates) => {
                let update_map: HashMap<String, UpdateInfo> =
                    updates.into_iter().map(|u| (u.name.clone(), u)).collect();

                debug!(
                    "[Planner] Found {} available updates out of {} packages checked",
                    update_map.len(),
                    packages_to_check.len()
                );
                debug!(
                    "[Planner] Available updates: {:?}",
                    update_map.keys().collect::<Vec<_>>()
                );

                for p_info in packages_to_check {
                    if plan.processed_globally.contains(&p_info.name) {
                        continue;
                    }
                    if let Some(ui) = update_map.get(&p_info.name) {
                        debug!(
                            "[Planner] Adding upgrade job for '{}': {} -> {}",
                            p_info.name, p_info.version, ui.available_version
                        );
                        plan.initial_ops.insert(
                            p_info.name.clone(),
                            (
                                JobAction::Upgrade {
                                    from_version: p_info.version.clone(),
                                    old_install_path: p_info.path.clone(),
                                },
                                Some(ui.target_definition.clone()),
                            ),
                        );
                        // Don't mark packages with updates as processed_globally
                        // so they can be included in the final job list
                    } else {
                        debug!(
                            "[Planner] No update available for '{}', marking as already satisfied",
                            p_info.name
                        );
                        plan.already_satisfied.insert(p_info.name.clone());
                        // Only mark packages without updates as processed_globally
                        plan.processed_globally.insert(p_info.name.clone());
                    }
                }
            }
            Err(e) => {
                plan.errors.push((
                    "[Update Check]".to_string(),
                    SpsError::Generic(format!("Failed to check for updates: {e}")),
                ));
            }
        }
        Ok(plan)
    }

    // This now returns sps_common::pipeline::PlannedOperations
    pub async fn plan_operations(
        &self,
        initial_targets: &[String],
        command_type: CommandType,
    ) -> PlanResult<PlannedOperations> {
        debug!(
            "[Planner] Starting plan_operations with command_type: {:?}, targets: {:?}",
            command_type, initial_targets
        );

        let mut intermediate_plan = match command_type {
            CommandType::Install => self.plan_for_install(initial_targets).await?,
            CommandType::Reinstall => self.plan_for_reinstall(initial_targets).await?,
            CommandType::Upgrade { all } => {
                debug!("[Planner] Calling plan_for_upgrade with all={}", all);
                let plan = self.plan_for_upgrade(initial_targets, all).await?;
                debug!("[Planner] plan_for_upgrade returned with {} initial_ops, {} errors, {} already_satisfied", 
                    plan.initial_ops.len(), plan.errors.len(), plan.already_satisfied.len());
                debug!(
                    "[Planner] Initial ops: {:?}",
                    plan.initial_ops.keys().collect::<Vec<_>>()
                );
                debug!("[Planner] Already satisfied: {:?}", plan.already_satisfied);
                plan
            }
        };

        let definitions_to_fetch: Vec<String> = intermediate_plan
            .initial_ops
            .iter()
            .filter(|(name, (_, opt_def))| {
                opt_def.is_none() && !intermediate_plan.processed_globally.contains(*name)
            })
            .map(|(name, _)| name.clone())
            .collect();

        if !definitions_to_fetch.is_empty() {
            let fetched_defs =
                fetch_target_definitions(&definitions_to_fetch, self.cache.clone()).await;
            for (name, result) in fetched_defs {
                match result {
                    Ok(target_def) => {
                        if let Some((_action, opt_install_target)) =
                            intermediate_plan.initial_ops.get_mut(&name)
                        {
                            *opt_install_target = Some(target_def);
                        }
                    }
                    Err(e) => {
                        intermediate_plan.errors.push((
                            name.clone(),
                            SpsError::Generic(format!(
                                "Failed to get definition for target '{}': {}",
                                name.cyan(),
                                e
                            )),
                        ));
                        intermediate_plan.processed_globally.insert(name);
                    }
                }
            }
        }
        self.event_tx
            .send(PipelineEvent::DependencyResolutionStarted)
            .ok();

        let mut formulae_for_resolution: HashMap<String, InstallTargetIdentifier> = HashMap::new();
        let mut cask_deps_map: HashMap<String, Arc<Cask>> = HashMap::new();
        let mut cask_processing_queue: VecDeque<String> = VecDeque::new();

        for (name, (action, opt_def)) in &intermediate_plan.initial_ops {
            if intermediate_plan.processed_globally.contains(name) {
                continue;
            }

            // Handle both normal formula targets and upgrade targets
            match opt_def {
                Some(target @ InstallTargetIdentifier::Formula(_)) => {
                    debug!(
                        "[Planner] Adding formula '{}' to resolution list with action {:?}",
                        name, action
                    );
                    formulae_for_resolution.insert(name.clone(), target.clone());
                }
                Some(InstallTargetIdentifier::Cask(c_arc)) => {
                    debug!("[Planner] Adding cask '{}' to processing queue", name);
                    cask_processing_queue.push_back(name.clone());
                    cask_deps_map.insert(name.clone(), c_arc.clone());
                }
                None => {
                    if !intermediate_plan
                        .errors
                        .iter()
                        .any(|(err_name, _)| err_name == name)
                    {
                        intermediate_plan.errors.push((
                            name.clone(),
                            SpsError::Generic(format!(
                                "Definition for '{name}' still missing after fetch attempt."
                            )),
                        ));
                        intermediate_plan.processed_globally.insert(name.clone());
                    }
                }
            }
        }

        let mut processed_casks_for_deps_pass: HashSet<String> =
            intermediate_plan.processed_globally.clone();

        while let Some(cask_token) = cask_processing_queue.pop_front() {
            if processed_casks_for_deps_pass.contains(&cask_token) {
                continue;
            }
            processed_casks_for_deps_pass.insert(cask_token.clone());

            let cask_arc = match cask_deps_map.get(&cask_token) {
                Some(c) => c.clone(),
                None => {
                    match fetch_target_definitions(
                        std::slice::from_ref(&cask_token),
                        self.cache.clone(),
                    )
                    .await
                    .remove(&cask_token)
                    {
                        Some(Ok(InstallTargetIdentifier::Cask(c))) => {
                            cask_deps_map.insert(cask_token.clone(), c.clone());
                            c
                        }
                        Some(Err(e)) => {
                            intermediate_plan.errors.push((cask_token.clone(), e));
                            intermediate_plan
                                .processed_globally
                                .insert(cask_token.clone());
                            continue;
                        }
                        _ => {
                            intermediate_plan.errors.push((
                                cask_token.clone(),
                                SpsError::NotFound(format!(
                                    "Cask definition for dependency '{cask_token}' not found."
                                )),
                            ));
                            intermediate_plan
                                .processed_globally
                                .insert(cask_token.clone());
                            continue;
                        }
                    }
                }
            };

            if let Some(deps) = &cask_arc.depends_on {
                for formula_dep_name in &deps.formula {
                    if formulae_for_resolution.contains_key(formula_dep_name)
                        || intermediate_plan
                            .errors
                            .iter()
                            .any(|(n, _)| n == formula_dep_name)
                        || intermediate_plan
                            .already_satisfied
                            .contains(formula_dep_name)
                    {
                        continue;
                    }
                    match fetch_target_definitions(
                        std::slice::from_ref(formula_dep_name),
                        self.cache.clone(),
                    )
                    .await
                    .remove(formula_dep_name)
                    {
                        Some(Ok(target_def @ InstallTargetIdentifier::Formula(_))) => {
                            formulae_for_resolution.insert(formula_dep_name.clone(), target_def);
                        }
                        Some(Ok(InstallTargetIdentifier::Cask(_))) => {
                            intermediate_plan.errors.push((
                                formula_dep_name.clone(),
                                SpsError::Generic(format!(
                                    "Dependency '{formula_dep_name}' of Cask '{cask_token}' is unexpectedly a Cask itself."
                                )),
                            ));
                            intermediate_plan
                                .processed_globally
                                .insert(formula_dep_name.clone());
                        }
                        Some(Err(e)) => {
                            intermediate_plan.errors.push((
                                formula_dep_name.clone(),
                                SpsError::Generic(format!(
                                    "Failed def fetch for formula dep '{formula_dep_name}' of cask '{cask_token}': {e}"
                                )),
                            ));
                            intermediate_plan
                                .processed_globally
                                .insert(formula_dep_name.clone());
                        }
                        None => {
                            intermediate_plan.errors.push((
                                formula_dep_name.clone(),
                                SpsError::NotFound(format!(
                                    "Formula dep '{formula_dep_name}' for cask '{cask_token}' not found."
                                )),
                            ));
                            intermediate_plan
                                .processed_globally
                                .insert(formula_dep_name.clone());
                        }
                    }
                }
                for dep_cask_token in &deps.cask {
                    if !processed_casks_for_deps_pass.contains(dep_cask_token)
                        && !cask_processing_queue.contains(dep_cask_token)
                    {
                        cask_processing_queue.push_back(dep_cask_token.clone());
                    }
                }
            }
        }

        let mut resolved_formula_graph_opt: Option<Arc<ResolvedGraph>> = None;
        if !formulae_for_resolution.is_empty() {
            let targets_for_resolver: Vec<_> = formulae_for_resolution.keys().cloned().collect();
            let formulary = Formulary::new(self.config.clone());
            let keg_registry = KegRegistry::new(self.config.clone());

            let per_target_prefs = PerTargetInstallPreferences {
                force_source_build_targets: if self.flags.build_from_source {
                    targets_for_resolver.iter().cloned().collect()
                } else {
                    HashSet::new()
                },
                force_bottle_only_targets: HashSet::new(),
            };

            // Create map of initial target actions for the resolver
            let initial_target_actions: HashMap<String, JobAction> = intermediate_plan
                .initial_ops
                .iter()
                .filter_map(|(name, (action, _))| {
                    if targets_for_resolver.contains(name) {
                        Some((name.clone(), action.clone()))
                    } else {
                        debug!("[Planner] WARNING: Target '{}' with action {:?} is not in targets_for_resolver!", name, action);
                        None
                    }
                })
                .collect();

            debug!(
                "[Planner] Created initial_target_actions map with {} entries: {:?}",
                initial_target_actions.len(),
                initial_target_actions
            );
            debug!("[Planner] Targets for resolver: {:?}", targets_for_resolver);

            let ctx = ResolutionContext {
                formulary: &formulary,
                keg_registry: &keg_registry,
                sps_prefix: self.config.sps_root(),
                include_optional: self.flags.include_optional,
                include_test: false,
                skip_recommended: self.flags.skip_recommended,
                initial_target_preferences: &per_target_prefs,
                build_all_from_source: self.flags.build_from_source,
                cascade_source_preference_to_dependencies: true,
                has_bottle_for_current_platform:
                    sps_core::install::bottle::has_bottle_for_current_platform,
                initial_target_actions: &initial_target_actions,
            };

            let mut resolver = DependencyResolver::new(ctx);
            debug!("[Planner] Created DependencyResolver, calling resolve_targets...");
            match resolver.resolve_targets(&targets_for_resolver) {
                Ok(g) => {
                    debug!(
                        "[Planner] Dependency resolution succeeded! Install plan has {} items",
                        g.install_plan.len()
                    );
                    resolved_formula_graph_opt = Some(Arc::new(g));
                }
                Err(e) => {
                    debug!("[Planner] Dependency resolution failed: {}", e);
                    let resolver_error_msg = e.to_string(); // Capture full error
                    for n in targets_for_resolver {
                        if !intermediate_plan
                            .errors
                            .iter()
                            .any(|(err_n, _)| err_n == &n)
                        {
                            intermediate_plan.errors.push((
                                n.clone(),
                                SpsError::DependencyError(resolver_error_msg.clone()),
                            ));
                        }
                        intermediate_plan.processed_globally.insert(n);
                    }
                }
            }
        }
        self.event_tx
            .send(PipelineEvent::DependencyResolutionFinished)
            .ok();

        let mut final_planned_jobs: Vec<PlannedJob> = Vec::new();
        let mut names_processed_from_initial_ops = HashSet::new();

        debug!(
            "[Planner] Processing {} initial_ops into final jobs",
            intermediate_plan.initial_ops.len()
        );
        for (name, (action, opt_def)) in &intermediate_plan.initial_ops {
            debug!(
                "[Planner] Processing initial op '{}': action={:?}, has_def={}",
                name,
                action,
                opt_def.is_some()
            );

            if intermediate_plan.processed_globally.contains(name) {
                debug!("[Planner] Skipping '{}' - already processed globally", name);
                continue;
            }
            // If an error was recorded for this specific initial target (e.g. resolver failed for
            // it, or def missing) ensure it's marked as globally processed and not
            // added to final_planned_jobs.
            if intermediate_plan
                .errors
                .iter()
                .any(|(err_name, _)| err_name == name)
            {
                debug!("[Planner] Skipping job for initial op '{}' as an error was recorded for it during planning.", name);
                intermediate_plan.processed_globally.insert(name.clone());
                continue;
            }
            if intermediate_plan.already_satisfied.contains(name) {
                debug!(
                    "[Planner] Skipping job for initial op '{}' as it's already satisfied.",
                    name
                );
                intermediate_plan.processed_globally.insert(name.clone());
                continue;
            }

            match opt_def {
                Some(target_def) => {
                    let is_source_build = determine_build_strategy_for_job(
                        target_def,
                        action,
                        self.flags,
                        resolved_formula_graph_opt.as_deref(),
                        self,
                    );

                    final_planned_jobs.push(PlannedJob {
                        target_id: name.clone(),
                        target_definition: target_def.clone(),
                        action: action.clone(),
                        is_source_build,
                        use_private_store_source: intermediate_plan
                            .private_store_sources
                            .get(name)
                            .cloned(),
                    });
                    names_processed_from_initial_ops.insert(name.clone());
                }
                None => {
                    tracing::error!("[Planner] CRITICAL: Definition missing for planned operation on '{}' but no error was recorded in intermediate_plan.errors. This should not happen.", name);
                    if !intermediate_plan
                        .errors
                        .iter()
                        .any(|(err_n, _)| err_n == name)
                    {
                        intermediate_plan.errors.push((
                            name.clone(),
                            SpsError::Generic("Definition missing unexpectedly.".into()),
                        ));
                    }
                    intermediate_plan.processed_globally.insert(name.clone());
                }
            }
        }

        if let Some(graph) = resolved_formula_graph_opt.as_ref() {
            for dep_detail in &graph.install_plan {
                let dep_name = dep_detail.formula.name();

                if names_processed_from_initial_ops.contains(dep_name)
                    || intermediate_plan.processed_globally.contains(dep_name)
                    || final_planned_jobs.iter().any(|j| j.target_id == dep_name)
                {
                    continue;
                }

                if intermediate_plan
                    .errors
                    .iter()
                    .any(|(err_name, _)| err_name == dep_name)
                {
                    tracing::debug!("[Planner] Skipping job for dependency '{}' due to a pre-existing error recorded for it.", dep_name);
                    intermediate_plan
                        .processed_globally
                        .insert(dep_name.to_string());
                    continue;
                }
                if dep_detail.status == ResolutionStatus::Failed {
                    tracing::debug!("[Planner] Skipping job for dependency '{}' as its resolution status is Failed. Adding to planner errors.", dep_name);
                    // Ensure this error is also captured if not already.
                    if !intermediate_plan
                        .errors
                        .iter()
                        .any(|(err_name, _)| err_name == dep_name)
                    {
                        intermediate_plan.errors.push((
                            dep_name.to_string(),
                            SpsError::DependencyError(format!(
                                "Resolution failed for dependency {dep_name}"
                            )),
                        ));
                    }
                    intermediate_plan
                        .processed_globally
                        .insert(dep_name.to_string());
                    continue;
                }

                if matches!(
                    dep_detail.status,
                    ResolutionStatus::Missing | ResolutionStatus::Requested
                ) {
                    let is_source_build_for_dep = determine_build_strategy_for_job(
                        &InstallTargetIdentifier::Formula(dep_detail.formula.clone()),
                        &JobAction::Install,
                        self.flags,
                        Some(graph),
                        self,
                    );
                    debug!(
                        "Planning install for new formula dependency '{}'. Source build: {}",
                        dep_name, is_source_build_for_dep
                    );

                    final_planned_jobs.push(PlannedJob {
                        target_id: dep_name.to_string(),
                        target_definition: InstallTargetIdentifier::Formula(
                            dep_detail.formula.clone(),
                        ),
                        action: JobAction::Install,
                        is_source_build: is_source_build_for_dep,
                        use_private_store_source: None,
                    });
                } else if dep_detail.status == ResolutionStatus::Installed {
                    intermediate_plan
                        .already_satisfied
                        .insert(dep_name.to_string());
                }
            }
        }

        for (cask_token, cask_arc) in cask_deps_map {
            if names_processed_from_initial_ops.contains(&cask_token)
                || intermediate_plan.processed_globally.contains(&cask_token)
                || final_planned_jobs.iter().any(|j| j.target_id == cask_token)
            {
                continue;
            }

            match self.check_installed_status(&cask_token).await {
                Ok(None) => {
                    final_planned_jobs.push(PlannedJob {
                        target_id: cask_token.clone(),
                        target_definition: InstallTargetIdentifier::Cask(cask_arc.clone()),
                        action: JobAction::Install,
                        is_source_build: false,
                        use_private_store_source: intermediate_plan
                            .private_store_sources
                            .get(&cask_token)
                            .cloned(),
                    });
                }
                Ok(Some(_installed_info)) => {
                    intermediate_plan
                        .already_satisfied
                        .insert(cask_token.clone());
                }
                Err(e) => {
                    intermediate_plan.errors.push((
                        cask_token.clone(),
                        SpsError::Generic(format!(
                            "Failed check install status for cask dependency {cask_token}: {e}"
                        )),
                    ));
                    intermediate_plan
                        .processed_globally
                        .insert(cask_token.clone());
                }
            }
        }
        if let Some(graph) = resolved_formula_graph_opt.as_ref() {
            if !final_planned_jobs.is_empty() {
                sort_planned_jobs(&mut final_planned_jobs, graph);
            }
        }

        debug!(
            "[Planner] Finishing plan_operations with {} jobs, {} errors, {} already_satisfied",
            final_planned_jobs.len(),
            intermediate_plan.errors.len(),
            intermediate_plan.already_satisfied.len()
        );
        debug!(
            "[Planner] Final jobs: {:?}",
            final_planned_jobs
                .iter()
                .map(|j| &j.target_id)
                .collect::<Vec<_>>()
        );

        Ok(PlannedOperations {
            jobs: final_planned_jobs,
            errors: intermediate_plan.errors,
            already_installed_or_up_to_date: intermediate_plan.already_satisfied,
            resolved_graph: resolved_formula_graph_opt,
        })
    }
}

fn determine_build_strategy_for_job(
    target_def: &InstallTargetIdentifier,
    action: &JobAction,
    flags: &PipelineFlags,
    resolved_graph: Option<&ResolvedGraph>,
    planner: &OperationPlanner,
) -> bool {
    match target_def {
        InstallTargetIdentifier::Formula(formula_arc) => {
            if flags.build_from_source {
                return true;
            }
            if let Some(graph) = resolved_graph {
                if let Some(resolved_detail) = graph.resolution_details.get(formula_arc.name()) {
                    match resolved_detail.determined_install_strategy {
                        NodeInstallStrategy::SourceOnly => return true,
                        NodeInstallStrategy::BottleOrFail => return false,
                        NodeInstallStrategy::BottlePreferred => {}
                    }
                }
            }
            if let JobAction::Upgrade {
                old_install_path, ..
            } = action
            {
                if planner
                    .get_previous_installation_type(old_install_path)
                    .as_deref()
                    == Some("source")
                {
                    return true;
                }
            }
            !sps_core::install::bottle::has_bottle_for_current_platform(formula_arc)
        }
        InstallTargetIdentifier::Cask(_) => false,
    }
}

fn sort_planned_jobs(jobs: &mut [PlannedJob], formula_graph: &ResolvedGraph) {
    let formula_order: HashMap<String, usize> = formula_graph
        .install_plan
        .iter()
        .enumerate()
        .map(|(idx, dep_detail)| (dep_detail.formula.name().to_string(), idx))
        .collect();

    jobs.sort_by_key(|job| match &job.target_definition {
        InstallTargetIdentifier::Formula(f_arc) => formula_order
            .get(f_arc.name())
            .copied()
            .unwrap_or(usize::MAX),
        InstallTargetIdentifier::Cask(_) => usize::MAX - 1,
    });
}
