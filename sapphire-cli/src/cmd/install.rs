// Location: sapphire-cli/src/cmd/install.rs

use clap::Args;
use colored::Colorize;
use futures::future::{BoxFuture, FutureExt};
use log::{error, info, warn}; // 'debug' is used in process_task_outcome
use reqwest::Client;
use sapphire_core::build;
use sapphire_core::dependency::{
    DependencyResolver, DependencyTag, ResolutionContext, ResolutionStatus,
};
use sapphire_core::formulary::Formulary;
use sapphire_core::keg::KegRegistry;
use sapphire_core::model::cask::Cask;
use sapphire_core::model::formula::Formula;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::{Result, SapphireError}; // Using the core Result/Error
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::{JoinError, JoinSet};

// ==================================================== CLI args

#[derive(Debug, Args)]
pub struct InstallArgs {
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
}

// ==================================================== public entry point

pub async fn execute(args: &InstallArgs, cfg: &Config, cache: Arc<Cache>) -> Result<()> {
    if args.cask {
        return install_casks(&args.names, args.max_concurrent_installs, cfg, Arc::clone(&cache))
            .await;
    }
    if args.skip_deps {
        warn!("--skip-deps not fully supported; dependencies will still be processed.");
    }
    install_formulae(args, cfg, Arc::clone(&cache)).await
}

fn join_to_err(e: JoinError) -> SapphireError {
    SapphireError::Generic(format!("Task join error: {}", e))
}

// ==================================================== internal state for bottle queue

#[derive(Debug, Clone, PartialEq, Eq)]
enum InstallState {
    Pending,
    Ready,
    Running,
    Ok(PathBuf), // Stores the final installation path (opt path) on success
    Failed(String),
}
#[derive(Debug)]
struct Node {
    formula: Arc<Formula>,
    deps_remaining: usize, // How many direct dependencies in the install plan must finish first
    dependents: Vec<String>, // List of direct dependents (nodes that need this one) in the install plan
    state: InstallState,
}

// ==================================================== formula / bottle workflow

