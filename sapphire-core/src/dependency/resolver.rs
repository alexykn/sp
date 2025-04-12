use crate::dependency::{Dependency, DependencyTag};
use crate::model::formula::Formula;
use crate::formulary::Formulary;
use crate::keg::KegRegistry;
use crate::utils::error::{Result, SapphireError};
use crate::build; // Import build module for prefix helper
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Represents a fully resolved dependency, including its load status and path.
#[derive(Debug, Clone)]
pub struct ResolvedDependency {
    pub formula: Rc<Formula>,
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
    // BuildDependency, // Potentially useful later
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
    formula_cache: HashMap<String, Rc<Formula>>,
    visiting: HashSet<String>,
    resolved: HashMap<String, ResolvedDependency>, // Tracks the final state of each node
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

    /// Resolves dependencies for the targets and returns the installation plan and build dependency paths.
    pub fn resolve_targets(&mut self, targets: &[String]) -> Result<ResolvedGraph> {
        println!("Starting dependency resolution for targets: {:?}", targets);
        self.visiting.clear();
        self.resolved.clear();
        let mut initial_deps = Vec::new();
        for target_name in targets {
            initial_deps.push(Dependency::new_runtime(target_name)); // Treat targets as runtime deps initially
        }

        for dep in initial_deps {
            self.resolve_recursive(&dep.name, dep.tags, true)?;
        }
        println!("Raw resolved map after initial pass: {:?}", self.resolved.iter().map(|(k, v)| (k, v.status.clone(), v.tags)).collect::<HashMap<_,_>>());

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
            if dep.status == ResolutionStatus::Installed || dep.status == ResolutionStatus::Requested {
                if let Some(opt_path) = &dep.opt_path {
                    if dep.tags.contains(DependencyTag::BUILD) && seen_build_paths.insert(opt_path.clone()) {
                         println!("Adding build dep path: {}", opt_path.display());
                        build_paths.push(opt_path.clone());
                    }
                    // A dependency can be both build and runtime
                    if dep.tags.intersects(DependencyTag::RUNTIME | DependencyTag::RECOMMENDED | DependencyTag::OPTIONAL) && seen_runtime_paths.insert(opt_path.clone()) {
                         println!("Adding runtime dep path: {}", opt_path.display());
                         runtime_paths.push(opt_path.clone());
                    }
                }
            }
        }


        println!("Final installation plan (sorted): {:?}", install_plan.iter().map(|d| (d.formula.name(), d.status.clone())).collect::<Vec<_>>());
        println!("Collected build dependency paths: {:?}", build_paths.iter().map(|p| p.display()).collect::<Vec<_>>());
         println!("Collected runtime dependency paths: {:?}", runtime_paths.iter().map(|p| p.display()).collect::<Vec<_>>());


