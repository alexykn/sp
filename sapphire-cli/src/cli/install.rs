use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use colored::Colorize;
use futures::future::{BoxFuture, FutureExt};
use reqwest::Client;
use sapphire_core::build;
use sapphire_core::build::formula::has_bottle_for_current_platform;
use sapphire_core::build::get_formula_opt_path;
use sapphire_core::dependency::{
    DependencyResolver, DependencyTag, ResolutionContext, ResolutionStatus,
};
use sapphire_core::formulary::Formulary;
use sapphire_core::keg::KegRegistry;
use sapphire_core::model::cask::Cask;
use sapphire_core::model::formula::Formula;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::{Result, SapphireError};
use tokio::sync::Semaphore;
use tokio::task::{JoinError, JoinSet};
use tracing::{error, info, warn};

#[derive(Debug, Args)]
pub struct Install {
    #[arg(required = true)]
    names: Vec<String>,
    #[arg(long)]
    skip_deps: bool,
    #[arg(long)]
    cask: bool,
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
impl Install {
    pub async fn run(&self, cfg: &Config, cache: Arc<Cache>) -> Result<()> {
        if self.cask {
            return install_casks(
                &self.names,
                self.max_concurrent_installs,
                cfg,
                Arc::clone(&cache),
            )
            .await;
        }
        if self.skip_deps {
            warn!("--skip-deps not fully supported; dependencies will still be processed.");
        }

        // Try installing as formulae first…
        match self.install_formulae(cfg, Arc::clone(&cache)).await {
            Ok(()) => {
                // success as formula
                Ok(())
            }
            Err(e) => {
                // Detect "formula not found" errors and fall back to cask install
                let msg = e.to_string();
                let any_not_found = self
                    .names
                    .iter()
                    .any(|name| msg.contains(&format!("Formula '{name}' not found")));

                if any_not_found {
                    info!(
                        "No matching formulae found for {:?}; trying to install as casks instead…",
                        self.names
                    );
                    // retry as casks
                    return install_casks(
                        &self.names,
                        self.max_concurrent_installs,
                        cfg,
                        Arc::clone(&cache),
                    )
                    .await;
                }

                // otherwise propagate the original error
                Err(e)
            }
        }
    }

