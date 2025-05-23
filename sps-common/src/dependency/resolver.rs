// sps-common/src/dependency/resolver.rs
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{debug, error, warn};

use crate::dependency::{Dependency, DependencyTag};
use crate::error::{Result, SpsError};
use crate::formulary::Formulary;
use crate::keg::KegRegistry;
use crate::model::formula::Formula;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeInstallStrategy {
    BottlePreferred,
    SourceOnly,
    BottleOrFail,
}

#[derive(Debug, Clone, Default)]
pub struct PerTargetInstallPreferences {
    pub force_source_build_targets: HashSet<String>,
    pub force_bottle_only_targets: HashSet<String>,
}

pub struct ResolutionContext<'a> {
    pub formulary: &'a Formulary,
    pub keg_registry: &'a KegRegistry,
    pub sps_prefix: &'a Path,
    pub include_optional: bool,
    pub include_test: bool,
    pub skip_recommended: bool,
    pub initial_target_preferences: &'a PerTargetInstallPreferences,
    pub build_all_from_source: bool,
    pub cascade_source_preference_to_dependencies: bool,
    pub has_bottle_for_current_platform: fn(&Formula) -> bool,
    pub initial_target_actions: &'a HashMap<String, crate::pipeline::JobAction>,
}

#[derive(Debug, Clone)]
pub struct ResolvedDependency {
    pub formula: Arc<Formula>,
    pub keg_path: Option<PathBuf>,
    pub opt_path: Option<PathBuf>,
    pub status: ResolutionStatus,
    pub accumulated_tags: DependencyTag,
    pub determined_install_strategy: NodeInstallStrategy,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionStatus {
    Installed,
    Missing,
    Requested,
    SkippedOptional,
    NotFound,
    Failed,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedGraph {
    pub install_plan: Vec<ResolvedDependency>,
    pub build_dependency_opt_paths: Vec<PathBuf>,
    pub runtime_dependency_opt_paths: Vec<PathBuf>,
    pub resolution_details: HashMap<String, ResolvedDependency>,
}

// Added empty constructor
impl ResolvedGraph {
    pub fn empty() -> Self {
        Default::default()
    }
}

pub struct DependencyResolver<'a> {
    context: ResolutionContext<'a>,
    formula_cache: HashMap<String, Arc<Formula>>,
    visiting: HashSet<String>,
    resolution_details: HashMap<String, ResolvedDependency>,
    errors: HashMap<String, Arc<SpsError>>,
}

impl<'a> DependencyResolver<'a> {
    pub fn new(context: ResolutionContext<'a>) -> Self {
        Self {
            context,
            formula_cache: HashMap::new(),
            visiting: HashSet::new(),
            resolution_details: HashMap::new(),
            errors: HashMap::new(),
        }
    }

    fn determine_node_install_strategy(
        &self,
        formula_name: &str,
        formula_arc: &Arc<Formula>,
        is_initial_target: bool,
        requesting_parent_strategy: Option<NodeInstallStrategy>,
    ) -> NodeInstallStrategy {
        if is_initial_target {
            if self
                .context
                .initial_target_preferences
                .force_source_build_targets
                .contains(formula_name)
            {
                return NodeInstallStrategy::SourceOnly;
            }
            if self
                .context
                .initial_target_preferences
                .force_bottle_only_targets
                .contains(formula_name)
            {
                return NodeInstallStrategy::BottleOrFail;
            }
        }

        if self.context.build_all_from_source {
            return NodeInstallStrategy::SourceOnly;
        }

        if self.context.cascade_source_preference_to_dependencies
            && matches!(
                requesting_parent_strategy,
                Some(NodeInstallStrategy::SourceOnly)
            )
        {
            return NodeInstallStrategy::SourceOnly;
        }
        if matches!(
            requesting_parent_strategy,
            Some(NodeInstallStrategy::BottleOrFail)
        ) {
            return NodeInstallStrategy::BottleOrFail;
        }

        let strategy = if (self.context.has_bottle_for_current_platform)(formula_arc) {
            NodeInstallStrategy::BottlePreferred
        } else {
            NodeInstallStrategy::SourceOnly
        };

        debug!(
            "Install strategy for '{formula_name}': {:?} (initial_target={is_initial_target}, parent={:?}, bottle_available={})",
            strategy,
            requesting_parent_strategy,
            (self.context.has_bottle_for_current_platform)(formula_arc)
        );
        strategy
    }

