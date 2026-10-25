//! The long-running service: HTTP server plus background workers.

use anyhow::{Context, Result};
use cimon_core::Config;
use cimon_engine::App;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Runs until `shutdown` is cancelled (Windows service stop) or Ctrl-C.
pub async fn run(cfg: Config, shutdown: Option<CancellationToken>) -> Result<()> {
    if cfg.tailscale.require_funnel_off {
        crate::tailscale::ensure_funnel_off()?;
    }
    let shutdown = shutdown.unwrap_or_default();
    let app = App::open(cfg).context("opening the job store")?;
    let (addr, server) = start(app, shutdown.clone()).await?;
    tracing::info!("cimon listening on http://{addr}");
    let ctrl_c = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        ctrl_c.cancel();
    });
    server.await??;
    Ok(())
}

/// Binds and starts serving; returns the bound address and the server task. Used by
/// `run` and by integration tests (which bind port 0).
pub async fn start(
    app: Arc<App>,
    shutdown: CancellationToken,
) -> Result<(std::net::SocketAddr, tokio::task::JoinHandle<std::io::Result<()>>)> {
    let bind = app.config.server.bind.clone();
    let listener = tokio::net::TcpListener::bind(&bind).await.with_context(|| format!("binding {bind}"))?;
    let addr = listener.local_addr()?;
    if !addr.ip().is_loopback() {
        anyhow::bail!("[server].bind must be a loopback address — tailscale serve is the only way in (design §15.3), got {addr}");
    }
    let router = cimon_mcp::router(app.clone(), shutdown.clone());
    let sd = shutdown.clone();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).with_graceful_shutdown(async move { sd.cancelled_owned().await }).await
    });
    Ok((addr, handle))
}
