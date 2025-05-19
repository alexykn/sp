// sps/src/pipeline/planner.rs

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use colored::Colorize;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::dependency::resolver::{
    DependencyResolver, PerTargetInstallPreferences, ResolutionContext, ResolutionStatus,
    ResolvedGraph,
};
use sps_common::error::{Result as SpsResult, SpsError};
use sps_common::formulary::Formulary;
use sps_common::keg::KegRegistry;
use sps_common::model::{Cask, InstallTargetIdentifier};
use sps_common::pipeline::{JobAction, PipelineEvent, PlannedJob};
use sps_core::check::installed::{self, InstalledPackageInfo, PackageType as CorePackageType};
use sps_core::check::update::{self, UpdateInfo};
use tokio::sync::broadcast;
use tracing::debug;

use super::runner::{fetch_target_definitions, CommandType, PipelineFlags};

pub(crate) type PlanResult<T> = SpsResult<T>;

#[derive(Debug, Default)]
pub(crate) struct PlannedOperations {
    pub jobs: Vec<PlannedJob>,
    pub errors: Vec<(String, SpsError)>,
    pub already_installed_or_up_to_date: HashSet<String>,
}

pub(crate) struct OperationPlanner<'a> {
    config: &'a Config,
    cache: Arc<Cache>,
    flags: &'a PipelineFlags,
    event_tx: broadcast::Sender<PipelineEvent>,
}