    pub fn resolve_targets(&mut self, targets: &[String]) -> Result<ResolvedGraph> {
        debug!("Starting dependency resolution for targets: {:?}", targets);
        self.visiting.clear();
        self.resolution_details.clear();
        self.errors.clear();

        for target_name in targets {
            if let Err(e) = self.resolve_recursive(target_name, DependencyTag::RUNTIME, true, None)
            {
                self.errors.insert(target_name.clone(), Arc::new(e));
                warn!(
                    "Resolution failed for target '{}', but continuing for others.",
                    target_name
                );
            }
        }

        debug!(
            "Raw resolved map after initial pass: {:?}",
            self.resolution_details
                .iter()
                .map(|(k, v)| (k.clone(), v.status, v.accumulated_tags))
                .collect::<Vec<_>>()
        );

        let sorted_list = match self.topological_sort() {
            Ok(list) => list,
            Err(e @ SpsError::DependencyError(_)) => {
                error!("Topological sort failed due to dependency cycle: {}", e);
                return Err(e);
            }
            Err(e) => {
                error!("Topological sort failed: {}", e);
                return Err(e);
            }
        };

        let install_plan: Vec<ResolvedDependency> = sorted_list
            .into_iter()
            .filter(|dep| {
                matches!(
                    dep.status,
                    ResolutionStatus::Missing | ResolutionStatus::Requested
                )
            })
            .collect();

        let mut build_paths = Vec::new();
        let mut runtime_paths = Vec::new();
        let mut seen_build_paths = HashSet::new();
        let mut seen_runtime_paths = HashSet::new();

        for dep in self.resolution_details.values() {
            if matches!(
                dep.status,
                ResolutionStatus::Installed
                    | ResolutionStatus::Requested
                    | ResolutionStatus::Missing
            ) {
                if let Some(opt_path) = &dep.opt_path {
                    if dep.accumulated_tags.contains(DependencyTag::BUILD)
                        && seen_build_paths.insert(opt_path.clone())
                    {
                        debug!("Adding build dep path: {}", opt_path.display());
                        build_paths.push(opt_path.clone());
                    }
                    if dep.accumulated_tags.intersects(
                        DependencyTag::RUNTIME
                            | DependencyTag::RECOMMENDED
                            | DependencyTag::OPTIONAL,
                    ) && seen_runtime_paths.insert(opt_path.clone())
                    {
                        debug!("Adding runtime dep path: {}", opt_path.display());
                        runtime_paths.push(opt_path.clone());
                    }
                } else if dep.status != ResolutionStatus::NotFound
                    && dep.status != ResolutionStatus::Failed
                {
                    debug!(
                        "Warning: No opt_path found for resolved dependency {} ({:?})",
                        dep.formula.name(),
                        dep.status
                    );
                }
            }
        }

        if !self.errors.is_empty() {
            warn!(
                "Resolution encountered errors for specific targets: {:?}",
                self.errors
                    .iter()
                    .map(|(k, v)| (k, v.to_string()))
                    .collect::<HashMap<_, _>>()
            );
        }

        debug!(
            "Final installation plan (needs install/build): {:?}",
            install_plan
                .iter()
                .map(|d| (d.formula.name(), d.status))
                .collect::<Vec<_>>()
        );
        debug!(
            "Collected build dependency paths: {:?}",
            build_paths.iter().map(|p| p.display()).collect::<Vec<_>>()
        );
        debug!(
            "Collected runtime dependency paths: {:?}",
            runtime_paths
                .iter()
                .map(|p| p.display())
                .collect::<Vec<_>>()
        );

        Ok(ResolvedGraph {
            install_plan,
            build_dependency_opt_paths: build_paths,
            runtime_dependency_opt_paths: runtime_paths,
            resolution_details: self.resolution_details.clone(),
        })
    }

