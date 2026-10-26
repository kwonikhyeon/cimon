//! Team-member onboarding (design §16.3): `GET /connect.ps1` / `GET /connect.sh` serve
//! a script with this server's address in it, and `POST /connect/redeem` trades a
//! one-time invite code (`cimon invite`) for a new user's token. The script stores the
//! token in the OS keystore and registers cimon with Claude Code, so the token never
//! passes through a person's hands.
//!
//! These routes take no token — the tailnet is the gate, the code the credential.
//! Failed codes are slowed down and, past a limit, refused for a while.

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use cimon_engine::App;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PS1: &str = include_str!("connect.ps1");
const SH: &str = include_str!("connect.sh");

/// Failed redeems in the last window; past the limit every redeem is refused.
static FAILURES: Mutex<VecDeque<Instant>> = Mutex::new(VecDeque::new());
const WINDOW: Duration = Duration::from_secs(600);
const MAX_FAILURES: usize = 20;

fn script(app: &App, template: &str) -> Response {
    let url = app.config.server.public_url.trim_end_matches('/');
    // the address goes inside single quotes in both scripts
    if url.is_empty() || !url.chars().all(|c| c.is_ascii_graphic() && c != '\'' && c != '"' && c != '`' && c != '$' && c != '\\') {
        return (StatusCode::INTERNAL_SERVER_ERROR, "[server].public_url cannot go into a script").into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::X_CONTENT_TYPE_OPTIONS, "nosniff"), (header::CACHE_CONTROL, "no-store")],
        template.replace("{{URL}}", url),
    )
        .into_response()
}

pub async fn ps1(State(app): State<Arc<App>>) -> Response {
    script(&app, PS1)
}

pub async fn sh(State(app): State<Arc<App>>) -> Response {
    script(&app, SH)
}

#[derive(serde::Deserialize)]
pub struct Redeem {
    code: String,
    #[serde(default)]
    device: Option<String>,
}

pub async fn redeem(State(app): State<Arc<App>>, headers: HeaderMap, Json(r): Json<Redeem>) -> Response {
    let recent = {
        let mut f = FAILURES.lock().unwrap_or_else(|p| p.into_inner());
        while f.front().is_some_and(|t| t.elapsed() > WINDOW) {
            f.pop_front();
        }
        f.len()
    };
    if recent >= MAX_FAILURES {
        return (StatusCode::TOO_MANY_REQUESTS, Json(serde_json::json!({"error": "too_many_attempts", "message": "too many wrong invite codes — wait 10 minutes"}))).into_response();
    }
    let login = headers.get("Tailscale-User-Login").and_then(|v| v.to_str().ok()).map(str::to_string);
    let device: String = r.device.unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).take(64).collect();
    let a = app.clone();
    let code = r.code.clone();
    let l2 = login.clone();
    let res = tokio::task::spawn_blocking(move || a.store.redeem_invite(&code, l2.as_deref())).await;
    match res {
        Ok(Ok(Some((user, role, token)))) => {
            let _ = app.store.audit(cimon_store::AuditEntry {
                user: &user,
                action: "invite_redeemed",
                tailnet_login: login.as_deref(),
                detail: Some(format!("device {device}")),
                ..Default::default()
            });
            Json(serde_json::json!({
                "user": user,
                "role": role.as_str(),
                "token": token,
                "url": app.config.server.public_url.trim_end_matches('/'),
            }))
            .into_response()
        }
        Ok(Ok(None)) => {
            FAILURES.lock().unwrap_or_else(|p| p.into_inner()).push_back(Instant::now());
            let _ = app.store.audit(cimon_store::AuditEntry { user: "?", action: "invite_failed", tailnet_login: login.as_deref(), detail: Some(format!("device {device}")), ..Default::default() });
            // a wrong guess costs time
            tokio::time::sleep(Duration::from_millis(500)).await;
            (StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "invalid_code", "message": "the invite code is wrong, used or expired — ask the admin for a new one (cimon invite)"}))).into_response()
        }
        Ok(Err(e)) => (StatusCode::CONFLICT, Json(serde_json::json!({"error": "conflict", "message": e.to_string()}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