async fn install_formulae(args: &InstallArgs, cfg: &Config, cache: Arc<Cache>) -> Result<()> {
    info!("{}", "📦 Beginning bottle installation…".blue().bold());

    // --- Phase 1: Dependency Resolution ---
    let formulary = Formulary::new(cfg.clone());
    let keg_registry = KegRegistry::new(cfg.clone());
    let ctx = ResolutionContext {
        formulary: &formulary,
        keg_registry: &keg_registry,
        sapphire_prefix: &cfg.prefix,
        include_optional: args.include_optional,
        include_test: false, // Tests aren't installed by default
        skip_recommended: args.skip_recommended,
        force_build: false, // Building from source not yet supported
    };
    let mut resolver = DependencyResolver::new(ctx);
    let graph = resolver.resolve_targets(&args.names)?;
    if graph.install_plan.is_empty() {
        info!("Everything already installed – nothing to do.");
        return Ok(());
    }

    // --- Phase 2: Build Node Map ---
    let mut nodes: HashMap<String, Node> = HashMap::new();
    for dep in &graph.install_plan {
        if dep.status == ResolutionStatus::Installed {
            continue; // skip fully installed items
        }
        let formula_deps = dep.formula.dependencies()?;
        nodes.insert(
            dep.formula.name().to_string(),
            Node {
                formula: dep.formula.clone(),
                // Calculate initial deps_remaining based on *relevant* dependencies within the plan
                deps_remaining: formula_deps
                    .iter()
                    .filter(|d| {
                        nodes.contains_key(&d.name) // Only count deps that are *also* in the install plan
                        && !d.tags.contains(DependencyTag::TEST)
                        && !(d.tags.contains(DependencyTag::OPTIONAL) && !args.include_optional)
                        && !(d.tags.contains(DependencyTag::RECOMMENDED) && args.skip_recommended)
                    })
                    .count(),
                dependents: vec![], // Will be populated below
                state: InstallState::Pending,
            },
        );
    }
    // Populate dependents list
    for name in nodes.keys().cloned().collect::<Vec<_>>() {
        let deps = nodes[&name].formula.dependencies()?;
        for d in deps {
            // Filter edges the same way deps_remaining was calculated
            if nodes.contains_key(&d.name)
                && !d.tags.contains(DependencyTag::TEST)
                && !(d.tags.contains(DependencyTag::OPTIONAL) && !args.include_optional)
                && !(d.tags.contains(DependencyTag::RECOMMENDED) && args.skip_recommended)
            {
                // If formula 'name' depends on 'd.name', then 'name' is a dependent of 'd.name'
                if let Some(dep_node) = nodes.get_mut(&d.name) {
                    dep_node.dependents.push(name.clone());
                }
            }
        }
    }

    // Seed queue with nodes ready to install (no remaining dependencies in the plan)
    let mut queue: VecDeque<String> = nodes
        .iter()
        .filter(|(_, n)| n.deps_remaining == 0 && matches!(n.state, InstallState::Pending))
        .map(|(n, _)| n.clone())
        .collect();
    // Mark initially ready nodes
    for name in &queue {
        if let Some(node) = nodes.get_mut(name) {
            node.state = InstallState::Ready;
        }
    }

    // --- Phase 3: Concurrent Work Queue ---
    let sem = Arc::new(Semaphore::new(args.max_concurrent_installs));
    let mut js: JoinSet<(String, Result<PathBuf>)> = JoinSet::new();
    let client = Arc::new(Client::new()); // Share reqwest client

    // Loop until all nodes are either Ok or Failed
    while !nodes
        .values()
        .all(|n| matches!(n.state, InstallState::Ok(_) | InstallState::Failed(_)))
    {
        // Spawn new tasks while permits are available and queue has ready items
        while let Some(name) = queue.pop_front() {
            // Double-check state before attempting to acquire (could have changed)
            if let Some(node) = nodes.get(&name) {
                if !matches!(node.state, InstallState::Ready) {
                    // If not ready (e.g., already running or failed), skip and check next item in queue
                    log::trace!(
                        "Node {} not ready (state: {:?}), skipping spawn attempt.",
                        name,
                        node.state
                    );
                    continue;
                }
            } else {
                // Should not happen if logic is correct
                error!("Node {} popped from queue but not found in map!", name);
                continue;
            }

            // Acquire a permit asynchronously
            match sem.clone().acquire_owned().await {
                Ok(permit) => {
                    // Got a permit, spawn the task
                    let node = nodes.get_mut(&name).unwrap(); // Know it exists and is ready
                    node.state = InstallState::Running;
                    let formula = node.formula.clone();
                    let task_cfg = cfg.clone(); // Clone config for this specific task
                    let cli = client.clone();
                    let cache_clone = Arc::clone(&cache);
                    let name_clone = name.clone(); // Clone name for the task

                    js.spawn(async move {
                        let res =
                            install_formula_task(&name_clone, formula, task_cfg, cli, cache_clone)
                                .await;
                        drop(permit); // Release permit when task finishes (or panics)
                        (name_clone, res)
                    });
                }
                Err(e) => {
                    // Semaphore closed, cannot acquire permit. This is likely fatal.
                    error!("Failed to acquire semaphore permit: {}", e);
                    if let Some(node) = nodes.get_mut(&name) {
                        node.state = InstallState::Failed(format!("Semaphore closed: {}", e));
                    }
                    // Pushing back might lead to infinite loop. Return error.
                    return Err(SapphireError::Generic(format!(
                        "Failed to acquire semaphore permit: {}",
                        e
                    )));
                }
            }
        } // End of inner loop (spawning tasks)

        // Wait for *any* task to complete if queue is empty or permits are unavailable
        // This prevents busy-waiting when all permits are in use but tasks are running.
        if js.is_empty() && queue.is_empty() {
             // If both are empty, check if all nodes are done (handled by outer loop condition)
             if nodes.values().all(|n| matches!(n.state, InstallState::Ok(_) | InstallState::Failed(_))) {
                 break; // All done
             } else {
                 // Should not happen if logic is correct - means nodes exist but no tasks running/queued
                 error!("Install loop stalled: No running tasks or queued items, but not all nodes are finished.");
                 return Err(SapphireError::Generic("Installation process stalled unexpectedly".to_string()));
             }
        }


        if queue.is_empty() || js.len() >= args.max_concurrent_installs {
            match js.join_next().await {
                Some(Ok((name, outcome))) => {
                    // Process the successfully joined task's outcome
                    process_task_outcome(&mut nodes, &mut queue, name, outcome);
                }
                Some(Err(e)) => {
                    // Handle task panic
                    error!("An installation task panicked: {}", e);
                    // Decide how to handle this. Maybe mark the panicked task's node as failed?
                    // For now, just log it. Dependencies won't be progressed.
                }
                None => {
                    // JoinSet is empty. If queue is also empty, the outer loop condition will break.
                    // If queue is not empty, the loop will continue to try spawning.
                }
            }
        } else {
            // Queue has items and permits *might* be available, yield to allow polling again soon
            tokio::task::yield_now().await;
        }
    } // End of outer loop (processing tasks)

    // --- Final Check ---
    let failures: Vec<_> = nodes
        .iter()
        .filter_map(|(n, node)| match &node.state {
            InstallState::Failed(msg) => Some((n.clone(), msg.clone())),
            _ => None,
        })
        .collect();

    if failures.is_empty() {
        info!("{}", "✅ All bottles installed".green().bold());
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

// --- process_task_outcome helper function ---
fn process_task_outcome(
    nodes: &mut HashMap<String, Node>,
    queue: &mut VecDeque<String>,
    name: String,
    outcome: Result<PathBuf>, // This is SapphireResult<PathBuf>
) {
    // Find the node corresponding to the completed task
    let node = match nodes.get_mut(&name) {
        Some(n) => n,
        None => {
            // This should not happen if the JoinSet only contains tasks we spawned
            error!(
                "Completed task for unknown node '{}'. State map inconsistent!",
                name
            );
            return;
        }
    };

    // Update the node's state based on the task outcome
    match outcome {
        Ok(opt_path) => {
            node.state = InstallState::Ok(opt_path); // Mark as successfully installed
            log::debug!("{} installed successfully", name); // Use log::debug now
        }
        Err(e) => {
            let error_msg = format!("{}", e); // Convert SapphireError to string
            node.state = InstallState::Failed(error_msg.clone()); // Mark as failed
            error!("install of {} failed: {}", name, error_msg);
        }
    }

    // If the task succeeded, update its dependents
    let succeeded = matches!(node.state, InstallState::Ok(_));
    let dependents = node.dependents.clone(); // Clone to avoid borrow issues while modifying map
    let failure_msg = if !succeeded {
        match &node.state {
            InstallState::Failed(msg) => msg.clone(),
            _ => String::from("Unknown failure"), // Should not happen
        }
    } else {
        String::new()
    };

    for dependent_name in dependents {
        if let Some(dep_node) = nodes.get_mut(&dependent_name) {
            if succeeded {
                // Only decrement dependency count if the dependent is still waiting
                if matches!(dep_node.state, InstallState::Pending | InstallState::Ready) {
                    dep_node.deps_remaining = dep_node.deps_remaining.saturating_sub(1);
                    // If all dependencies are now met and it was pending, mark as ready and queue it
                    if dep_node.deps_remaining == 0
                        && matches!(dep_node.state, InstallState::Pending)
                    {
                        dep_node.state = InstallState::Ready;
                        // Avoid adding duplicates to the queue
                        if !queue.contains(&dependent_name) {
                           queue.push_back(dependent_name.clone());
                        }
                    }
                }
            } else {
                // If the dependency failed, mark the dependent as failed too, unless it already finished/failed
                if matches!(
                    dep_node.state,
                    InstallState::Pending | InstallState::Ready | InstallState::Running
                ) {
                    dep_node.state = InstallState::Failed(format!(
                        "dependency '{}' failed: {}",
                        name, failure_msg
                    ));
                    log::debug!(
                        "Marking dependent '{}' as failed due to upstream failure of '{}'",
                        dependent_name,
                        name
                    );
                    // Remove from queue if it was ready to prevent attempting to run it
                    queue.retain(|item| item != &dependent_name);
                }
            }
        }
    }
}


// --- helper: single bottle task ---
async fn install_formula_task(
    name: &str,
    formula: Arc<Formula>,
    cfg: Config,
    client: Arc<Client>,
    _cache: Arc<Cache>, // Keep param for signature consistency, even if unused directly here
) -> Result<PathBuf> {
    info!("⬇️ Downloading bottle for {}...", name);
    // download_bottle uses cfg.cache_dir, so cache Arc isn't directly needed
    let bottle_path = build::formula::bottle::download_bottle(&formula, &cfg, &client).await?;

    info!("🍺 Pouring bottle for {}...", name);
    // Pouring (extracting) and linking can be blocking, use spawn_blocking
    let opt_path: PathBuf = tokio::task::spawn_blocking({
        // Clone data needed for the blocking closure
        let formula = formula.clone();
        let cfg_clone = cfg.clone();
        let bottle_clone = bottle_path.clone();
        move || -> Result<PathBuf> {
            // Closure returns the core Result type
            let install_dir =
                build::formula::bottle::install_bottle(&bottle_clone, &formula, &cfg_clone)?;
            build::formula::link::link_formula_artifacts(&formula, &install_dir)?;
            // Return the final installation path (e.g., /opt/sapphire/opt/formula-name)
            Ok(build::get_formula_opt_path(&formula))
        }
    })
    .await // Wait for the blocking task to complete
    .map_err(join_to_err)? // Convert potential JoinError
    ?; // Unwrap the inner Result<PathBuf, SapphireError>

    info!("🔗 Linked {}", name);
    Ok(opt_path)
}

// ==================================================== cask workflow

// --- install_casks function ---
fn install_casks<'a>(
    tokens: &'a [String],
    max_parallel: usize,
    cfg: &'a Config,
    cache: Arc<Cache>, // Accept Arc<Cache>
) -> BoxFuture<'static, Result<()>> {
    // Clone data needed for the 'static future
    let tokens_owned = tokens.to_vec();
    let cfg_clone = cfg.clone(); // Config needs to be Clone

    async move {
        info!("{}", "🍹 Beginning cask installation…".blue().bold());
        let sem = Arc::new(Semaphore::new(max_parallel));
        let mut js: JoinSet<(String, Result<()>)> = JoinSet::new(); // Task returns Result<()>

        for token in tokens_owned.iter().cloned() {
            // Acquire permit, map error if semaphore closes
            let permit = Arc::clone(&sem).acquire_owned().await
                .map_err(|e| SapphireError::Generic(format!("Failed to acquire semaphore for cask {}: {}", token, e)))?;

            // Clone Arcs and Config for the task
            let cache_clone = Arc::clone(&cache);
            let cfg_task_clone = cfg_clone.clone(); // Clone Config for this specific task

            js.spawn(async move {
                // Pass owned Config to the task
                let res = install_cask_task(&token, &cache_clone, &cfg_task_clone).await;
                drop(permit); // Release permit
                (token, res)
            });
        }

        // Wait for all cask tasks to complete
        let mut failures = vec![];
        while let Some(join_res) = js.join_next().await {
            match join_res {
                Ok((token, outcome)) => {
                    // Task completed (successfully or with SapphireError)
                    match outcome {
                        Ok(()) => info!("✔ installed cask {token}"),
                        Err(e) => {
                            error!("✖ {}: {}", token, e); // Use basic format for SapphireError
                            failures.push(token.clone());
                        }
                    }
                }
                Err(e) => {
                    // Task panicked
                    error!("A cask installation task panicked: {}", e);
                    // Record a generic failure note, as we don't know which token panicked easily
                    failures.push("PANICKED_TASK".to_string());
                }
            }
        }

        // Report final status
        if failures.is_empty() {
            info!("{}", "✅ All casks installed".green().bold());
            Ok(())
        } else {
            Err(SapphireError::InstallError(format!(
                "{} cask(s) failed",
                failures.len()
            )))
        }
    }
    .boxed() // Box the future
}