    fn resolve_recursive(
        &mut self,
        name: &str,
        tags_from_parent_edge: DependencyTag,
        is_initial_target: bool,
        requesting_parent_strategy: Option<NodeInstallStrategy>,
    ) -> Result<()> {
        debug!(
            "Resolving: {} (requested as {:?}, is_target: {})",
            name, tags_from_parent_edge, is_initial_target
        );

        if self.visiting.contains(name) {
            error!("Dependency cycle detected involving: {}", name);
            return Err(SpsError::DependencyError(format!(
                "Dependency cycle detected involving '{name}'"
            )));
        }

        if let Some(existing) = self.resolution_details.get_mut(name) {
            let original_status = existing.status;
            let original_tags = existing.accumulated_tags;

            let mut new_status = original_status;
            if is_initial_target && new_status == ResolutionStatus::Missing {
                new_status = ResolutionStatus::Requested;
            }
            if new_status == ResolutionStatus::SkippedOptional
                && (tags_from_parent_edge.contains(DependencyTag::RUNTIME)
                    || tags_from_parent_edge.contains(DependencyTag::BUILD)
                    || (tags_from_parent_edge.contains(DependencyTag::RECOMMENDED)
                        && !self.context.skip_recommended)
                    || (is_initial_target && self.context.include_optional))
            {
                new_status = if existing.keg_path.is_some() {
                    ResolutionStatus::Installed
                } else if is_initial_target {
                    ResolutionStatus::Requested
                } else {
                    ResolutionStatus::Missing
                };
            }

            let mut needs_revisit = false;
            if new_status != original_status {
                debug!(
                    "Updating status for '{name}' from {:?} to {:?}",
                    original_status, new_status
                );
                existing.status = new_status;
                needs_revisit = true;
            }

            let combined_tags = original_tags | tags_from_parent_edge;
            if combined_tags != original_tags {
                debug!(
                    "Updating tags for '{name}' from {:?} to {:?}",
                    original_tags, combined_tags
                );
                existing.accumulated_tags = combined_tags;
                needs_revisit = true;
            }

            if !needs_revisit {
                debug!("'{}' already resolved with compatible status/tags.", name);
                return Ok(());
            }

            debug!(
                "Re-evaluating dependencies for '{}' due to status/tag update",
                name
            );
        } else {
            self.visiting.insert(name.to_string());

            let formula_arc = match self.formula_cache.get(name) {
                Some(f) => f.clone(),
                None => {
                    debug!("Loading formula definition for '{}'", name);
                    match self.context.formulary.load_formula(name) {
                        Ok(f) => {
                            let arc = Arc::new(f);
                            self.formula_cache.insert(name.to_string(), arc.clone());
                            arc
                        }
                        Err(e) => {
                            error!("Failed to load formula definition for '{}': {}", name, e);
                            let msg = e.to_string();
                            self.resolution_details.insert(
                                name.to_string(),
                                ResolvedDependency {
                                    formula: Arc::new(Formula::placeholder(name)),
                                    keg_path: None,
                                    opt_path: None,
                                    status: ResolutionStatus::NotFound,
                                    accumulated_tags: tags_from_parent_edge,
                                    determined_install_strategy:
                                        NodeInstallStrategy::BottlePreferred,
                                    failure_reason: Some(msg.clone()),
                                },
                            );
                            self.visiting.remove(name);
                            self.errors
                                .insert(name.to_string(), Arc::new(SpsError::NotFound(msg)));
                            return Ok(());
                        }
                    }
                }
            };

            let current_node_strategy = self.determine_node_install_strategy(
                name,
                &formula_arc,
                is_initial_target,
                requesting_parent_strategy,
            );

            let (status, keg_path) = match current_node_strategy {
                NodeInstallStrategy::SourceOnly => (
                    if is_initial_target {
                        ResolutionStatus::Requested
                    } else {
                        ResolutionStatus::Missing
                    },
                    None,
                ),
                NodeInstallStrategy::BottlePreferred | NodeInstallStrategy::BottleOrFail => {
                    if let Some(keg) = self.context.keg_registry.get_installed_keg(name)? {
                        // Check if this is an upgrade target - if so, mark as Requested even if
                        // installed
                        let should_request_upgrade = is_initial_target
                            && self
                                .context
                                .initial_target_actions
                                .get(name)
                                .map(|action| {
                                    matches!(action, crate::pipeline::JobAction::Upgrade { .. })
                                })
                                .unwrap_or(false);

                        debug!("[Resolver] Package '{}': is_initial_target={}, should_request_upgrade={}, action={:?}",
                            name, is_initial_target, should_request_upgrade,
                            self.context.initial_target_actions.get(name));

                        if should_request_upgrade {
                            debug!("[Resolver] Marking upgrade target '{}' as Requested (was installed)", name);
                            (ResolutionStatus::Requested, Some(keg.path))
                        } else {
                            debug!("[Resolver] Marking '{}' as Installed (normal case)", name);
                            (ResolutionStatus::Installed, Some(keg.path))
                        }
                    } else {
                        debug!(
                            "[Resolver] Package '{}' not installed, marking as {}",
                            name,
                            if is_initial_target {
                                "Requested"
                            } else {
                                "Missing"
                            }
                        );
                        (
                            if is_initial_target {
                                ResolutionStatus::Requested
                            } else {
                                ResolutionStatus::Missing
                            },
                            None,
                        )
                    }
                }
            };

            debug!(
                "Initial status for '{}': {:?}, keg: {:?}, opt: {}",
                name,
                status,
                keg_path,
                self.context.keg_registry.get_opt_path(name).display()
            );

            self.resolution_details.insert(
                name.to_string(),
                ResolvedDependency {
                    formula: formula_arc.clone(),
                    keg_path,
                    opt_path: Some(self.context.keg_registry.get_opt_path(name)),
                    status,
                    accumulated_tags: tags_from_parent_edge,
                    determined_install_strategy: current_node_strategy,
                    failure_reason: None,
                },
            );
        }

        let dep_snapshot = self
            .resolution_details
            .get(name)
            .expect("just inserted")
            .clone();

        if matches!(
            dep_snapshot.status,
            ResolutionStatus::Failed | ResolutionStatus::NotFound
        ) {
            self.visiting.remove(name);
            return Ok(());
        }

        for dep in dep_snapshot.formula.dependencies()? {
            let dep_name = &dep.name;
            let dep_tags = dep.tags;
            let parent_name = dep_snapshot.formula.name();
            let parent_strategy = dep_snapshot.determined_install_strategy;

            debug!(
                "RESOLVER: Evaluating edge: parent='{}' ({:?}), child='{}' ({:?})",
                parent_name, parent_strategy, dep_name, dep_tags
            );

            if !self.should_consider_dependency(&dep) {
                if !self.resolution_details.contains_key(dep_name.as_str()) {
                    debug!("RESOLVER: Child '{}' of '{}' globally SKIPPED (e.g. optional/test not included). Tags: {:?}", dep_name, parent_name, dep_tags);
                }
                continue;
            }

            let should_process = self.context.should_process_dependency_edge(
                &dep_snapshot.formula,
                dep_tags,
                parent_strategy,
            );

            if !should_process {
                debug!(
                    "RESOLVER: Edge from '{}' (Strategy: {:?}) to child '{}' (Tags: {:?}) was SKIPPED by should_process_dependency_edge.",
                    parent_name, parent_strategy, dep_name, dep_tags
                );
                continue;
            }

            debug!(
                "RESOLVER: Edge from '{}' (Strategy: {:?}) to child '{}' (Tags: {:?}) WILL BE PROCESSED. Recursing.",
                parent_name, parent_strategy, dep_name, dep_tags
            );

            if let Err(e) = self.resolve_recursive(dep_name, dep_tags, false, Some(parent_strategy))
            {
                // Log the error but don't necessarily stop all resolution for this branch yet
                warn!(
                    "Error resolving child dependency '{}' for parent '{}': {}",
                    dep_name, name, e
                );
                // Optionally, mark parent as failed if child error is critical
                // self.errors.insert(name.to_string(), Arc::new(e)); // Storing error for parent if
                // needed
            }
        }

        self.visiting.remove(name);
        debug!("Finished resolving '{}'", name);
        Ok(())
    }