#[derive(Debug, Default)]
struct IntermediatePlan {
    initial_ops: HashMap<String, (JobAction, Option<InstallTargetIdentifier>)>,
    errors: Vec<(String, SpsError)>,
    already_satisfied: HashSet<String>,
    processed_globally: HashSet<String>,
    private_store_sources: HashMap<String, PathBuf>,
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
                                            proceed_with_install = true;
                                        }
                                    }
                                }
                            }
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
                for p in packages_to_check {
                    if plan.processed_globally.contains(&p.name) {
                        continue;
                    }
                    if let Some(ui) = update_map.get(&p.name) {
                        plan.initial_ops.insert(
                            p.name.clone(),
                            (
                                JobAction::Upgrade {
                                    from_version: p.version.clone(),
                                    old_install_path: p.path.clone(),
                                },
                                Some(ui.target_definition.clone()),
                            ),
                        );
                    } else {
                        plan.already_satisfied.insert(p.name.clone());
                    }
                    plan.processed_globally.insert(p.name.clone());
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

    pub async fn plan_operations(
        &self,
        initial_targets: &[String],
        command_type: CommandType,
    ) -> PlanResult<PlannedOperations> {
        let mut intermediate_plan = match command_type {
            CommandType::Install => self.plan_for_install(initial_targets).await?,
            CommandType::Reinstall => self.plan_for_reinstall(initial_targets).await?,
            CommandType::Upgrade { all } => self.plan_for_upgrade(initial_targets, all).await?,
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
                        if let Some((_, opt)) = intermediate_plan.initial_ops.get_mut(&name) {
                            *opt = Some(target_def);
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

        for (name, (_, opt_def)) in &intermediate_plan.initial_ops {
            if intermediate_plan.processed_globally.contains(name) {
                continue;
            }
            match opt_def {
                Some(target @ InstallTargetIdentifier::Formula(_)) => {
                    formulae_for_resolution.insert(name.clone(), target.clone());
                }
                Some(InstallTargetIdentifier::Cask(c_arc)) => {
                    cask_processing_queue.push_back(name.clone());
                    cask_deps_map.insert(name.clone(), c_arc.clone());
                }
                None => {}
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

        let mut resolved_formula_graph: Option<Arc<ResolvedGraph>> = None;
        if !formulae_for_resolution.is_empty() {
            let targets_for_resolver: Vec<_> = formulae_for_resolution.keys().cloned().collect();
            let formulary = Formulary::new(self.config.clone());
            let keg_registry = KegRegistry::new(self.config.clone());
            let per_target_prefs = PerTargetInstallPreferences::default();
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
            };
            let mut resolver = DependencyResolver::new(ctx);
            match resolver.resolve_targets(&targets_for_resolver) {
                Ok(g) => resolved_formula_graph = Some(Arc::new(g)),
                Err(e) => {
                    for n in targets_for_resolver {
                        if !intermediate_plan
                            .errors
                            .iter()
                            .any(|(err_n, _)| err_n == &n)
                        {
                            intermediate_plan
                                .errors
                                .push((n.clone(), SpsError::DependencyError(e.to_string())));
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

        for (name, (action, opt_def)) in &intermediate_plan.initial_ops {
            if intermediate_plan
                .errors
                .iter()
                .any(|(err_name, _)| err_name == name)
            {
                tracing::debug!("[Planner] Skipping job for initial op '{}' due to an existing error recorded for it.", name);
                intermediate_plan.processed_globally.insert(name.clone());
                continue;
            }

            match opt_def {
                Some(target_def) => {
                    let is_source_build: bool;
                    match target_def {
                        InstallTargetIdentifier::Formula(new_formula_arc) => {
                            if self.flags.build_from_source {
                                is_source_build = true;
                                tracing::debug!(
                                    "User explicitly set --build-from-source for operation on '{}'. Planning source build.",
                                    name
                                );
                            } else if let JobAction::Upgrade {
                                old_install_path, ..
                            } = action
                            {
                                let previous_install_type =
                                    self.get_previous_installation_type(old_install_path);
                                if previous_install_type.as_deref() == Some("source") {
                                    is_source_build = true;
                                    tracing::debug!(
                                        "Formula '{}' was previously installed from source. Upgrading from source.",
                                        name
                                    );
                                } else {
                                    is_source_build =
                                        !sps_core::install::bottle::has_bottle_for_current_platform(
                                            new_formula_arc,
                                        );
                                    if is_source_build {
                                        tracing::debug!(
                                            "Upgrading formula '{}': Previous was bottle/unknown, and new version has no bottle. Planning source build.",
                                            name
                                        );
                                    } else {
                                        tracing::debug!(
                                            "Upgrading formula '{}': Previous was bottle/unknown, and new version has a bottle. Planning bottle upgrade.",
                                            name
                                        );
                                    }
                                }
                            } else {
                                is_source_build =
                                    !sps_core::install::bottle::has_bottle_for_current_platform(
                                        new_formula_arc,
                                    );
                                if is_source_build {
                                    tracing::debug!(
                                        "Fresh install/reinstall of formula '{}': No bottle available. Planning source build.",
                                        name
                                    );
                                } else {
                                    tracing::debug!(
                                        "Fresh install/reinstall of formula '{}': Bottle available. Planning bottle install.",
                                        name
                                    );
                                }
                            }
                        }
                        InstallTargetIdentifier::Cask(_) => {
                            is_source_build = false;
                        }
                    }

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
                    intermediate_plan.errors.push((
                        name.clone(),
                        SpsError::Generic(
                            "Definition missing unexpectedly for an operation that should have had one.".into(),
                        ),
                    ));
                    intermediate_plan.processed_globally.insert(name.clone());
                }
            }
        }

        if let Some(graph) = resolved_formula_graph.as_ref() {
            for dep_detail in &graph.install_plan {
                let dep_name = dep_detail.formula.name();

                if names_processed_from_initial_ops.contains(dep_name)
                    || intermediate_plan.processed_globally.contains(dep_name)
                    || final_planned_jobs.iter().any(|j| j.target_id == dep_name)
                {
                    continue;
                }

                if matches!(
                    dep_detail.status,
                    ResolutionStatus::Missing | ResolutionStatus::Requested
                ) {
                    let is_source_build_for_dep = self.flags.build_from_source
                        || !sps_core::install::bottle::has_bottle_for_current_platform(
                            &dep_detail.formula,
                        );
                    debug!(
                        "Planning install for new dependency '{}'. Source build: {} (Global flag: {}, Bottle available: {})",
                        dep_name, is_source_build_for_dep, self.flags.build_from_source, sps_core::install::bottle::has_bottle_for_current_platform(&dep_detail.formula)
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
                }
            }
        }

        for (cask_token, cask_arc) in cask_deps_map {
            if intermediate_plan.initial_ops.contains_key(&cask_token)
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
                Ok(Some(_)) => {
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
                }
            }
        }

        if let Some(graph) = resolved_formula_graph.as_ref() {
            if !final_planned_jobs.is_empty() {
                sort_planned_jobs_by_dependency_order(&mut final_planned_jobs, graph);
            }
        }

        Ok(PlannedOperations {
            jobs: final_planned_jobs,
            errors: intermediate_plan.errors,
            already_installed_or_up_to_date: intermediate_plan.already_satisfied,
        })
    }
}

fn sort_planned_jobs_by_dependency_order(jobs: &mut [PlannedJob], graph: &ResolvedGraph) {
    let formula_order: HashMap<String, usize> = graph
        .install_plan
        .iter()
        .enumerate()
        .map(|(idx, dep)| (dep.formula.name().to_string(), idx))
        .collect();

    jobs.sort_by_key(|job| match &job.target_definition {
        InstallTargetIdentifier::Formula(_) => formula_order
            .get(&job.target_id)
            .copied()
            .unwrap_or(usize::MAX),
        InstallTargetIdentifier::Cask(_) => usize::MAX,
    });
}
