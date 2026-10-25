//! The service core: configuration, job store, recipe store, job lifecycle (intake →
//! provisioning → ready → running → done), the runner loop and the janitor.

pub mod artifacts;
pub mod backup;
pub mod fixtures;
pub mod janitor;
pub mod jobs;
pub mod proposals;
pub mod recipes;
pub mod result;
pub mod run;
pub mod toolchains;

use cimon_core::{Config, Layout};
use cimon_recipestore::RecipeStore;
use cimon_store::Store;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// Runtime handles shared by workers and tool calls.
pub struct Engine {
    pub intake: Notify,
    pub runner: Notify,
    /// Cancel flags of jobs currently on the runner.
    pub running: Mutex<HashMap<i64, Arc<AtomicBool>>>,
    pub shutdown: CancellationToken,
    /// The build account, when not in single-account mode (logged on at start).
    pub build_account: Mutex<Option<Arc<cimon_runner::BuildAccount>>>,
    /// `cimon.exe` used for the `__exec` / `__sweep` / `__account-env` helpers.
    pub helper: Mutex<Option<PathBuf>>,
    /// Held by the runner while a job runs, and by the janitor while it deletes.
    pub runner_lock: tokio::sync::Mutex<()>,
    /// Wakes the janitor (disk floor reached during a call).
    pub janitor: Notify,
    /// One lock per mirror: intake fetches/reads and GC never overlap.
    pub mirror_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Held while anything runs as the build account (a job on the runner, an install
    /// script or probe during intake): a job's start/end sweep terminates every
    /// build-account process, so the two must not overlap.
    pub build_lock: Mutex<()>,
}

impl Engine {
    pub fn mirror_lock(&self, key: &str) -> Arc<Mutex<()>> {
        self.mirror_locks.lock().unwrap().entry(key.to_string()).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
    }
}

pub struct App {
    pub config: Config,
    pub layout: Layout,
    pub store: Store,
    pub recipes: RecipeStore,
    pub engine: Engine,
}

impl App {
    pub fn open(config: Config) -> anyhow::Result<Arc<App>> {
        let layout = Layout::new(&config.paths.root);
        let store = Store::open(&layout.db())?;
        let recipes = RecipeStore::open(&config.recipes_store(), &config.recipes.default_branch, &config.recipes.trial_prefix)
            .map_err(|e| anyhow::anyhow!("opening the recipe store: {e}"))?;
        let engine = Engine {
            intake: Notify::new(),
            runner: Notify::new(),
            running: Mutex::new(HashMap::new()),
            shutdown: CancellationToken::new(),
            build_account: Mutex::new(None),
            helper: Mutex::new(std::env::current_exe().ok()),
            runner_lock: tokio::sync::Mutex::new(()),
            janitor: Notify::new(),
            mirror_locks: Mutex::new(HashMap::new()),
            build_lock: Mutex::new(()),
        };
        Ok(Arc::new(App { config, layout, store, recipes, engine }))
    }

    pub fn build_account(&self) -> Option<Arc<cimon_runner::BuildAccount>> {
        self.engine.build_account.lock().unwrap().clone()
    }

    pub fn helper(&self) -> Option<PathBuf> {
        self.engine.helper.lock().unwrap().clone()
    }
}

/// Starts the background workers: recovery, intake, runner, ticker. Returns once they
/// are spawned; they stop on `app.engine.shutdown`.
pub fn start(app: &Arc<App>) -> anyhow::Result<()> {
    for d in app.layout.all_dirs() {
        std::fs::create_dir_all(&d)?;
    }
    if !app.config.accounts.single_account() {
        #[cfg(windows)]
        {
            let helper = app.helper().ok_or_else(|| anyhow::anyhow!("cannot locate cimon.exe for the build-account helper"))?;
            let acct = cimon_runner::BuildAccount::logon(&app.config.accounts.build, &app.layout.config_dir().join("build-account.dpapi"), &helper)
                .map_err(|e| anyhow::anyhow!("build account {}: {e}", app.config.accounts.build))?;
            *app.engine.build_account.lock().unwrap() = Some(Arc::new(acct));
        }
    }
    for n in jobs::recover(app) {
        tracing::warn!("{n}");
    }
    match app.recipes.reconcile(&app.store) {
        Ok(notes) => notes.iter().for_each(|n| tracing::warn!("{n}")),
        Err(e) => tracing::error!("proposal reconciliation failed: {e}"),
    }
    janitor::startup(app);
    tokio::spawn(jobs::intake_loop(app.clone()));
    tokio::spawn(run::runner_loop(app.clone()));
    tokio::spawn(janitor::ticker(app.clone()));
    Ok(())
}

/// `GET /healthz` body (design §16.4).
pub fn health(app: &App) -> serde_json::Value {
    use cimon_core::JobState::*;
    let queued = app.store.jobs_in_states(&[Intake, Provisioning, Ready]).map(|v| v.len()).unwrap_or(0);
    let running = app.store.jobs_in_states(&[Running, Collecting, Cancelling]).map(|v| v.len()).unwrap_or(0);
    serde_json::json!({
        "ok": true,
        "queued": queued,
        "running": running,
        "draining": app.layout.draining_flag().exists(),
        "disk_free_gb": cimon_runner::free_bytes(&app.layout.root).map(|b| b / (1 << 30)),
        "recipes_main": app.recipes.main_commit(),
        "last_recipe_backup": app.store.last_backup("recipes").ok().flatten(),
        "last_recipe_offsite": app.store.last_offsite("recipes").ok().flatten(),
        "last_db_offsite": app.store.last_offsite("db").ok().flatten(),
        "warnings": backup::warnings(app),
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// Git environment carrying the stored GitHub token (`cimon github-token`), if any.
pub fn git_auth(app: &App) -> Vec<(String, String)> {
    let p = app.layout.config_dir().join("github-token.dpapi");
    let Ok(blob) = std::fs::read(&p) else { return vec![] };
    #[cfg(windows)]
    let token = cimon_runner::win::unprotect(&blob).ok().and_then(|b| String::from_utf8(b).ok());
    #[cfg(not(windows))]
    let token: Option<String> = String::from_utf8(blob).ok();
    match token {
        Some(t) if !t.trim().is_empty() => cimon_workspace::auth_env(t.trim()),
        _ => {
            tracing::error!("{} exists but cannot be decrypted — fetches go without credentials", p.display());
            vec![]
        }
    }
}

/// Toolchain state for `plan` (design §7.2).
pub fn provision_needs(app: &App, v: &recipes::Validated) -> serde_json::Value {
    toolchains::plan_view(app, v)
}