    async fn install_formulae(&self, cfg: &Config, cache: Arc<Cache>) -> Result<()> {
        info!("{}", "Beginning bottle installation…".blue().bold());

        // Phase 1: Dependency Resolution
        let formulary = Formulary::new(cfg.clone());
        let keg_registry = KegRegistry::new(cfg.clone());
        let ctx = ResolutionContext {
            formulary: &formulary,
            keg_registry: &keg_registry,
            sapphire_prefix: cfg.prefix(),
            include_optional: self.include_optional,
            include_test: false,
            skip_recommended: self.skip_recommended,
            force_build: false,
        };
        let mut resolver = DependencyResolver::new(ctx);
        let graph = resolver.resolve_targets(&self.names)?;
        if graph.install_plan.is_empty() {
            info!("Everything already installed – nothing to do.");
            return Ok(());
        }

        // Phase 2: Build Node Map
        let mut nodes: HashMap<String, Node> = HashMap::new();
        for dep in &graph.install_plan {
            if dep.status == ResolutionStatus::Installed {
                continue;
            }
            let formula_deps = dep.formula.dependencies()?;
            nodes.insert(
                dep.formula.name().to_string(),
                Node {
                    formula: dep.formula.clone(),
                    deps_remaining: formula_deps
                        .iter()
                        .filter(|d| {
                            nodes.contains_key(&d.name)
                            && !d.tags.contains(DependencyTag::TEST)
                             // --- Simplified ---
                            && (!d.tags.contains(DependencyTag::OPTIONAL) || self.include_optional)
                             // --- End Simplified ---
                            && !(d.tags.contains(DependencyTag::RECOMMENDED)
                                && self.skip_recommended)
                        })
                        .count(),
                    dependents: vec![],
                    state: InstallState::Pending,
                },
            );
        }
        for name in nodes.keys().cloned().collect::<Vec<_>>() {
            let deps = nodes[&name].formula.dependencies()?;
            for d in deps {
                if nodes.contains_key(&d.name)
                && !d.tags.contains(DependencyTag::TEST)
                 // --- Simplified ---
                && (!d.tags.contains(DependencyTag::OPTIONAL) || self.include_optional)
                 // --- End Simplified ---
                && !(d.tags.contains(DependencyTag::RECOMMENDED) && self.skip_recommended)
                {
                    if let Some(dep_node) = nodes.get_mut(&d.name) {
                        dep_node.dependents.push(name.clone());
                    }
                }
            }
        }
        let mut queue: VecDeque<String> = nodes
            .iter()
            .filter(|(_, n)| n.deps_remaining == 0 && matches!(n.state, InstallState::Pending))
            .map(|(n, _)| n.clone())
            .collect();
        for name in &queue {
            if let Some(node) = nodes.get_mut(name) {
                node.state = InstallState::Ready;
            }
        }

        // Phase 3: Concurrent Work Queue
        let sem = Arc::new(Semaphore::new(self.max_concurrent_installs));
        let mut js: JoinSet<(String, Result<PathBuf>)> = JoinSet::new();
        let client = Arc::new(Client::new());

        while !nodes
            .values()
            .all(|n| matches!(n.state, InstallState::Ok(_) | InstallState::Failed(_)))
        {
            while let Some(name) = queue.pop_front() {
                if let Some(node) = nodes.get(&name) {
                    if !matches!(node.state, InstallState::Ready) {
                        tracing::trace!(
                            "Node {} not ready (state: {:?}), skipping spawn attempt.",
                            name,
                            node.state
                        );
                        continue;
                    }
                } else {
                    error!("Node {} popped from queue but not found in map!", name);
                    continue;
                }

                match sem.clone().acquire_owned().await {
                    Ok(permit) => {
                        let node = nodes.get_mut(&name).unwrap();
                        node.state = InstallState::Running;
                        let formula = node.formula.clone();
                        let task_cfg = cfg.clone();
                        let cli = client.clone();
                        let _cache_clone = Arc::clone(&cache);
                        let name_clone = name.clone();
                        let force_source_build = self.build_from_source;
                        let all_paths_for_build = graph
                            .install_plan
                            .iter()
                            .filter_map(|dep| dep.opt_path.clone()) // Get opt paths from resolved graph
                            .collect::<Vec<_>>();

                        js.spawn(async move {
                            let res = install_formula_task(
                                &name_clone,
                                formula,
                                task_cfg,
                                cli,
                                all_paths_for_build,
                                force_source_build,
                            )
                            .await;
                            drop(permit);
                            (name_clone, res)
                        });
                    }
                    Err(e) => {
                        error!("Failed to acquire semaphore permit: {}", e);
                        if let Some(node) = nodes.get_mut(&name) {
                            node.state = InstallState::Failed(format!("Semaphore closed: {e}"));
                        }
                        return Err(SapphireError::Generic(format!(
                            "Failed to acquire semaphore permit: {e}"
                        )));
                    }
                }
            }

            if js.is_empty() && queue.is_empty() {
                if nodes
                    .values()
                    .all(|n| matches!(n.state, InstallState::Ok(_) | InstallState::Failed(_)))
                {
                    break;
                } else {
                    error!("Install loop stalled: No running tasks or queued items, but not all nodes are finished.");
                    return Err(SapphireError::Generic(
                        "Installation process stalled unexpectedly".to_string(),
                    ));
                }
            }

            if queue.is_empty() || js.len() >= self.max_concurrent_installs {
                match js.join_next().await {
                    Some(Ok((name, outcome))) => {
                        process_task_outcome(&mut nodes, &mut queue, name, outcome)
                    }
                    Some(Err(e)) => error!("An installation task panicked: {}", e),
                    None => (),
                }
            } else {
                tokio::task::yield_now().await;
            }
        }

        // Final Check
        let failures: Vec<_> = nodes
            .iter()
            .filter_map(|(n, node)| match &node.state {
                InstallState::Failed(msg) => Some((n.clone(), msg.clone())),
                _ => None,
            })
            .collect();

        if failures.is_empty() {
            info!("{}", "All bottles installed".green().bold());
            Ok(())
        } else {
            error!("Installation failed for:");
            for (pkg, msg) in &failures {
                error!("  ✖ {}: {}", pkg, msg);
            }
            Err(SapphireError::InstallError(format!(
                "{} bottle(s) failed to install.",
                failures.len()
            )))
        }
    }
}
fn join_to_err(e: JoinError) -> SapphireError {
    SapphireError::Generic(format!("Task join error: {e}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InstallState {
    Pending,
    Ready,
    Running,
    Ok(PathBuf),
    Failed(String),
}

#[derive(Debug)]
struct Node {
    formula: Arc<Formula>,
    deps_remaining: usize,
    dependents: Vec<String>,
    state: InstallState,
}

fn process_task_outcome(
    nodes: &mut HashMap<String, Node>,
    queue: &mut VecDeque<String>,
    name: String,
    outcome: Result<PathBuf>,
) {
    let node = match nodes.get_mut(&name) {
        Some(n) => n,
        None => {
            error!(
                "Completed task for unknown node '{}'. State map inconsistent!",
                name
            );
            return;
        }
    };
    match outcome {
        Ok(opt_path) => {
            node.state = InstallState::Ok(opt_path);
            tracing::debug!("{} installed successfully", name);
        }
        Err(e) => {
            let msg = format!("{e}");
            node.state = InstallState::Failed(msg.clone());
            error!("install of {} failed: {}", name, msg);
        }
    }
    let succeeded = matches!(node.state, InstallState::Ok(_));
    let failure_msg = if !succeeded {
        if let InstallState::Failed(m) = &node.state {
            m.clone()
        } else {
            "Unknown failure".into()
        }
    } else {
        String::new()
    };
    for dependent in node.dependents.clone() {
        if let Some(dep_node) = nodes.get_mut(&dependent) {
            if succeeded {
                if matches!(dep_node.state, InstallState::Pending | InstallState::Ready) {
                    dep_node.deps_remaining = dep_node.deps_remaining.saturating_sub(1);
                    if dep_node.deps_remaining == 0 {
                        dep_node.state = InstallState::Ready;
                        if !queue.contains(&dependent) {
                            queue.push_back(dependent.clone());
                        }
                    }
                }
            } else if matches!(
                dep_node.state,
                InstallState::Pending | InstallState::Ready | InstallState::Running
            ) {
                dep_node.state =
                    InstallState::Failed(format!("dependency '{name}' failed: {failure_msg}"));
                tracing::debug!(
                    "Marking dependent '{}' as failed due to upstream failure of '{}'",
                    dependent,
                    name
                );
                queue.retain(|i| i != &dependent);
            }
        }
    }
}

// Complete, corrected install_formula_task function
async fn install_formula_task(
    name: &str,
    formula: Arc<Formula>,
    cfg: Config,
    client: Arc<Client>,
    all_installed_paths: Vec<PathBuf>,
    force_source_build: bool,
) -> Result<PathBuf> {
    let should_build_source = force_source_build || !has_bottle_for_current_platform(&formula);
    let final_opt_path = get_formula_opt_path(&formula, &cfg);

    if should_build_source {
        info!("Building {} from source...", name);
        info!("Downloading source for {}...", name);

        let source_path =
            sapphire_core::build::formula::source::download_source(&formula, &cfg).await?;

        info!("Compiling {}...", name);
        let install_dir: PathBuf = sapphire_core::build::formula::source::build_from_source(
            &source_path,
            &formula,
            &cfg,
            &all_installed_paths,
        )
        .await?;

        info!("Linking {}...", name);
        sapphire_core::build::formula::link::link_formula_artifacts(&formula, &install_dir, &cfg)?;

        info!("Built and linked {}", name);
    } else {
        info!("Downloading bottle for {}...", name);
        let bottle_path =
            sapphire_core::build::formula::bottle::download_bottle(&formula, &cfg, &client).await?;

        info!("Pouring bottle for {}...", name);
        let install_dir: PathBuf = tokio::task::spawn_blocking({
            let formula = formula.clone();
            let cfg_clone = cfg.clone();
            let bottle_clone = bottle_path.clone();
            move || -> Result<PathBuf> {
                sapphire_core::build::formula::bottle::install_bottle(
                    &bottle_clone,
                    &formula,
                    &cfg_clone,
                )
            }
        })
        .await
        .map_err(join_to_err)??;

        info!("Linking {}...", name);
        sapphire_core::build::formula::link::link_formula_artifacts(&formula, &install_dir, &cfg)?;

        info!("Poured and linked {}", name);
    }

    Ok(final_opt_path)
}

// Primary async cask installer (non-boxed)
async fn install_casks(
    tokens: &[String],
    max_parallel: usize,
    cfg: &Config,
    cache: Arc<Cache>,
) -> Result<()> {
    info!("{}", "Beginning cask installation…".blue().bold());
    let sem = Arc::new(Semaphore::new(max_parallel));
    let mut js: JoinSet<(String, Result<()>)> = JoinSet::new();
    for token in tokens.iter().cloned() {
        let permit = sem.clone().acquire_owned().await.map_err(|e| {
            SapphireError::Generic(format!("Failed to acquire semaphore for cask {token}: {e}"))
        })?;
        let cache = Arc::clone(&cache);
        let cfg_clone = cfg.clone();
        js.spawn(async move {
            let res = install_cask_task(&token, cache, &cfg_clone).await;
            drop(permit);
            (token, res)
        });
    }
    let mut failures = Vec::new();
    while let Some(join_res) = js.join_next().await {
        match join_res {
            Ok((token, outcome)) => match outcome {
                Ok(()) => info!("Installed cask {token}"),
                Err(e) => {
                    error!("✖ {}: {}", token, e);
                    failures.push(token.clone());
                }
            },
            Err(e) => {
                error!("A cask installation task panicked: {}", e);
                failures.push("PANICKED_TASK".into());
            }
        }
    }
    if failures.is_empty() {
        info!("{}", "All casks installed".green().bold());
        Ok(())
    } else {
        Err(SapphireError::InstallError(format!(
            "{} cask(s) failed",
            failures.len()
        )))
    }
}

// Boxed helper to break async recursion
fn install_casks_boxed(
    tokens: Vec<String>,
    max_parallel: usize,
    cfg: Config,
    cache: Arc<Cache>,
) -> BoxFuture<'static, Result<()>> {
    async move { install_casks(&tokens, max_parallel, &cfg, cache).await }.boxed()
}

async fn install_cask_task(token: &str, cache: Arc<Cache>, cfg: &Config) -> Result<()> {
    info!("Fetching info for cask {}...", token);
    let cask: Cask = sapphire_core::fetch::api::get_cask(token).await?;

    if let Some(deps) = &cask.depends_on {
        // Formula dependencies
        if !deps.formula.is_empty() {
            info!(
                "Installing formula dependencies for cask {}: {:?}",
                token, deps.formula
            );
            let dep_args = Install {
                names: deps.formula.clone(),
                skip_deps: false,
                cask: false,
                include_optional: false,
                skip_recommended: false,
                max_concurrent_installs: 4,
                build_from_source: false,
            };
            dep_args.install_formulae(cfg, Arc::clone(&cache)).await?;
        }

        // Cask‐to‐cask dependencies
        if !deps.cask.is_empty() {
            info!(
                "Installing cask dependencies for cask {}: {:?}",
                token, deps.cask
            );
            let casks_to_install = deps.cask.clone();
            let cache_clone = Arc::clone(&cache);
            let cfg_clone = cfg.clone();
            tokio::spawn(install_casks_boxed(
                casks_to_install,
                2,
                cfg_clone,
                cache_clone,
            ))
            .await
            .map_err(join_to_err)??;
        }
    }

    if cask.is_installed(cfg) {
        info!("Cask {} already installed – skipping.", token);
        return Ok(());
    }

    info!("Downloading cask {}...", token);
    let dl = build::cask::download_cask(&cask, cache.as_ref()).await?;

    info!("Installing cask {}...", token);
    tokio::task::spawn_blocking({
        let cask_clone = cask.clone();
        let dl_clone = dl.clone();
        let cfg_clone = cfg.clone();
        move || -> Result<()> { build::cask::install_cask(&cask_clone, &dl_clone, &cfg_clone) }
    })
    .await
    .map_err(join_to_err)??;

    info!("Cask {} installed successfully", token);
    Ok(())
}
