// src/cmd/install.rs — bounded‑concurrency work‑queue implementation
// --------------------------------------------------------------------------------
// The installer now uses an explicit queue plus a Tokio `Semaphore` to cap the
// number of parallel bottle / cask installs **while still honouring the
// dependency DAG** calculated by `DependencyResolver`.
// --------------------------------------------------------------------------------

use clap::Args;
use colored::Colorize;
use log::{debug, error, info, warn};
use reqwest::Client;
use sapphire_core::build;
use sapphire_core::dependency::{DependencyResolver, DependencyTag, ResolutionContext, ResolutionStatus};
use sapphire_core::formulary::Formulary;
use sapphire_core::keg::KegRegistry;
use sapphire_core::model::cask::Cask;
use sapphire_core::model::formula::Formula;
use sapphire_core::utils::cache::Cache;
use sapphire_core::utils::config::Config;
use sapphire_core::utils::error::{Result, SapphireError};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::{JoinError, JoinSet};
use futures::future::{BoxFuture, FutureExt}; // Add this import // Ensure Arc is imported if not already

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

// --- Entry Point Modification ---
// The main `execute` function now needs to await the BoxFuture
pub async fn execute(args: &InstallArgs, cfg: &Config) -> Result<()> {
    if args.cask {
        // Await the BoxFuture returned by install_casks
        return install_casks(&args.names, args.max_concurrent_installs, cfg).await;
    }

    if args.skip_deps {
        warn!("--skip-deps not fully supported; dependencies will still be processed.");
    }

    install_formulae(args, cfg).await
}

// Helper (ensure it exists)
fn join_to_err(e: JoinError) -> SapphireError {
    SapphireError::Generic(format!("Join error: {e}"))
}

// ==================================================== internal state for bottle queue

#[derive(Debug, Clone, PartialEq, Eq)]
enum InstallState {
    Pending,          // waiting for deps
    Ready,            // in queue, waiting for permit
    Running,          // currently installing
    Ok(PathBuf),      // success (opt path)
    Failed(String),   // error message
}

#[derive(Debug)]
struct Node {
    formula: Arc<Formula>,
    deps_remaining: usize,
    dependents: Vec<String>,
    state: InstallState,
}

// ==================================================== formula / bottle workflow