// --- helper: single cask task ---
async fn install_cask_task(token: &str, cache: &Arc<Cache>, cfg: &Config) -> Result<()> {
    info!("🔎 Fetching info for cask {}...", token);
    // Fetch cask metadata
    let cask: Cask = sapphire_core::fetch::api::get_cask(token).await?;

    // --- Handle Dependencies ---
    if let Some(deps) = &cask.depends_on {
        // Handle Formula Dependencies
        if let Some(formulas) = &deps.formula {
            if !formulas.is_empty() {
                info!(
                    "⚙️ Installing formula dependencies for cask {}: {:?}",
                    token, formulas
                );
                let dep_args = InstallArgs {
                    names: formulas.clone(), skip_deps: false, cask: false,
                    include_optional: false, skip_recommended: false,
                    max_concurrent_installs: 4, // Consider adjusting this
                };
                // Recursively call install_formulae, passing the shared cache
                install_formulae(&dep_args, cfg, Arc::clone(cache)).await?;
            }
        }
        // Handle Cask Dependencies
        if let Some(casks) = &deps.cask {
            if !casks.is_empty() {
                info!("🍹 Installing cask dependencies for cask {}: {:?}", token, casks);
                let casks_to_install = casks.clone();
                let cfg_clone_rec = cfg.clone(); // Clone config for the new task
                let cache_clone_rec = Arc::clone(cache); // Clone Arc for the new task
                 // Spawn the recursive call as a new, independent Tokio task
                 // This allows the dependency installs to happen concurrently (up to their own limit)
                let join_handle = tokio::spawn(async move {
                    // Need to clone config again *inside* the spawned future if it's used directly
                    let cfg_local = cfg_clone_rec.clone();
                    // Call install_casks (which returns BoxFuture) and await it
                    install_casks(&casks_to_install, 2, &cfg_local, cache_clone_rec).await // Limit recursion concurrency
                });
                // Await the spawned task, propagating JoinError and the inner Result
                join_handle.await.map_err(join_to_err)??;
            }
        }
    }

    // --- Installation ---
    // Skip if already installed
    if cask.is_installed() {
        info!("✅ Cask {} already installed – skipping download and install.", token);
        return Ok(());
    }

    // Download
    info!("⬇️ Downloading cask {}...", token);
    let dl = build::cask::download_cask(&cask, cache.as_ref()).await?;

    // Install (blocking operation)
    info!("🍺 Installing cask {}...", token);
    tokio::task::spawn_blocking({
        // Clone data for the blocking closure
        let cask = cask.clone();
        let dl_clone = dl.clone();
        move || -> Result<()> {
            // Closure returns the core Result
            build::cask::install_cask(&cask, &dl_clone)
        }
    })
    .await // Wait for blocking task
    .map_err(join_to_err)? // Propagate JoinError
    ?; // Propagate inner Result error

    info!("✅ Cask {} installed successfully", token);
    Ok(())
}