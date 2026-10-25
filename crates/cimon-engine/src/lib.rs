//! The service core: configuration, job store, recipe store, job lifecycle (intake →
//! provisioning → ready → running → done), the runner loop and the janitor.

use cimon_core::{Config, Layout};
use cimon_store::Store;
use std::sync::Arc;

pub struct App {
    pub config: Config,
    pub layout: Layout,
    pub store: Store,
}

impl App {
    pub fn open(config: Config) -> anyhow::Result<Arc<App>> {
        let layout = Layout::new(&config.paths.root);
        let store = Store::open(&layout.db())?;
        Ok(Arc::new(App { config, layout, store }))
    }
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
        "version": env!("CARGO_PKG_VERSION"),
    })
}
