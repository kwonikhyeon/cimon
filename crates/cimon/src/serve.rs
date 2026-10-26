//! The long-running service: HTTP server plus background workers.

use anyhow::{Context, Result};
use cimon_core::Config;
use cimon_engine::App;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Runs until `shutdown` is cancelled (Windows service stop) or Ctrl-C.
pub async fn run(cfg: Config, shutdown: Option<CancellationToken>) -> Result<()> {
    let mut notices = vec![];
    if cfg.tailscale.require_funnel_off {
        match crate::tailscale::check_funnel()? {
            crate::tailscale::Funnel::Off => {}
            crate::tailscale::Funnel::Unobservable(why) if cfg.tailscale.allow_unverified_funnel => {
                let w = format!(
                    "Tailscale Funnel 이 꺼졌는지 서비스 계정으로 확인하지 못했다({why}) — [tailscale].allow_unverified_funnel 에 따라 시작했다. Tailscale 을 쓰는 사용자로 `cimon doctor` 를 돌려 확인하라"
                );
                tracing::warn!("{w}");
                notices.push(w);
            }
            crate::tailscale::Funnel::Unobservable(why) => anyhow::bail!(
                "cannot confirm Tailscale Funnel is off: {why}. If Tailscale answers only the user who owns it (Windows), set [tailscale].allow_unverified_funnel = true and check Funnel with `cimon doctor` as that user"
            ),
        }
    }
    let shutdown = shutdown.unwrap_or_default();
    let app = App::open(cfg).context("opening the job store")?;
    app.engine.notices.lock().unwrap().extend(notices);
    // an upgrade drains, stops, replaces and starts: a started service takes jobs again
    let _ = std::fs::remove_file(app.layout.draining_flag());
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
    // the engine's workers stop with the HTTP server, and a running job's processes
    // are terminated (the job is recorded as interrupted) instead of holding the stop
    let app2 = app.clone();
    let sd2 = shutdown.clone();
    tokio::spawn(async move {
        sd2.cancelled().await;
        app2.engine.shutdown.cancel();
        for flag in app2.engine.running.lock().unwrap().values() {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });
    cimon_engine::start(&app)?;
    let mut router = cimon_mcp::router(app.clone(), shutdown.clone());
    if app.config.web.enabled {
        router = router.merge(cimon_web::router(app.clone()));
    }
    let sd = shutdown.clone();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).with_graceful_shutdown(async move { sd.cancelled_owned().await }).await
    });
    Ok((addr, handle))
}
