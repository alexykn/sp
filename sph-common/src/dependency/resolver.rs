// FILE: sph-core/src/dependency/resolver.rs

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{debug, error, warn};

use crate::dependency::{Dependency, DependencyTag};
use crate::error::{Result, SphError};
use crate::formulary::Formulary;
use crate::keg::KegRegistry;
use crate::model::formula::Formula;

#[derive(Debug, Clone)]
pub struct ResolvedDependency {
    pub formula: Arc<Formula>,
    pub keg_path: Option<PathBuf>,
    pub opt_path: Option<PathBuf>,
    pub status: ResolutionStatus,
    pub tags: DependencyTag,
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

#[derive(Debug, Clone)]
pub struct ResolvedGraph {
    pub install_plan: Vec<ResolvedDependency>,
    pub build_dependency_opt_paths: Vec<PathBuf>,
    pub runtime_dependency_opt_paths: Vec<PathBuf>,
    pub resolution_details: HashMap<String, ResolvedDependency>,
}

pub struct ResolutionContext<'a> {
    pub formulary: &'a Formulary,
    pub keg_registry: &'a KegRegistry,
    pub sph_prefix: &'a Path,
    pub include_optional: bool,
    pub include_test: bool,
    pub skip_recommended: bool,
    pub force_build: bool,
}