    fn topological_sort(&self) -> Result<Vec<ResolvedDependency>> {
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
        let mut sorted_list = Vec::new();
        let mut queue = VecDeque::new();

        let relevant_nodes_map: HashMap<String, &ResolvedDependency> = self
            .resolution_details
            .iter()
            .filter(|(_, dep)| {
                !matches!(
                    dep.status,
                    ResolutionStatus::NotFound | ResolutionStatus::Failed
                )
            })
            .map(|(k, v)| (k.clone(), v))
            .collect();

        for (parent_name, parent_rd) in &relevant_nodes_map {
            adj.entry(parent_name.clone()).or_default();
            in_degree.entry(parent_name.clone()).or_default();

            let parent_strategy = parent_rd.determined_install_strategy;
            if let Ok(dependencies) = parent_rd.formula.dependencies() {
                for child_edge in dependencies {
                    let child_name = &child_edge.name;
                    if relevant_nodes_map.contains_key(child_name)
                        && self.context.should_process_dependency_edge(
                            &parent_rd.formula,
                            child_edge.tags,
                            parent_strategy,
                        )
                        && adj
                            .entry(parent_name.clone())
                            .or_default()
                            .insert(child_name.clone())
                    {
                        *in_degree.entry(child_name.clone()).or_default() += 1;
                    }
                }
            }
        }

        for name in relevant_nodes_map.keys() {
            if *in_degree.get(name).unwrap_or(&1) == 0 {
                queue.push_back(name.clone());
            }
        }

        while let Some(u_name) = queue.pop_front() {
            if let Some(resolved_dep) = relevant_nodes_map.get(&u_name) {
                sorted_list.push((**resolved_dep).clone()); // Deref Arc then clone
                                                            // ResolvedDependency
            }
            if let Some(neighbors) = adj.get(&u_name) {
                for v_name in neighbors {
                    if relevant_nodes_map.contains_key(v_name) {
                        // Check if neighbor is relevant
                        if let Some(degree) = in_degree.get_mut(v_name) {
                            *degree = degree.saturating_sub(1);
                            if *degree == 0 {
                                queue.push_back(v_name.clone());
                            }
                        }
                    }
                }
            }
        }

        // Check for cycles: if sorted_list's length doesn't match relevant_nodes_map's length
        // (excluding already installed, skipped optional if not included, etc.)
        // A more direct check is if in_degree still contains non-zero values for relevant nodes.
        let mut cycle_detected = false;
        for (name, &degree) in &in_degree {
            if degree > 0 && relevant_nodes_map.contains_key(name) {
                // Further check if this node should have been processed (not skipped globally)
                if let Some(detail) = self.resolution_details.get(name) {
                    if self
                        .context
                        .should_consider_edge_globally(detail.accumulated_tags)
                    {
                        error!("Cycle detected or unresolved dependency: Node '{}' still has in-degree {}. Tags: {:?}", name, degree, detail.accumulated_tags);
                        cycle_detected = true;
                    } else {
                        debug!("Node '{}' has in-degree {} but was globally skipped. Tags: {:?}. Not a cycle error.", name, degree, detail.accumulated_tags);
                    }
                }
            }
        }

        if cycle_detected {
            return Err(SpsError::DependencyError(
                "Circular dependency detected or graph resolution incomplete".to_string(),
            ));
        }

        Ok(sorted_list) // Return the full sorted list of relevant nodes
    }

