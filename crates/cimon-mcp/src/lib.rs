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
    Router::new()
        .route("/healthz", get(healthz))
        .route("/artifacts/{job}/{name}", get(download))
        .with_state(app.clone())
        .merge(mcp_routes)
}

#[derive(serde::Deserialize)]
struct LinkQuery {
    t: Option<String>,
}

/// `GET /artifacts/j<id>/<name>?t=<token>` (design §12.3): HMAC link token, `410 Gone`
/// after retention, `Range` support.
async fn download(
    State(app): State<Arc<App>>,
    axum::extract::Path((job, name)): axum::extract::Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<LinkQuery>,
    req: Request<Body>,
) -> Response {
    let Some(id) = cimon_core::parse_job_label(&job) else {
        return (StatusCode::NOT_FOUND, "no such job").into_response();
    };
    let key = cimon_engine::artifacts::link_key(&app);
    if let Err(e) = cimon_engine::artifacts::verify_token(&key, id, &name, q.t.as_deref().unwrap_or("")) {
        return (StatusCode::FORBIDDEN, e).into_response();
    }
    let row = match app.store.artifact(id, &name) {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such artifact").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if let Some(when) = row.deleted_at.clone() {
        return (StatusCode::GONE, format!("{} 의 산출물은 {when} 에 만료돼 지웠다", cimon_core::job_label(id))).into_response();
    }
    let path = std::path::PathBuf::from(&row.path);
    if !path.starts_with(app.layout.artifacts()) || !path.is_file() {
        return (StatusCode::GONE, format!("{} 의 산출물 {name} 이 더 이상 없다", cimon_core::job_label(id))).into_response();
    }
    let file_name = row
        .inspect_json
        .as_deref()
        .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
        .and_then(|v| v.get("file_name").and_then(|f| f.as_str()).map(str::to_string))
        .unwrap_or_else(|| name.clone());
    let _ = app.store.audit(cimon_store::AuditEntry { user: "link", action: "download", job_id: Some(id), detail: Some(name.clone()), ..Default::default() });
    use tower::ServiceExt;
    let mut resp = match tower_http::services::ServeFile::new(&path).oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let safe: String = file_name.chars().filter(|c| c.is_ascii_graphic() && *c != '"' && *c != '\\').collect();
    if let Ok(v) = http::HeaderValue::from_str(&format!("attachment; filename=\"{safe}\"")) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp.headers_mut().insert("x-content-sha256", http::HeaderValue::from_str(&row.sha256).unwrap());
    resp
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