pub struct DependencyResolver<'a> {
    context: ResolutionContext<'a>,
    formula_cache: HashMap<String, Arc<Formula>>,
    visiting: HashSet<String>,
    resolution_details: HashMap<String, ResolvedDependency>,
    // Store Arc<SphError> instead of SphError
    errors: HashMap<String, Arc<SphError>>,
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

    pub fn resolve_targets(&mut self, targets: &[String]) -> Result<ResolvedGraph> {
        debug!("Starting dependency resolution for targets: {:?}", targets);
        self.visiting.clear();
        self.resolution_details.clear();
        self.errors.clear();

        for target_name in targets {
            if let Err(e) = self.resolve_recursive(target_name, DependencyTag::RUNTIME, true) {
                // Wrap error in Arc for storage
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
                .map(|(k, v)| (k.clone(), v.status, v.tags))
                .collect::<Vec<_>>()
        );

        let sorted_list = match self.topological_sort() {
            Ok(list) => list,
            Err(e @ SphError::DependencyError(_)) => {
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
                    if dep.tags.contains(DependencyTag::BUILD)
                        && seen_build_paths.insert(opt_path.clone())
                    {
                        debug!("Adding build dep path: {}", opt_path.display());
                        build_paths.push(opt_path.clone());
                    }
                    if dep.tags.intersects(
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

    /// Walk a dependency node, collecting status and propagating errors
    fn resolve_recursive(
        &mut self,
        name: &str,
        tags_from_parent: DependencyTag,
        is_target: bool,
    ) -> Result<()> {
        debug!(
            "Resolving: {} (requested as {:?}, is_target: {})",
            name, tags_from_parent, is_target
        );

        // -------- cycle guard -------------------------------------------------------------
        if self.visiting.contains(name) {
            error!("Dependency cycle detected involving: {}", name);
            return Err(SphError::DependencyError(format!(
                "Dependency cycle detected involving '{name}'"
            )));
        }

        // -------- if we have a previous entry, maybe promote status / tags -----------------
        if let Some(existing) = self.resolution_details.get_mut(name) {
            let original_status = existing.status;
            let original_tags = existing.tags;

            // status promotion rules -------------------------------------------------------
            let mut new_status = original_status;
            if is_target && new_status == ResolutionStatus::Missing {
                new_status = ResolutionStatus::Requested;
            }
            if new_status == ResolutionStatus::SkippedOptional
                && (tags_from_parent.contains(DependencyTag::RUNTIME)
                    || tags_from_parent.contains(DependencyTag::BUILD)
                    || (tags_from_parent.contains(DependencyTag::RECOMMENDED)
                        && !self.context.skip_recommended)
                    || (is_target && self.context.include_optional))
            {
                new_status = if existing.keg_path.is_some() {
                    ResolutionStatus::Installed
                } else if is_target {
                    ResolutionStatus::Requested
                } else {
                    ResolutionStatus::Missing
                };
            }

            // apply any changes ------------------------------------------------------------
            let mut needs_revisit = false;
            if new_status != original_status {
                debug!(
                    "Updating status for '{name}' from {:?} to {:?}",
                    original_status, new_status
                );
                existing.status = new_status;
                needs_revisit = true;
            }

            let combined_tags = original_tags | tags_from_parent;
            if combined_tags != original_tags {
                debug!(
                    "Updating tags for '{name}' from {:?} to {:?}",
                    original_tags, combined_tags
                );
                existing.tags = combined_tags;
                needs_revisit = true;
            }

            // nothing else to do
            if !needs_revisit {
                debug!("'{}' already resolved with compatible status/tags.", name);
                return Ok(());
            }

            debug!(
                "Re-evaluating dependencies for '{}' due to status/tag update",
                name
            );
        }
        // -------- first time we see this node ---------------------------------------------
        else {
            self.visiting.insert(name.to_string());

            // load / cache the formula -----------------------------------------------------
            let formula: Arc<Formula> = match self.formula_cache.get(name) {
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
                                    tags: tags_from_parent,
                                    failure_reason: Some(msg.clone()),
                                },
                            );
                            self.visiting.remove(name);

                            self.errors
                                .insert(name.to_string(), Arc::new(SphError::NotFound(msg)));

                            return Ok(()); // treat “not found” as a soft failure
                        }
                    }
                }
            };

            // work out installation state --------------------------------------------------
            let installed_keg = if self.context.force_build {
                None
            } else {
                self.context.keg_registry.get_installed_keg(name)?
            };
            let opt_path = self.context.keg_registry.get_opt_path(name);

            let (status, keg_path) = match installed_keg {
                Some(keg) => (ResolutionStatus::Installed, Some(keg.path)),
                None => (
                    if is_target {
                        ResolutionStatus::Requested
                    } else {
                        ResolutionStatus::Missing
                    },
                    None,
                ),
            };

            debug!(
                "Initial status for '{}': {:?}, keg: {:?}, opt: {}",
                name,
                status,
                keg_path,
                opt_path.display()
            );

            self.resolution_details.insert(
                name.to_string(),
                ResolvedDependency {
                    formula,
                    keg_path,
                    opt_path: Some(opt_path),
                    status,
                    tags: tags_from_parent,
                    failure_reason: None,
                },
            );
        }

        // --------------------------------------------------------------------- recurse ----
        let dep_snapshot = self
            .resolution_details
            .get(name)
            .expect("just inserted")
            .clone();

        // if this node is already irrecoverably broken, stop here
        if matches!(
            dep_snapshot.status,
            ResolutionStatus::Failed | ResolutionStatus::NotFound
        ) {
            self.visiting.remove(name);
            return Ok(());
        }

        // iterate its declared dependencies -----------------------------------------------
        for dep in dep_snapshot.formula.dependencies()? {
            let dep_name = &dep.name;
            let dep_tags = dep.tags;

            debug!(
                "Processing dependency '{}' for '{}' with tags {:?}",
                dep_name, name, dep_tags
            );

            // optional / test filtering
            if !self.should_consider_dependency(&dep) {
                if !self.resolution_details.contains_key(dep_name.as_str()) {
                    debug!("Marking '{}' as SkippedOptional", dep_name);

                    if let Ok(f) = self.context.formulary.load_formula(dep_name) {
                        let arc = Arc::new(f);
                        let opt = self.context.keg_registry.get_opt_path(dep_name);

                        self.formula_cache.insert(dep_name.to_string(), arc.clone());
                        self.resolution_details.insert(
                            dep_name.to_string(),
                            ResolvedDependency {
                                formula: arc,
                                keg_path: None,
                                opt_path: Some(opt),
                                status: ResolutionStatus::SkippedOptional,
                                tags: dep_tags,
                                failure_reason: None,
                            },
                        );
                    }
                }
                continue;
            }

            // --- real recursion -----------------------------------------------------------
            if let Err(e) = self.resolve_recursive(dep_name, dep_tags, false) {
                warn!(
                    "Recursive resolution for '{}' (child of '{}') failed: {}",
                    dep_name, name, e
                );

                // we’ll need the details after moving `e`, so harvest now
                let is_cycle = matches!(e, SphError::DependencyError(_));
                let msg = e.to_string();

                // move `e` into the error map
                self.errors
                    .entry(dep_name.to_string())
                    .or_insert_with(|| Arc::new(e));

                // mark the node as failed
                if let Some(node) = self.resolution_details.get_mut(dep_name.as_str()) {
                    node.status = ResolutionStatus::Failed;
                    node.failure_reason = Some(msg);
                }

                // propagate cycles upward
                if is_cycle {
                    self.visiting.remove(name);
                    return Err(SphError::DependencyError(
                        "Circular dependency detected".into(),
                    ));
                }
            }
        }

        self.visiting.remove(name);
        debug!("Finished resolving '{}'", name);
        Ok(())
    }

    fn topological_sort(&self) -> Result<Vec<ResolvedDependency>> {
        debug!("Starting topological sort");
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
        let mut sorted_list = Vec::new();
        let mut queue = VecDeque::new();

        let relevant_nodes: Vec<_> = self
            .resolution_details
            .iter()
            .filter(|(_, dep)| {
                matches!(
                    dep.status,
                    ResolutionStatus::Installed
                        | ResolutionStatus::Missing
                        | ResolutionStatus::Requested
                )
            })
            .map(|(name, _)| name.clone())
            .collect();

        for name in &relevant_nodes {
            in_degree.entry(name.clone()).or_insert(0);
            adj.entry(name.clone()).or_default();
        }

        for name in &relevant_nodes {
            let resolved_dep = self.resolution_details.get(name).unwrap();
            match resolved_dep.formula.dependencies() {
                Ok(dependencies) => {
                    for dep in dependencies {
                        if relevant_nodes.contains(&dep.name)
                            && self.should_consider_dependency(&dep)
                            && adj
                                .entry(dep.name.clone())
                                .or_default()
                                .insert(name.clone())
                        {
                            *in_degree.entry(name.clone()).or_insert(0) += 1;
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "Failed to get dependencies for '{}' during sort: {}",
                        name, e
                    );
                    return Err(e);
                }
            }
        }

        debug!("In-degrees (relevant nodes only): {:?}", in_degree);

        for name in &relevant_nodes {
            if *in_degree.get(name).unwrap_or(&1) == 0 {
                queue.push_back(name.clone());
            }
        }

        debug!("Initial queue: {:?}", queue);

        while let Some(u_name) = queue.pop_front() {
            if let Some(resolved_dep) = self.resolution_details.get(&u_name) {
                if matches!(
                    resolved_dep.status,
                    ResolutionStatus::Installed
                        | ResolutionStatus::Missing
                        | ResolutionStatus::Requested
                ) {
                    sorted_list.push(resolved_dep.clone());
                }
            } else {
                error!(
                    "Error: Node '{}' from queue not found in resolved map!",
                    u_name
                );
                return Err(SphError::Generic(format!(
                    "Topological sort inconsistency: node {u_name} not found"
                )));
            }

            if let Some(neighbors) = adj.get(&u_name) {
                for v_name in neighbors {
                    if relevant_nodes.contains(v_name) {
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

        if sorted_list.len() != relevant_nodes.len() {
            error!(
                "Cycle detected! Sorted count ({}) != Relevant node count ({}).",
                sorted_list.len(),
                relevant_nodes.len()
            );
            let cyclic_nodes: Vec<_> = relevant_nodes
                .iter()
                .filter(|n| in_degree.get(*n).unwrap_or(&0) > &0)
                .cloned()
                .collect();
            error!(
                "Nodes potentially involved in cycle (relevant, in-degree > 0): {:?}",
                cyclic_nodes
            );
            return Err(SphError::DependencyError(
                "Circular dependency detected".to_string(),
            ));
        }

        debug!(
            "Topological sort successful. {} relevant nodes in sorted list.",
            sorted_list.len()
        );
        Ok(sorted_list)
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
            version_semver: semver::Version::new(0, 0, 0),
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