async fn install_formulae(args: &InstallArgs, cfg: &Config) -> Result<()> {
    info!("{}", "📦 Beginning bottle installation…".blue().bold());

    // -------- Phase 1: dependency resolution (sync) --------
    let formulary    = Formulary::new(cfg.clone());
    let keg_registry = KegRegistry::new(cfg.clone());
    let ctx = ResolutionContext {
        formulary: &formulary,
        keg_registry: &keg_registry,
        sapphire_prefix: &cfg.prefix,
        include_optional: args.include_optional,
        include_test: false,
        skip_recommended: args.skip_recommended,
        force_build: false,
    };
    let mut resolver = DependencyResolver::new(ctx);
    let graph        = resolver.resolve_targets(&args.names)?;
    if graph.install_plan.is_empty() {
        info!("Everything already installed – nothing to do.");
        return Ok(());
    }

    // -------- Phase 2: build node map from plan --------
    let mut nodes: HashMap<String, Node> = HashMap::new();
    for dep in &graph.install_plan {
        if dep.status == ResolutionStatus::Installed {
            continue; // skip fully installed items
        }
        nodes.insert(
            dep.formula.name().to_string(),
            Node {
                formula: dep.formula.clone(),
                deps_remaining: 0,
                dependents: vec![],
                state: InstallState::Pending,
            },
        );
    }

    // collect edges without violating the borrow‑checker
    let mut edges: Vec<(String, String)> = vec![]; // (parent, dependency)
    for name in nodes.keys().cloned().collect::<Vec<_>>() {
        let deps = nodes[&name].formula.dependencies()?;
        for d in deps {
            if d.tags.contains(DependencyTag::TEST)
                || (d.tags.contains(DependencyTag::OPTIONAL) && !args.include_optional)
                || (d.tags.contains(DependencyTag::RECOMMENDED) && args.skip_recommended)
            {
                continue;
            }
            if nodes.contains_key(&d.name) {
                edges.push((name.clone(), d.name.clone()));
            }
        }
    }

    // apply edges to nodes
    for (parent, dep) in edges {
        nodes.get_mut(&parent).unwrap().deps_remaining += 1;
        nodes.get_mut(&dep).unwrap().dependents.push(parent);
    }

    // seed queue with nodes ready to install
    let mut queue: VecDeque<String> = nodes
        .iter()
        .filter(|(_, n)| n.deps_remaining == 0)
        .map(|(n, _)| n.clone())
        .collect();

    // -------- Phase 3: concurrent work‑queue --------
    let sem   = Arc::new(Semaphore::new(args.max_concurrent_installs));
    let mut js: JoinSet<(String, Result<PathBuf>)> = JoinSet::new();
    let client = Arc::new(Client::new());

    while !queue.is_empty() || !js.is_empty() {
        // spawn tasks while permits & queue allow
        while let Some(name) = queue.pop_front() {
            match sem.clone().try_acquire_owned() {
                Ok(permit) => {
                    let node     = nodes.get_mut(&name).unwrap();
                    node.state   = InstallState::Running;
                    let formula  = node.formula.clone();
                    let cfg      = cfg.clone();
                    let cli      = client.clone();

                    js.spawn(async move {
                        let res = install_formula_task(&name, formula, cfg, cli).await;
                        drop(permit);
                        (name, res)
                    });
                }
                Err(_) => {
                    // no permit – push back & break
                    queue.push_front(name);
                    break;
                }
            }
        }

        if let Some(join_res) = js.join_next().await {
            let (name, outcome) = join_res.expect("task panicked");
            let node = nodes.get_mut(&name).unwrap();
            match outcome {
                Ok(opt_path) => {
                    node.state = InstallState::Ok(opt_path);
                    debug!("{name} installed successfully");
                }
                Err(e) => {
                    node.state = InstallState::Failed(e.to_string());
                    error!("install of {name} failed: {e}");
                }
            }

            // propagate to dependents
            let childrens = node.dependents.clone();
            let succeeded = matches!(node.state, InstallState::Ok(_));
            for child in childrens {
                let cnode = nodes.get_mut(&child).unwrap();
                if succeeded {
                    cnode.deps_remaining -= 1;
                    if cnode.deps_remaining == 0 && matches!(cnode.state, InstallState::Pending) {
                        cnode.state = InstallState::Ready;
                        queue.push_back(child);
                    }
                } else {
                    if !matches!(cnode.state, InstallState::Failed(_)) {
                        cnode.state = InstallState::Failed(format!("dependency {name} failed"));
                    }
                }
            }
        }
    }

    let failures: Vec<_> = nodes
        .iter()
        .filter_map(|(n, node)| match &node.state {
            InstallState::Failed(msg) => Some((n, msg)),
            _ => None,
        })
        .collect();

    if failures.is_empty() {
        info!("{}", "✅ All bottles installed".green().bold());
        Ok(())
    } else {
        for (pkg, msg) in &failures {
            error!("✖ {pkg}: {msg}");
        }
        Err(SapphireError::InstallError(format!("{} bottle(s) failed", failures.len())))
    }
}

// ---------------- helper: single bottle task ---------------------------------

async fn install_formula_task(
    name: &str,
    formula: Arc<Formula>,
    cfg: Config,
    client: Arc<Client>,
) -> Result<PathBuf> {
    // download bottle
    let bottle_path = build::formula::bottle::download_bottle(&formula, &cfg, &client).await?;

    // pour + link in blocking task pool
    let opt_path: PathBuf = tokio::task::spawn_blocking({
        let formula = formula.clone();
        let cfg     = cfg.clone();
        let bottle  = bottle_path.clone();
        move || {
            let install_dir = build::formula::bottle::install_bottle(&bottle, &formula, &cfg)?;
            build::formula::link::link_formula_artifacts(&formula, &install_dir)?;
            Ok::<PathBuf, SapphireError>(build::get_formula_opt_path(&formula))
        }
    })
    .await
    .map_err(join_to_err)??;

    info!("✔ {name} installed → {}", opt_path.display());
    Ok(opt_path)
}

// ==================================================== cask workflow