    fn should_consider_dependency(&self, dep: &Dependency) -> bool {
        let tags = dep.tags;
        if tags.contains(DependencyTag::TEST) && !self.context.include_test {
            return false;
        }
        if tags.contains(DependencyTag::OPTIONAL) && !self.context.include_optional {
            return false;
        }
        if tags.contains(DependencyTag::RECOMMENDED) && self.context.skip_recommended {
            return false;
        }
        true
    }
}

impl Formula {
    fn placeholder(name: &str) -> Self {
        Self {
            name: name.to_string(),
            stable_version_str: "0.0.0".to_string(),
            version_semver: semver::Version::new(0, 0, 0), // Direct use
            revision: 0,
            desc: Some("Placeholder for unresolved formula".to_string()),
            homepage: None,
            url: String::new(),
            sha256: String::new(),
            mirrors: Vec::new(),
            bottle: Default::default(),
            dependencies: Vec::new(),
            requirements: Vec::new(),
            resources: Vec::new(),
            install_keg_path: None,
        }
    }
}

impl<'a> ResolutionContext<'a> {
    pub fn should_process_dependency_edge(
        &self,
        parent_formula_for_logging: &Arc<Formula>,
        edge_tags: DependencyTag,
        parent_node_determined_strategy: NodeInstallStrategy,
    ) -> bool {
        if !self.should_consider_edge_globally(edge_tags) {
            debug!(
                "Edge with tags {:?} for child of '{}' globally SKIPPED (e.g. optional/test not included).",
                edge_tags, parent_formula_for_logging.name()
            );
            return false;
        }

        match parent_node_determined_strategy {
            NodeInstallStrategy::BottlePreferred | NodeInstallStrategy::BottleOrFail => {
                let is_purely_build_dependency = edge_tags.contains(DependencyTag::BUILD)
                    && !edge_tags.intersects(
                        DependencyTag::RUNTIME
                            | DependencyTag::RECOMMENDED
                            | DependencyTag::OPTIONAL,
                    );
                if is_purely_build_dependency {
                    debug!("Edge with tags {:?} SKIPPED: Pure BUILD dependency of a bottle-installed parent '{}'.", edge_tags, parent_formula_for_logging.name());
                    return false;
                }
            }
            NodeInstallStrategy::SourceOnly => {
                // Process all relevant (non-globally-skipped) dependencies for source builds
            }
        }
        debug!(
            "Edge with tags {:?} WILL BE PROCESSED for parent '{}' (strategy {:?}).",
            edge_tags,
            parent_formula_for_logging.name(),
            parent_node_determined_strategy
        );
        true
    }

    pub fn should_consider_edge_globally(&self, edge_tags: DependencyTag) -> bool {
        if edge_tags.contains(DependencyTag::TEST) && !self.include_test {
            return false;
        }
        if edge_tags.contains(DependencyTag::OPTIONAL) && !self.include_optional {
            return false;
        }
        if edge_tags.contains(DependencyTag::RECOMMENDED) && self.skip_recommended {
            return false;
        }
        true
    }
}
