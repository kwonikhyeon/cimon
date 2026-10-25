//! MCP endpoint (`POST /mcp`, revision 2026-07-28 + Tasks extension) and its HTTP
//! plumbing: bearer-token auth in front of rmcp, identity-header audit, `/healthz`.

pub mod server;
pub mod tools;

use axum::{
    body::Body,
    extract::State,
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use cimon_core::AuthUser;
use cimon_engine::App;
use rmcp::transport::streamable_http_server::{session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub use server::CimonMcp;

/// Builds the router serving `/mcp` and `/healthz`. The caller adds artifact and web
/// routes and binds it to `127.0.0.1` only (design §15.3).
pub fn router(app: Arc<App>, shutdown: CancellationToken) -> Router {
    let mut hosts: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    hosts.extend(app.config.server.allowed_hosts.iter().cloned());
    let factory_app = app.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(CimonMcp::new(factory_app.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_sse_keep_alive(None)
            .with_json_response(true)
            .with_allowed_hosts(hosts)
            .with_cancellation_token(shutdown),
    );
    let mcp_routes = Router::new()
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(app.clone(), require_token));
    Router::new().route("/healthz", get(healthz)).with_state(app.clone()).merge(mcp_routes)
}

/// Rejects requests without a valid user token with HTTP 401 before rmcp sees them,
/// and attaches the [`AuthUser`] for the tool handlers.
async fn require_token(State(app): State<Arc<App>>, mut req: Request<Body>, next: Next) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(|s| s.trim().to_string());
    let user = match token {
        Some(t) if !t.is_empty() => match app.store.authenticate(&t) {
            Ok(u) => u,
            Err(e) => {
                tracing::error!("auth lookup failed: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "auth lookup failed").into_response();
            }
        },
        _ => None,
    };
    let Some(user) = user else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer realm=\"cimon\"")],
            "cimon: missing or invalid token — see design §15.4",
        )
            .into_response();
    };
    if app.config.tailscale.audit_identity_headers {
        let login = req.headers().get("Tailscale-User-Login").and_then(|v| v.to_str().ok()).map(str::to_string);
        if let Some(login) = &login {
            // Identity headers are audit-only: a local process can forge them (design §15.2).
            if !login.split('@').next().unwrap_or_default().eq_ignore_ascii_case(&user.name) {
                tracing::warn!(user = %user.name, tailnet_login = %login, "token user differs from Tailscale identity header");
            }
        }
        req.extensions_mut().insert(TailnetLogin(login));
    }
    req.extensions_mut().insert(user);
    next.run(req).await
}

/// `Tailscale-User-Login` as received (audit only).
#[derive(Debug, Clone)]
pub struct TailnetLogin(pub Option<String>);

async fn healthz(State(app): State<Arc<App>>) -> impl IntoResponse {
    Json(cimon_engine::health(&app))
}

/// The authenticated user stored by [`require_token`] in the request parts rmcp hands
/// to tool handlers.
pub(crate) fn auth_user(parts: Option<&http::request::Parts>) -> Option<AuthUser> {
    parts.and_then(|p| p.extensions.get::<AuthUser>().cloned())
}

pub(crate) fn tailnet_login(parts: Option<&http::request::Parts>) -> Option<String> {
    parts.and_then(|p| p.extensions.get::<TailnetLogin>().and_then(|t| t.0.clone()))
}
