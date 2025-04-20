use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{debug, error};

use crate::dependency::{Dependency, DependencyTag};
use crate::formulary::Formulary;
use crate::keg::KegRegistry;
use crate::model::formula::Formula;
use crate::utils::error::{Result, SapphireError}; // Use log crate

/// Represents a fully resolved dependency, including its load status and path.
#[derive(Debug, Clone)]
pub struct ResolvedDependency {
    pub formula: Arc<Formula>,
    /// Path to the specific installed version (e.g., /opt/homebrew/Cellar/foo/1.2.3)
    pub keg_path: Option<PathBuf>,
    /// Path to the linked 'opt' directory (e.g., /opt/homebrew/opt/foo)
    pub opt_path: Option<PathBuf>,
    pub status: ResolutionStatus,
    pub tags: DependencyTag, // Track tags relevant for this resolution path
}

/// Status of a dependency during resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionStatus {
    Installed,
    Missing,
    Requested,
    SkippedOptional,
}

/// Holds the results of dependency resolution.
#[derive(Debug, Clone)]
pub struct ResolvedGraph {
    /// The topologically sorted list of dependencies to be installed/ensured.
    pub install_plan: Vec<ResolvedDependency>,
    /// The resolved 'opt' paths for all *build* dependencies required by the target(s).
    pub build_dependency_opt_paths: Vec<PathBuf>,
    /// The resolved 'opt' paths for all *runtime* dependencies required by the target(s).
    pub runtime_dependency_opt_paths: Vec<PathBuf>,
}

/// Context for dependency resolution, holding options and shared resources.
pub struct ResolutionContext<'a> {
    pub formulary: &'a Formulary,
    pub keg_registry: &'a KegRegistry,
    pub sapphire_prefix: &'a Path, // Add prefix for calculating opt paths
    pub include_optional: bool,
    pub include_test: bool,
    pub skip_recommended: bool,
    pub force_build: bool,
}

/// Resolves the dependency graph for a given set of target formulas.
pub struct DependencyResolver<'a> {
    context: ResolutionContext<'a>,
    formula_cache: HashMap<String, Arc<Formula>>,
    visiting: HashSet<String>,
    // Make resolved accessible within the crate (for install.rs)
    pub resolved: HashMap<String, ResolvedDependency>, // Tracks the final state of each node
}

impl<'a> DependencyResolver<'a> {
    pub fn new(context: ResolutionContext<'a>) -> Self {
        Self {
            context,
            formula_cache: HashMap::new(),
            visiting: HashSet::new(),
            resolved: HashMap::new(),
        }
    }

