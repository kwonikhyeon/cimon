//! The service core: configuration, job store, recipe store, job lifecycle (intake →
//! provisioning → ready → running → done), the runner loop and the janitor.

pub mod backup;
pub mod recipes;

use cimon_core::{Config, Layout};
use cimon_recipestore::RecipeStore;
use cimon_store::Store;
use std::sync::Arc;

pub struct App {
    pub config: Config,
    pub layout: Layout,
    pub store: Store,
    pub recipes: RecipeStore,
}

impl App {
    pub fn open(config: Config) -> anyhow::Result<Arc<App>> {
        let layout = Layout::new(&config.paths.root);
        let store = Store::open(&layout.db())?;
        let recipes = RecipeStore::open(&config.recipes_store(), &config.recipes.default_branch, &config.recipes.trial_prefix)
            .map_err(|e| anyhow::anyhow!("opening the recipe store: {e}"))?;
        Ok(Arc::new(App { config, layout, store, recipes }))
    }
}

/// Toolchain needs for a plan. Filled in by the toolchain manager (M3); until then
/// the kinds are listed without install state.
pub fn provision_needs(_app: &App, v: &recipes::Validated) -> serde_json::Value {
    serde_json::json!(v.plan.toolchains)
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
        "recipes_main": app.recipes.main_commit(),
        "last_recipe_backup": app.store.last_backup("recipes").ok().flatten(),
        "last_recipe_offsite": app.store.last_offsite("recipes").ok().flatten(),
        "last_db_offsite": app.store.last_offsite("db").ok().flatten(),
        "warnings": backup::warnings(app),
        "version": env!("CARGO_PKG_VERSION"),
    })
}
