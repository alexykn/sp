use crate::dependency::{Dependency, DependencyTag};
use crate::model::formula::Formula;
use crate::formulary::Formulary;
use crate::keg::KegRegistry;
use crate::utils::error::{Result, SapphireError};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;

/// Represents a fully resolved dependency, including its load status and path.
#[derive(Debug, Clone)]
pub struct ResolvedDependency {
    pub formula: Rc<Formula>,
    pub keg_path: Option<PathBuf>,
    pub status: ResolutionStatus,
}

/// Status of a dependency during resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionStatus {
    Installed,
    Missing,
    Requested,
    SkippedOptional,
}

/// Context for dependency resolution, holding options and shared resources.
pub struct ResolutionContext<'a> {
    pub formulary: &'a Formulary,
    pub keg_registry: &'a KegRegistry,
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
    resolved: HashMap<String, ResolvedDependency>,
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

    pub fn resolve_targets(&mut self, targets: &[String]) -> Result<Vec<ResolvedDependency>> {
        println!("Starting dependency resolution for targets: {:?}", targets);
        self.visiting.clear();
        self.resolved.clear();
        for target_name in targets {
            self.resolve_recursive(target_name, true)?;
        }
        println!("Raw resolved map: {:?}", self.resolved);
        let sorted_list = self.topological_sort()?;
        let install_plan: Vec<ResolvedDependency> = sorted_list
            .into_iter()
            .filter(|dep| dep.status != ResolutionStatus::SkippedOptional)
            .collect();
        println!("Final installation plan (sorted): {:?}", install_plan.iter().map(|d| d.formula.name()).collect::<Vec<_>>());
        Ok(install_plan)
    }

    fn resolve_recursive(&mut self, name: &str, is_target: bool) -> Result<()> {
        println!("Resolving: {}", name);
        if self.visiting.contains(name) {
            println!("Dependency cycle detected involving: {}", name);
            return Err(SapphireError::DependencyError(format!("Dependency cycle detected involving '{}'", name)));
        }
        if self.resolved.contains_key(name) {
            println!("'{}' already resolved.", name);
            if is_target {
                if let Some(resolved_dep) = self.resolved.get_mut(name) {
                    if resolved_dep.status != ResolutionStatus::Requested && resolved_dep.status != ResolutionStatus::Installed {
                        println!("Marking '{}' as requested (was {:?})", name, resolved_dep.status);
                        resolved_dep.status = ResolutionStatus::Requested;
                    }
                }
            }
            return Ok(());
        }
        self.visiting.insert(name.to_string());

        // Load formula using concrete Formulary
        let formula = match self.formula_cache.get(name) {
            Some(f) => f.clone(),
            None => {
                let loaded_formula = Rc::new(self.context.formulary.load_formula(name)?);
                self.formula_cache.insert(name.to_string(), loaded_formula.clone());
                loaded_formula
            }
        };

        // Check if installed using concrete KegRegistry
        let installed_keg = if self.context.force_build { None } else { self.context.keg_registry.get_installed_keg(name)? };
        let (status, keg_path) = match installed_keg {
            Some(keg) => (ResolutionStatus::Installed, Some(keg.path)),
            None => (if is_target { ResolutionStatus::Requested } else { ResolutionStatus::Missing }, None)
        };
        self.resolved.insert(name.to_string(), ResolvedDependency {
            formula: formula.clone(),
            keg_path: keg_path.clone(),
            status: status.clone(),
        });

        // Recurse for dependencies
        let dependencies = formula.dependencies()?;
        for dep in dependencies {
            let dep_name = &dep.name;
            let dep_tags = dep.tags;
            println!("Processing dependency '{}' for '{}' with tags: {:?}", dep_name, name, dep_tags);
            // Skip based on flags
            if dep_tags.contains(DependencyTag::TEST) && !self.context.include_test { continue; }
            if dep_tags.contains(DependencyTag::OPTIONAL) && !self.context.include_optional {
                if !self.resolved.contains_key(dep_name.as_str()) {
                    if let Ok(loaded_formula) = self.context.formulary.load_formula(dep_name) {
                        let f_rc = Rc::new(loaded_formula);
                        self.formula_cache.insert(dep_name.to_string(), f_rc.clone());
                        self.resolved.insert(dep_name.to_string(), ResolvedDependency { formula: f_rc, keg_path: None, status: ResolutionStatus::SkippedOptional });
                    } else { println!("Could not load skipped optional dependency '{}' to mark it.", dep_name); }
                }
                continue;
            }
            if dep_tags.contains(DependencyTag::RECOMMENDED) && self.context.skip_recommended {
                if !self.resolved.contains_key(dep_name.as_str()) {
                    if let Ok(loaded_formula) = self.context.formulary.load_formula(dep_name) {
                        let f_rc = Rc::new(loaded_formula);
                        self.formula_cache.insert(dep_name.to_string(), f_rc.clone());
                        self.resolved.insert(dep_name.to_string(), ResolvedDependency { formula: f_rc, keg_path: None, status: ResolutionStatus::SkippedOptional });
                    } else { println!("Could not load skipped recommended dependency '{}' to mark it.", dep_name); }
                }
                continue;
            }
            self.resolve_recursive(dep_name, false)?;
        }
        self.visiting.remove(name);
        println!("Finished resolving: {}", name);
        Ok(())
    }

    fn topological_sort(&self) -> Result<Vec<ResolvedDependency>> {
        println!("Starting topological sort...");
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut adj: HashMap<String, Vec<String>> = HashMap::new();
        let mut sorted_list = Vec::new();
        let mut queue = VecDeque::new();

        for (name, resolved_dep) in &self.resolved {
            in_degree.entry(name.clone()).or_insert(0);
            adj.entry(name.clone()).or_insert_with(Vec::new);
            if resolved_dep.status != ResolutionStatus::SkippedOptional {
                match resolved_dep.formula.dependencies() {
                    Ok(dependencies) => {
                        for dep in dependencies {
                            if let Some(resolved_target_dep) = self.resolved.get(&dep.name) {
                                if resolved_target_dep.status != ResolutionStatus::SkippedOptional {
                                    if self.should_consider_dependency(&dep) {
                                        adj.entry(name.clone()).or_default().push(dep.name.clone());
                                        *in_degree.entry(dep.name.clone()).or_insert(0) += 1;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => { println!("Failed to get dependencies for '{}' during sort: {}", name, e); return Err(e); }
                }
            }
        }

        for (name, degree) in &in_degree {
            if *degree == 0 {
                if let Some(resolved_dep) = self.resolved.get(name) {
                    if resolved_dep.status != ResolutionStatus::SkippedOptional {
                        queue.push_back(name.clone());
                    }
                }
            }
        }

        while let Some(u_name) = queue.pop_front() {
            if let Some(resolved_dep) = self.resolved.get(&u_name) {
                sorted_list.push(resolved_dep.clone());
            } else { println!("Node '{}' from queue not found in resolved map!", u_name); continue; }

            if let Some(neighbors) = adj.get(&u_name) {
                for v_name in neighbors {
                    if let Some(degree) = in_degree.get_mut(v_name) {
                        *degree -= 1;
                        if *degree == 0 {
                            if let Some(resolved_neighbor) = self.resolved.get(v_name) {
                                if resolved_neighbor.status != ResolutionStatus::SkippedOptional {
                                    queue.push_back(v_name.clone());
                                }
                            }
                        }
                    }
                }
            }
        }

        let non_skipped_count = self.resolved.values().filter(|d| d.status != ResolutionStatus::SkippedOptional).count();
        if sorted_list.len() != non_skipped_count {
            println!("Cycle detected! Sorted count ({}) != Non-skipped node count ({}).", sorted_list.len(), non_skipped_count);
            let cyclic_nodes: Vec<_> = in_degree.iter().filter(|(_, &d)| d > 0).map(|(n, _)| n.clone()).collect();
            println!("Nodes potentially involved in cycle: {:?}", cyclic_nodes);
            return Err(SapphireError::DependencyError("Circular dependency detected".to_string()));
        }

        println!("Topological sort successful. {} nodes in sorted list.", sorted_list.len());
        Ok(sorted_list)
    }

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