    /// Resolves dependencies for the targets and returns the installation plan and build dependency
    /// paths.
    pub fn resolve_targets(&mut self, targets: &[String]) -> Result<ResolvedGraph> {
        debug!("Starting dependency resolution for targets: {:?}", targets);
        self.visiting.clear();
        self.resolved.clear();
        let mut initial_deps = Vec::new();
        for target_name in targets {
            initial_deps.push(Dependency::new_runtime(target_name)); // Treat targets as runtime
                                                                     // deps initially
        }

        for dep in initial_deps {
            self.resolve_recursive(&dep.name, dep.tags, true)?;
        }
        // Corrected collect type to Vec<_> for debug printing
        debug!(
            "Raw resolved map after initial pass: {:?}",
            self.resolved
                .iter()
                .map(|(k, v)| (k.clone(), v.status.clone(), v.tags))
                .collect::<Vec<_>>()
        );

        let sorted_list = self.topological_sort()?;
        let install_plan: Vec<ResolvedDependency> = sorted_list
            .into_iter()
            .filter(|dep| dep.status != ResolutionStatus::SkippedOptional)
            .collect();

        // Collect build and runtime dependency paths from the *entire* resolved graph
        let mut build_paths = Vec::new();
        let mut runtime_paths = Vec::new();
        let mut seen_build_paths = HashSet::new();
        let mut seen_runtime_paths = HashSet::new();

        for dep in self.resolved.values() {
            // Only consider installed or requested dependencies for path collection
            if dep.status == ResolutionStatus::Installed
                || dep.status == ResolutionStatus::Requested
            {
                if let Some(opt_path) = &dep.opt_path {
                    if opt_path.exists() {
                        // Check path existence
                        if dep.tags.contains(DependencyTag::BUILD)
                            && seen_build_paths.insert(opt_path.clone())
                        {
                            debug!("Adding build dep path: {}", opt_path.display());
                            build_paths.push(opt_path.clone());
                        }
                        // A dependency can be both build and runtime
                        if dep.tags.intersects(
                            DependencyTag::RUNTIME
                                | DependencyTag::RECOMMENDED
                                | DependencyTag::OPTIONAL,
                        ) && seen_runtime_paths.insert(opt_path.clone())
                        {
                            debug!("Adding runtime dep path: {}", opt_path.display());
                            runtime_paths.push(opt_path.clone());
                        }
                    } else {
                        debug!("Opt path {} for dependency {} does not exist, skipping for path collection.", opt_path.display(), dep.formula.name());
                    }
                } else if dep.status != ResolutionStatus::Missing {
                    // Don't warn for missing deps
                    debug!(
                        "Warning: No opt_path found for resolved dependency {} ({:?})",
                        dep.formula.name(),
                        dep.status
                    );
                }
            }
        }

        debug!(
            "Final installation plan (sorted): {:?}",
            install_plan
                .iter()
                .map(|d| (d.formula.name(), d.status.clone()))
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
        })
    }

    /// Recursively resolves a dependency.
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

        if self.visiting.contains(name) {
            error!("Dependency cycle detected involving: {}", name);
            return Err(SapphireError::DependencyError(format!(
                "Dependency cycle detected involving '{}'",
                name
            )));
        }

        // Check if already resolved and update tags/status if necessary
        if let Some(existing_dep) = self.resolved.get_mut(name) {
            let mut needs_re_evaluation = false;
            let original_tags = existing_dep.tags;
            let original_status = existing_dep.status.clone();

            // Promote to requested if it's a target and wasn't already
            if is_target
                && (existing_dep.status == ResolutionStatus::Missing
                    || existing_dep.status == ResolutionStatus::SkippedOptional)
            {
                debug!(
                    "Marking '{}' as requested (was {:?})",
                    name, existing_dep.status
                );
                existing_dep.status = ResolutionStatus::Requested;
                needs_re_evaluation = true;
            }
            // Add tags from the current resolution path
            existing_dep.tags |= tags_from_parent;
            if existing_dep.tags != original_tags {
                debug!(
                    "Updated tags for '{}' from {:?} to {:?}",
                    name, original_tags, existing_dep.tags
                );
                needs_re_evaluation = true;
            }
            // If status changed from Skipped to something else, needs re-eval
            if original_status == ResolutionStatus::SkippedOptional
                && existing_dep.status != ResolutionStatus::SkippedOptional
            {
                debug!(
                    "Dependency '{}' was previously skipped, now required. Re-evaluating.",
                    name
                );
                needs_re_evaluation = true;
            }

            if !needs_re_evaluation {
                debug!("'{}' already resolved with compatible status/tags.", name);
                return Ok(());
            }
            debug!(
                "Re-evaluating dependencies for '{}' due to status/tag update",
                name
            );
            // Fall through to process dependencies again
        } else {
            // First time encountering this dependency
            // Load formula
            let formula = match self.formula_cache.get(name) {
                Some(f) => f.clone(),
                None => {
                    debug!("Loading formula definition for '{}'", name);
                    let loaded_formula = self.context.formulary.load_formula(name)?;
                    let formula_arc = Arc::new(loaded_formula);
                    self.formula_cache
                        .insert(name.to_string(), formula_arc.clone());
                    formula_arc
                }
            };

            // Check if installed
            let installed_keg = if self.context.force_build {
                None
            } else {
                self.context.keg_registry.get_installed_keg(name)?
            };
            let opt_path = self.context.keg_registry.get_opt_path(name); // Calculate opt path regardless

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
                "Initial status for '{}': {:?}, Keg Path: {:?}, Opt Path: {}",
                name,
                status,
                keg_path,
                opt_path.display()
            );

            self.resolved.insert(
                name.to_string(),
                ResolvedDependency {
                    formula: formula.clone(),
                    keg_path: keg_path.clone(),
                    opt_path: Some(opt_path),
                    status: status.clone(),
                    tags: tags_from_parent,
                },
            );
            // Fall through to process dependencies
        }

        // Add self back to visiting set before recursing to detect cycles correctly
        self.visiting.insert(name.to_string());

        // Get the formula again (might have been updated)
        let formula = self.resolved.get(name).unwrap().formula.clone();

        // Process dependencies declared *within* the current formula
        let dependencies = formula.dependencies()?;
        for dep in dependencies {
            let dep_name = &dep.name;
            let dep_tags = dep.tags; // Tags defined *in the formula*
            debug!(
                "Processing dependency '{}' for '{}' with tags: {:?}",
                dep_name, name, dep_tags
            );

            // Skip based on flags and context
            if !self.should_consider_dependency(&dep) {
                // Mark as skipped *if not already resolved otherwise*
                if !self.resolved.contains_key(dep_name.as_str()) {
                    debug!("Marking dependency '{}' as SkippedOptional", dep_name);
                    match self.context.formulary.load_formula(dep_name) {
                        Ok(skipped_formula) => {
                            let skipped_opt_path = self.context.keg_registry.get_opt_path(dep_name);
                            let skipped_arc = Arc::new(skipped_formula);
                            self.formula_cache
                                .insert(dep_name.to_string(), skipped_arc.clone());
                            self.resolved.insert(
                                dep_name.to_string(),
                                ResolvedDependency {
                                    formula: skipped_arc,
                                    keg_path: None,
                                    opt_path: Some(skipped_opt_path),
                                    status: ResolutionStatus::SkippedOptional,
                                    tags: dep_tags,
                                },
                            );
                        }
                        Err(e) => debug!(
                            "Could not load skipped dependency '{}' to mark it: {}",
                            dep_name, e
                        ),
                    }
                } else {
                    debug!(
                        "Dependency '{}' already resolved, not marking as skipped.",
                        dep_name
                    );
                }
                continue; // Skip recursion
            }

            // Recurse: Pass the tags defined *in the formula* (`dep_tags`)
            self.resolve_recursive(dep_name, dep_tags, false)?; // is_target is false for
                                                                // dependencies
        }
        self.visiting.remove(name); // Remove after processing all children
        debug!("Finished resolving: {}", name);
        Ok(())
    }

    /// Performs topological sort on the resolved dependencies.
    fn topological_sort(&self) -> Result<Vec<ResolvedDependency>> {
        debug!("Starting topological sort...");
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut adj: HashMap<String, HashSet<String>> = HashMap::new(); // Use HashSet for neighbors
        let mut sorted_list = Vec::new();
        let mut queue = VecDeque::new();

        // Initialize graph structure from resolved dependencies (only include non-skipped)
        for (name, resolved_dep) in &self.resolved {
            if resolved_dep.status != ResolutionStatus::SkippedOptional {
                in_degree.entry(name.clone()).or_insert(0);
                adj.entry(name.clone()).or_insert_with(HashSet::new);
            }
        }

        // Build adjacency list and calculate in-degrees
        for (name, resolved_dep) in &self.resolved {
            if resolved_dep.status != ResolutionStatus::SkippedOptional {
                match resolved_dep.formula.dependencies() {
                    Ok(dependencies) => {
                        for dep in dependencies {
                            if self
                                .resolved
                                .get(&dep.name)
                                .map_or(false, |rd| rd.status != ResolutionStatus::SkippedOptional)
                                && self.should_consider_dependency(&dep)
                            {
                                // Add edge from dependency `dep.name` to current formula `name`
                                if adj
                                    .entry(dep.name.clone())
                                    .or_default()
                                    .insert(name.clone())
                                {
                                    debug!("Adding edge from {} -> {}", dep.name, name);
                                    *in_degree.entry(name.clone()).or_insert(0) += 1;
                                }
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
        }

        debug!("In-degrees: {:?}", in_degree);

        // Initialize queue with nodes having in-degree 0
        for (name, degree) in &in_degree {
            if *degree == 0 {
                if self.resolved.contains_key(name) {
                    // Ensure it exists
                    debug!("Adding node with in-degree 0 to queue: {}", name);
                    queue.push_back(name.clone());
                }
            }
        }

        debug!("Initial queue: {:?}", queue);

        // Process the queue
        while let Some(u_name) = queue.pop_front() {
            debug!("Processing node from queue: {}", u_name);
            if let Some(resolved_dep) = self.resolved.get(&u_name) {
                sorted_list.push(resolved_dep.clone());
            } else {
                error!(
                    "Error: Node '{}' from queue not found in resolved map!",
                    u_name
                );
                return Err(SapphireError::Generic(format!(
                    "Topological sort inconsistency: node {} not found",
                    u_name
                )));
            }

            // Decrease in-degree of neighbors
            if let Some(neighbors) = adj.get(&u_name) {
                debug!("Neighbors of {}: {:?}", u_name, neighbors);
                for v_name in neighbors {
                    if let Some(degree) = in_degree.get_mut(v_name) {
                        *degree -= 1;
                        debug!("Decremented in-degree of {} to {}", v_name, *degree);
                        if *degree == 0 {
                            debug!("Adding neighbor {} to queue", v_name);
                            queue.push_back(v_name.clone());
                        }
                    } else {
                        debug!(
                            "Warning: Neighbor '{}' of '{}' not found in in_degree map.",
                            v_name, u_name
                        );
                    }
                }
            } else {
                debug!("Node {} has no neighbors in adj list.", u_name);
            }
        }

        // Check for cycles
        let non_skipped_count = self
            .resolved
            .values()
            .filter(|d| d.status != ResolutionStatus::SkippedOptional)
            .count();
        if sorted_list.len() != non_skipped_count {
            error!(
                "Cycle detected! Sorted count ({}) != Non-skipped node count ({}).",
                sorted_list.len(),
                non_skipped_count
            );
            let cyclic_nodes: Vec<_> = in_degree
                .iter()
                .filter(|(_, &d)| d > 0)
                .map(|(n, _)| n.clone())
                .collect();
            error!(
                "Nodes potentially involved in cycle (in-degree > 0): {:?}",
                cyclic_nodes
            );
            return Err(SapphireError::DependencyError(
                "Circular dependency detected".to_string(),
            ));
        }

        debug!(
            "Topological sort successful. {} nodes in sorted list.",
            sorted_list.len()
        );
        Ok(sorted_list)
    }

    /// Helper to determine if a dependency should be considered based on context flags.
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

    // Removed unused get_resolved_map method
    // pub(crate) fn get_resolved_map(&self) -> &HashMap<String, ResolvedDependency> {
    //     &self.resolved
    // }
}