        Ok(ResolvedGraph {
            install_plan,
            build_dependency_opt_paths: build_paths,
            runtime_dependency_opt_paths: runtime_paths,
        })
    }

    /// Recursively resolves a dependency.
    /// `tags_from_parent` indicates the *reason* this dependency is being resolved (build, runtime, etc.).
    fn resolve_recursive(&mut self, name: &str, tags_from_parent: DependencyTag, is_target: bool) -> Result<()> {
        println!("Resolving: {} (requested as {:?}, is_target: {})", name, tags_from_parent, is_target);

        if self.visiting.contains(name) {
            println!("Dependency cycle detected involving: {}", name);
            return Err(SapphireError::DependencyError(format!("Dependency cycle detected involving '{}'", name)));
        }

        // Check if already resolved and update tags/status if necessary
        if let Some(existing_dep) = self.resolved.get_mut(name) {
            let mut needs_update = false;
            // Promote to requested if it's a target and wasn't already
            if is_target && existing_dep.status != ResolutionStatus::Requested && existing_dep.status != ResolutionStatus::Installed {
                 println!("Marking '{}' as requested (was {:?})", name, existing_dep.status);
                existing_dep.status = ResolutionStatus::Requested;
                needs_update = true;
            }
            // Add tags from the current resolution path
            let original_tags = existing_dep.tags;
            existing_dep.tags |= tags_from_parent;
            if existing_dep.tags != original_tags {
                 println!("Updated tags for '{}' from {:?} to {:?}", name, original_tags, existing_dep.tags);
                 needs_update = true; // Tags changed, might affect dependencies
            }

            if !needs_update {
                println!("'{}' already resolved with compatible status/tags.", name);
                return Ok(());
            }
             // If status/tags updated, we might need to re-evaluate its dependencies
             println!("Re-evaluating dependencies for '{}' due to status/tag update", name);

        } else {
             // First time encountering this dependency
             self.visiting.insert(name.to_string());

             // Load formula
            let formula = match self.formula_cache.get(name) {
                Some(f) => f.clone(),
                None => {
                    println!("Loading formula definition for '{}'", name);
                    let loaded_formula = Rc::new(self.context.formulary.load_formula(name)?);
                    self.formula_cache.insert(name.to_string(), loaded_formula.clone());
                    loaded_formula
                }
            };

             // Check if installed
            let installed_keg = if self.context.force_build { None } else { self.context.keg_registry.get_installed_keg(name)? };
            let opt_path = self.context.keg_registry.get_opt_path(name); // Get opt path regardless of install status

            let (status, keg_path) = match installed_keg {
                Some(keg) => (ResolutionStatus::Installed, Some(keg.path)),
                None => (if is_target { ResolutionStatus::Requested } else { ResolutionStatus::Missing }, None)
            };

             println!("Initial status for '{}': {:?}, Keg Path: {:?}, Opt Path: {:?}", name, status, keg_path, opt_path);

             self.resolved.insert(name.to_string(), ResolvedDependency {
                formula: formula.clone(),
                keg_path: keg_path.clone(),
                opt_path: opt_path, // Store the calculated opt path
                status: status.clone(),
                tags: tags_from_parent, // Initial tags based on how it was first requested
            });
        }


        // Recurse for dependencies *of the current formula*
        // Use the formula loaded/retrieved for `name`
        let formula = self.resolved.get(name).unwrap().formula.clone(); // Safe to unwrap as we just inserted/updated it
        let dependencies = formula.dependencies()?;

        self.visiting.insert(name.to_string()); // Add back before recursing dependencies

        for dep in dependencies {
            let dep_name = &dep.name;
            let dep_tags = dep.tags; // These are the tags defined *in the formula*
            println!("Processing dependency '{}' for '{}' with tags: {:?}", dep_name, name, dep_tags);

            // Skip based on flags and context
            if dep_tags.contains(DependencyTag::TEST) && !self.context.include_test {
                 println!("Skipping TEST dependency: {}", dep_name);
                 continue;
            }
            if dep_tags.contains(DependencyTag::OPTIONAL) && !self.context.include_optional {
                println!("Skipping OPTIONAL dependency: {}", dep_name);
                // Mark as skipped *if not already resolved*
                if !self.resolved.contains_key(dep_name.as_str()) {
                     // Try load formula just to mark it
                     if let Ok(loaded_formula) = self.context.formulary.load_formula(dep_name) {
                        let f_rc = Rc::new(loaded_formula);
                        let opt_path = self.context.keg_registry.get_opt_path(dep_name);
                        self.formula_cache.insert(dep_name.to_string(), f_rc.clone());
                        self.resolved.insert(dep_name.to_string(), ResolvedDependency {
                             formula: f_rc, keg_path: None, opt_path, status: ResolutionStatus::SkippedOptional, tags: DependencyTag::OPTIONAL
                        });
                     } else { println!("Could not load skipped optional dependency '{}' to mark it.", dep_name); }
                }
                 continue;
            }
            if dep_tags.contains(DependencyTag::RECOMMENDED) && self.context.skip_recommended {
                 println!("Skipping RECOMMENDED dependency: {}", dep_name);
                // Mark as skipped *if not already resolved*
                 if !self.resolved.contains_key(dep_name.as_str()) {
                     if let Ok(loaded_formula) = self.context.formulary.load_formula(dep_name) {
                         let f_rc = Rc::new(loaded_formula);
                         let opt_path = self.context.keg_registry.get_opt_path(dep_name);
                         self.formula_cache.insert(dep_name.to_string(), f_rc.clone());
                         self.resolved.insert(dep_name.to_string(), ResolvedDependency {
                             formula: f_rc, keg_path: None, opt_path, status: ResolutionStatus::SkippedOptional, tags: DependencyTag::RECOMMENDED
                         });
                     } else { println!("Could not load skipped recommended dependency '{}' to mark it.", dep_name); }
                 }
                 continue;
            }

            // Recurse: Pass the tags defined *in the formula* (`dep_tags`)
            self.resolve_recursive(dep_name, dep_tags, false)?; // is_target is false for dependencies
        }
        self.visiting.remove(name);
        println!("Finished resolving: {}", name);
        Ok(())
    }


    /// Performs topological sort on the resolved dependencies.
    fn topological_sort(&self) -> Result<Vec<ResolvedDependency>> {
        println!("Starting topological sort...");
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut adj: HashMap<String, Vec<String>> = HashMap::new();
        let mut sorted_list = Vec::new();
        let mut queue = VecDeque::new();

        // Initialize graph structure from resolved dependencies
        for (name, resolved_dep) in &self.resolved {
            // Only include nodes that weren't skipped
             if resolved_dep.status != ResolutionStatus::SkippedOptional {
                in_degree.entry(name.clone()).or_insert(0);
                adj.entry(name.clone()).or_insert_with(Vec::new);
             }
        }

        // Build adjacency list and calculate in-degrees based on dependencies
         for (name, resolved_dep) in &self.resolved {
            if resolved_dep.status != ResolutionStatus::SkippedOptional {
                match resolved_dep.formula.dependencies() {
                    Ok(dependencies) => {
                        for dep in dependencies {
                             // Check if the dependency itself should be considered (not skipped)
                             if let Some(resolved_target_dep) = self.resolved.get(&dep.name) {
                                 if resolved_target_dep.status != ResolutionStatus::SkippedOptional && self.should_consider_dependency(&dep) {
                                     // Add edge from dependency `dep.name` to current formula `name`
                                     // Because `name` depends on `dep.name`
                                      println!("Adding edge from {} -> {}", dep.name, name);
                                      adj.entry(dep.name.clone()).or_default().push(name.clone());
                                      *in_degree.entry(name.clone()).or_insert(0) += 1;
                                 }
                             }
                        }
                    }
                    Err(e) => { println!("Failed to get dependencies for '{}' during sort: {}", name, e); return Err(e); }
                }
            }
         }

         println!("In-degrees: {:?}", in_degree);
         println!("Adjacency List: {:?}", adj);


        // Initialize queue with nodes having in-degree 0
        for (name, degree) in &in_degree {
            if *degree == 0 {
                 // Ensure the node exists in the resolved map (it should if it's in in_degree)
                if let Some(resolved_dep) = self.resolved.get(name) {
                    if resolved_dep.status != ResolutionStatus::SkippedOptional {
                        println!("Adding node with in-degree 0 to queue: {}", name);
                        queue.push_back(name.clone());
                    }
                } else {
                     println!("Warning: Node '{}' found in in_degree map but not in resolved map during queue initialization.", name);
                }
            }
        }

         println!("Initial queue: {:?}", queue);

        // Process the queue
        while let Some(u_name) = queue.pop_front() {
             println!("Processing node from queue: {}", u_name);
            if let Some(resolved_dep) = self.resolved.get(&u_name) {
                // Ensure we only add non-skipped items to the final list
                 if resolved_dep.status != ResolutionStatus::SkippedOptional {
                     sorted_list.push(resolved_dep.clone());
                 } else {
                      println!("Skipping node '{}' during sort list construction as it was marked SkippedOptional.", u_name);
                      continue; // Skip processing neighbors if this node itself was skipped
                 }
            } else {
                // This shouldn't happen if the graph construction is correct
                println!("Error: Node '{}' from queue not found in resolved map!", u_name);
                return Err(SapphireError::Generic(format!("Topological sort inconsistency: node {} not found", u_name)));
            }

            // Decrease in-degree of neighbors
             if let Some(neighbors) = adj.get(&u_name) {
                 println!("Neighbors of {}: {:?}", u_name, neighbors);
                for v_name in neighbors {
                     // Ensure neighbor exists in the in_degree map before decrementing
                     if let Some(degree) = in_degree.get_mut(v_name) {
                        *degree -= 1;
                         println!("Decremented in-degree of {} to {}", v_name, *degree);
                        if *degree == 0 {
                            // Check neighbor's status before adding to queue
                            if let Some(resolved_neighbor) = self.resolved.get(v_name) {
                                if resolved_neighbor.status != ResolutionStatus::SkippedOptional {
                                     println!("Adding neighbor {} to queue", v_name);
                                     queue.push_back(v_name.clone());
                                } else {
                                     println!("Neighbor {} has in-degree 0 but is skipped, not adding to queue.", v_name);
                                }
                            } else {
                                 println!("Warning: Neighbor '{}' reached in-degree 0 but not found in resolved map.", v_name);
                            }
                        }
                    } else {
                          println!("Warning: Neighbor '{}' of '{}' not found in in_degree map.", v_name, u_name);
                    }
                }
            } else {
                 println!("Node {} has no neighbors in adj list.", u_name);
            }
        }

        // Check for cycles
        let non_skipped_count = self.resolved.values().filter(|d| d.status != ResolutionStatus::SkippedOptional).count();
        if sorted_list.len() != non_skipped_count {
            println!("Cycle detected! Sorted count ({}) != Non-skipped node count ({}).", sorted_list.len(), non_skipped_count);
            let cyclic_nodes: Vec<_> = in_degree.iter().filter(|(_, &d)| d > 0).map(|(n, _)| n.clone()).collect();
            println!("Nodes potentially involved in cycle (in-degree > 0): {:?}", cyclic_nodes);
            // More detailed cycle detection could be implemented here if needed
            return Err(SapphireError::DependencyError("Circular dependency detected".to_string()));
        }

        println!("Topological sort successful. {} nodes in sorted list.", sorted_list.len());
        Ok(sorted_list)
    }

    /// Helper to determine if a dependency should be considered based on context flags.
    fn should_consider_dependency(&self, dep: &Dependency) -> bool {
        let tags = dep.tags;
        if tags.contains(DependencyTag::TEST) && !self.context.include_test { return false; }
        if tags.contains(DependencyTag::OPTIONAL) && !self.context.include_optional { return false; }
        if tags.contains(DependencyTag::RECOMMENDED) && self.context.skip_recommended { return false; }
        true
    }
}

// --- Logging Macros ---
#[allow(unused_macros)]
macro_rules! debug {
    ($($arg:tt)*) => { eprintln!("DEBUG [resolver]: {}", format!($($arg)*)); };
}
#[allow(unused_macros)]
macro_rules! warn {
    ($($arg:tt)*) => { eprintln!("WARN [resolver]: {}", format!($($arg)*)); };
}
#[allow(unused_macros)]
macro_rules! error {
    ($($arg:tt)*) => { eprintln!("ERROR [resolver]: {}", format!($($arg)*)); };
}