// BOXING: Change return type from impl Future (implicit) to BoxFuture
// BOXING: Make the function return BoxFuture, not async fn
fn install_casks<'a>(tokens: &'a [String], max_parallel: usize, cfg: &'a Config) -> BoxFuture<'static, Result<()>> {
    // BOXING: Clone owned data needed for the 'static future
    let tokens_owned = tokens.to_vec();
    let cfg_clone = cfg.clone(); // Assuming Config is Clone

    async move {
        // Use owned/cloned data inside the async block
        info!("{}", "🍹 Beginning cask installation…".blue().bold());

        // Use cfg_clone here
        let cache = Arc::new(Cache::new(&cfg_clone.cache_dir).map_err(|e| SapphireError::Cache(e.to_string()))?);
        let sem = Arc::new(Semaphore::new(max_parallel));
        let mut js: JoinSet<(String, Result<()>)> = JoinSet::new();

        // Use tokens_owned here
        for token in tokens_owned.iter().cloned() {
            // BOXING NOTE: Permit acquisition might need adjustment if acquire_owned introduces lifetime issues.
            // Arc::clone(&sem).acquire_owned() should be fine as Semaphore is Send + Sync.
            let permit = sem.clone().acquire_owned().await.map_err(|e| SapphireError::Generic(format!("Semaphore closed: {e}")))?;
            let cache_clone = cache.clone(); // Clone Arc for the task

            js.spawn(async move { // This future needs to be Send + 'static
                // Directly call the task without spinner:
                let res = install_cask_task(&token, &cache_clone).await;
                drop(permit); // Permit is dropped here
                (token, res)
            });
        }

        let mut failures = vec![];
        while let Some(join_res) = js.join_next().await {
            let (token, outcome) = join_res.expect("task panicked");
            match outcome {
                Ok(()) => info!("✔ installed cask {token}"),
                Err(e) => {
                    error!("✖ {token}: {e}");
                    failures.push(token.clone()); // clone token to avoid moving it
                }
            }
            info!("Finished install for: {token}");
        }

        if failures.is_empty() {
            info!("{}", "✅ All casks installed".green().bold());
            Ok(())
        } else {
            Err(SapphireError::InstallError(format!("{} cask(s) failed", failures.len())))
        }
    }
    .boxed() // BOXING: Box the future, erasing its concrete type
}

// Modify install_cask_task slightly for clarity if needed, but the call should work
async fn install_cask_task(token: &str, cache: &Arc<Cache>) -> Result<()> {
    // fetch metadata
    let cask: Cask = sapphire_core::fetch::api::get_cask(token).await?;

    // formula dependencies
    if let Some(deps) = &cask.depends_on {
        // --- Handle Formula Dependencies --- (No change needed here usually)
        if let Some(formulas) = &deps.formula {
            if !formulas.is_empty() {
                info!("Installing formula dependency for cask {token}");
                let cfg = Config::load()?; // Load config as needed
                let dep_args = InstallArgs {
                    names: formulas.clone(),
                    skip_deps: false, cask: false, include_optional: false, skip_recommended: false,
                    max_concurrent_installs: 4, // Consider adjusting concurrency
                };
                // Assuming install_formulae remains async fn
                install_formulae(&dep_args, &cfg).await?;
            }
        }

        // --- Handle Cask Dependencies ---
        if let Some(casks) = &deps.cask {
            if !casks.is_empty() {
                info!("Installing cask dependency for cask {token}");
                let casks_to_install = casks.clone();

                // Spawn the recursive call as a new, independent Tokio task
                let join_handle = tokio::spawn(async move {
                    // Load config inside the new task scope
                    let cfg = Config::load().map_err(|e| SapphireError::Generic(format!("Failed to load config for recursive cask install: {}", e)))?;
                    // Call the modified install_casks function. It returns BoxFuture now,
                    // but .await works directly on it.
                    install_casks(&casks_to_install, 2, &cfg).await // Limit concurrency
                });

                // Await the result (no change needed here)
                match join_handle.await {
                    Ok(Ok(())) => { /* Recursive install succeeded */ }
                    Ok(Err(e)) => return Err(e), // Propagate SapphireError
                    Err(e) => return Err(join_to_err(e)), // Handle JoinError
                }
            }
        }
    }

    // skip if already installed
    if cask.is_installed() {
        info!("cask {token} already installed – skipping");
        return Ok(());
    }

    info!("Downloading cask {token}…");
    let dl = build::cask::download_cask(&cask, cache.as_ref()).await?;
    info!("Installing cask {token}…");
    tokio::task::spawn_blocking({
        let cask = cask.clone();
        let dl   = dl.clone();
        move || build::cask::install_cask(&cask, &dl)
    })
    .await
    .map_err(join_to_err)??;

    info!("✔ cask {token} installed successfully");
    Ok(())
}
