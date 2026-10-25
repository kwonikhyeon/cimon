//! Recipe review screen (docs/review-ui.md, M9): server-rendered HTML, no scripts,
//! strict CSP, cookie sessions entered through one-time links from `web_login`.
//! Every action calls the same engine functions as the MCP tools and re-checks the
//! role on the server.

mod md;
mod views;

use axum::{
    body::Body,
    extract::{Form, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use cimon_core::AuthUser;
use cimon_engine::App;
use serde::Deserialize;
use std::sync::Arc;

pub const COOKIE: &str = "cimon_session";

/// Routes under `/ui`.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/ui", get(|| async { Redirect::to("/ui/") }))
        .route("/ui/", get(home))
        .route("/ui/login", get(login_code).post(login_token))
        .route("/ui/logout", post(logout))
        .route("/ui/proposals", get(proposals))
        .route("/ui/proposals/{id}", get(proposal))
        .route("/ui/proposals/{id}/promote", post(promote))
        .route("/ui/proposals/{id}/close", post(close))
        .route("/ui/proposals/{id}/comment", post(comment))
        .route("/ui/history", get(history))
        .route("/ui/history/revert", post(revert))
        .route("/ui/recipes/{rref}/projects/{id}", get(project))
        .route("/ui/recipes/{rref}/tree", get(tree_root))
        .route("/ui/recipes/{rref}/tree/{*path}", get(tree))
        .route("/ui/jobs/{job}", get(job))
        .route("/ui/style.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8"), (header::CACHE_CONTROL, "max-age=3600")], views::CSS) }))
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}

/// CSP and friends on every response (review-ui §10.3).
async fn security_headers(req: Request<Body>, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// The signed-in user, its CSRF token and session id.
pub struct Session {
    pub user: AuthUser,
    pub csrf: String,
}

fn session(app: &App, headers: &HeaderMap) -> Option<Session> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    let sid = cookies.split(';').filter_map(|c| c.trim().split_once('=')).find(|(k, _)| *k == COOKIE).map(|(_, v)| v.to_string())?;
    let s = app.store.web_session(&sid).ok()??;
    let user = app.store.user(&s.user).ok()??;
    Some(Session { user, csrf: s.csrf })
}

fn need(app: &App, headers: &HeaderMap) -> Result<Session, Response> {
    session(app, headers).ok_or_else(|| (StatusCode::UNAUTHORIZED, Html(views::login_page(None).into_string())).into_response())
}

/// Compares without an early exit, so response time says nothing about how much of a
/// guessed token was right.
fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn check_csrf(s: &Session, token: &str) -> Result<(), Response> {
    if token.is_empty() || !same_secret(token, &s.csrf) {
        return Err((StatusCode::FORBIDDEN, "CSRF token mismatch — reload the page").into_response());
    }
    Ok(())
}

fn cookie(app: &App, sid: &str, max_age: i64) -> HeaderValue {
    let secure = if app.config.server.public_url.starts_with("https://") { "; Secure" } else { "" };
    HeaderValue::from_str(&format!("{COOKIE}={sid}; HttpOnly; SameSite=Strict; Path=/ui; Max-Age={max_age}{secure}")).unwrap()
}

fn page(content: maud::Markup) -> Response {
    Html(content.into_string()).into_response()
}

fn fail(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Html(views::message_page(&msg.into()).into_string())).into_response()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("blocking task")
}

#[derive(Deserialize)]
struct CodeQ {
    code: Option<String>,
}

/// One-time link from `web_login` (review-ui §8.1).
async fn login_code(State(app): State<Arc<App>>, Query(q): Query<CodeQ>) -> Response {
    let Some(code) = q.code else { return page(views::login_page(None)) };
    let a = app.clone();
    let user = blocking(move || a.store.consume_login_code(&code)).await;
    match user {
        Ok(Some(u)) => start_session(&app, &u).await,
        _ => (StatusCode::UNAUTHORIZED, Html(views::login_page(Some("링크가 만료됐거나 이미 쓰였다 — Claude 에게 새 로그인 링크를 요청하라")).into_string())).into_response(),
    }
}

#[derive(Deserialize)]
struct TokenForm {
    token: String,
}

/// Fallback: paste a token (review-ui §8.1, item 3).
async fn login_token(State(app): State<Arc<App>>, Form(f): Form<TokenForm>) -> Response {
    let a = app.clone();
    let user = blocking(move || a.store.authenticate(f.token.trim())).await;
    match user {
        Ok(Some(u)) => start_session(&app, &u.name).await,
        _ => (StatusCode::UNAUTHORIZED, Html(views::login_page(Some("토큰이 맞지 않는다")).into_string())).into_response(),
    }
}

async fn start_session(app: &Arc<App>, user: &str) -> Response {
    let a = app.clone();
    let u = user.to_string();
    let hours = app.config.web.session_hours;
    match blocking(move || a.store.create_web_session(&u, hours)).await {
        Ok((sid, _)) => {
            let _ = app.store.audit(cimon_store::AuditEntry { user, action: "web_login", ..Default::default() });
            let mut r = Redirect::to("/ui/").into_response();
            r.headers_mut().insert(header::SET_COOKIE, cookie(app, &sid, hours as i64 * 3600));
            r
        }
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct CsrfForm {
    #[serde(default)]
    csrf: String,
}

async fn logout(State(app): State<Arc<App>>, headers: HeaderMap, Form(f): Form<CsrfForm>) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = check_csrf(&s, &f.csrf) {
        return r;
    }
    if let Some(sid) = headers.get(header::COOKIE).and_then(|c| c.to_str().ok()).and_then(|c| c.split(';').filter_map(|x| x.trim().split_once('=')).find(|(k, _)| *k == COOKIE).map(|(_, v)| v.to_string())) {
        let _ = app.store.revoke_web_session(&sid);
    }
    let mut r = Redirect::to("/ui/login").into_response();
    r.headers_mut().insert(header::SET_COOKIE, cookie(&app, "", 0));
    r
}

async fn home(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let a = app.clone();
    page(blocking(move || views::home(&a, &s)).await)
}

#[derive(Deserialize)]
struct StatusQ {
    status: Option<String>,
}

async fn proposals(State(app): State<Arc<App>>, headers: HeaderMap, Query(q): Query<StatusQ>) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let a = app.clone();
    page(blocking(move || views::proposals(&a, &s, q.status.as_deref())).await)
}

async fn proposal(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<TabQ>) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let a = app.clone();
    blocking(move || match cimon_engine::proposals::find(&a, &id) {
        Ok(p) => page(views::proposal(&a, &s, &p, q.tab.as_deref().unwrap_or("semantic"), q.msg.as_deref())),
        Err(e) => fail(StatusCode::NOT_FOUND, e.message),
    })
    .await
}

#[derive(Deserialize)]
struct TabQ {
    tab: Option<String>,
    msg: Option<String>,
}

#[derive(Deserialize)]
struct PromoteForm {
    #[serde(default)]
    csrf: String,
    head: String,
    #[serde(default)]
    override_reason: String,
}

/// Promote with the head the page showed; conditions recomputed (review-ui §4.7).
async fn promote(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>, Form(f): Form<PromoteForm>) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = check_csrf(&s, &f.csrf) {
        return r;
    }
    if !s.user.is_admin() {
        return fail(StatusCode::FORBIDDEN, "반영은 관리자만 할 수 있다 — 관리자에게 요청하라");
    }
    let a = app.clone();
    blocking(move || {
        let p = match cimon_engine::proposals::find(&a, &id) {
            Ok(p) => p,
            Err(e) => return fail(StatusCode::NOT_FOUND, e.message),
        };
        let reason = Some(f.override_reason.trim().to_string()).filter(|r| !r.is_empty());
        match cimon_engine::proposals::promote(&a, &s.user, &p, &f.head, reason.as_deref()) {
            Ok(_) => Redirect::to(&format!("/ui/proposals/{}?msg=promoted", cimon_core::proposal_label(p.id))).into_response(),
            Err(e) => fail(StatusCode::CONFLICT, format!("[{}] {}", e.code, e.message)),
        }
    })
    .await
}

#[derive(Deserialize)]
struct CloseForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    reason: String,
}

async fn close(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>, Form(f): Form<CloseForm>) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = check_csrf(&s, &f.csrf) {
        return r;
    }
    let a = app.clone();
    blocking(move || {
        let p = match cimon_engine::proposals::find(&a, &id) {
            Ok(p) => p,
            Err(e) => return fail(StatusCode::NOT_FOUND, e.message),
        };
        if p.author != s.user.name && !s.user.is_admin() {
            return fail(StatusCode::FORBIDDEN, "제안은 작성자나 관리자만 닫는다");
        }
        let reason = Some(f.reason.trim().to_string()).filter(|r| !r.is_empty());
        match a.recipes.close(&a.store, &s.user.name, p.id, reason.as_deref()) {
            Ok(()) => {
                let _ = a.store.audit(cimon_store::AuditEntry { user: &s.user.name, action: "proposal_close", proposal_id: Some(p.id), detail: reason, ..Default::default() });
                Redirect::to(&format!("/ui/proposals/{}", cimon_core::proposal_label(p.id))).into_response()
            }
            Err(e) => fail(StatusCode::CONFLICT, e.to_string()),
        }
    })
    .await
}

#[derive(Deserialize)]
struct CommentForm {
    #[serde(default)]
    csrf: String,
    body: String,
}

async fn comment(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>, Form(f): Form<CommentForm>) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = check_csrf(&s, &f.csrf) {
        return r;
    }
    if f.body.trim().is_empty() || f.body.len() > 20_000 {
        return fail(StatusCode::BAD_REQUEST, "댓글은 1~20000자");
    }
    let a = app.clone();
    blocking(move || {
        let p = match cimon_engine::proposals::find(&a, &id) {
            Ok(p) => p,
            Err(e) => return fail(StatusCode::NOT_FOUND, e.message),
        };
        let head = a.recipes.branch_head(&p.branch).or(p.promoted_commit.clone()).unwrap_or_default();
        match a.store.add_comment(p.id, &s.user.name, &f.body, &head) {
            Ok(_) => {
                let _ = a.store.audit(cimon_store::AuditEntry { user: &s.user.name, action: "comment", proposal_id: Some(p.id), ..Default::default() });
                Redirect::to(&format!("/ui/proposals/{}?tab=comments", cimon_core::proposal_label(p.id))).into_response()
            }
            Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        }
    })
    .await
}

async fn history(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let a = app.clone();
    page(blocking(move || views::history(&a, &s)).await)
}

#[derive(Deserialize)]
struct RevertForm {
    #[serde(default)]
    csrf: String,
    commit: String,
    #[serde(default)]
    reason: String,
}

async fn revert(State(app): State<Arc<App>>, headers: HeaderMap, Form(f): Form<RevertForm>) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = check_csrf(&s, &f.csrf) {
        return r;
    }
    if !s.user.is_admin() {
        return fail(StatusCode::FORBIDDEN, "되돌리기는 관리자만 할 수 있다");
    }
    let a = app.clone();
    blocking(move || match a.recipes.revert(&s.user.name, &f.commit, &f.reason) {
        Ok(new) => {
            let _ = a.store.audit(cimon_store::AuditEntry { user: &s.user.name, action: "recipe_revert", recipe_commit: Some(&new), detail: Some(format!("{} — {}", f.commit, f.reason)), ..Default::default() });
            let _ = cimon_engine::backup::backup_recipes(&a, "revert");
            Redirect::to("/ui/history").into_response()
        }
        Err(e) => fail(StatusCode::CONFLICT, e.to_string()),
    })
    .await
}

async fn project(State(app): State<Arc<App>>, headers: HeaderMap, Path((rref, id)): Path<(String, String)>) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let a = app.clone();
    blocking(move || {
        let rref = rref.replace('~', "/");
        match a.recipes.resolve(&rref).and_then(|c| a.recipes.tree(&c).map(|t| (c, t))) {
            Ok((commit, t)) => match cimon_recipe::resolve_project(&*t, &id) {
                Ok(r) => page(views::project(&a, &s, &rref, &commit, &r)),
                Err(d) => fail(StatusCode::NOT_FOUND, d.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("\n")),
            },
            Err(e) => fail(StatusCode::NOT_FOUND, e.to_string()),
        }
    })
    .await
}

async fn tree_root(state: State<Arc<App>>, headers: HeaderMap, Path(rref): Path<String>) -> Response {
    tree(state, headers, Path((rref, String::new()))).await
}

async fn tree(State(app): State<Arc<App>>, headers: HeaderMap, Path((rref, path)): Path<(String, String)>) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let a = app.clone();
    blocking(move || {
        let rref = rref.replace('~', "/");
        match a.recipes.resolve(&rref).and_then(|c| a.recipes.show(&c, &path).map(|r| (c, r))) {
            Ok((commit, shown)) => page(views::tree(&a, &s, &rref, &commit, &shown)),
            Err(e) => fail(StatusCode::NOT_FOUND, e.to_string()),
        }
    })
    .await
}

async fn job(State(app): State<Arc<App>>, headers: HeaderMap, Path(job): Path<String>) -> Response {
    let s = match need(&app, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let a = app.clone();
    blocking(move || {
        let Some(id) = cimon_core::parse_job_label(&job) else { return fail(StatusCode::NOT_FOUND, "no such job") };
        match a.store.job(id) {
            Ok(Some(j)) => page(views::job(&a, &s, &j)),
            _ => fail(StatusCode::NOT_FOUND, format!("{} does not exist or its record expired", cimon_core::job_label(id))),
        }
    })
    .await
}

/// `web_login` (review-ui §9): a one-time link valid for `[web].login_code_minutes`.
pub fn login_link(app: &App, user: &str) -> Result<String, String> {
    if !app.config.web.enabled {
        return Err("리뷰 화면이 꺼져 있다([web].enabled = false) — recipe_proposal 로 보라".into());
    }
    let code = app.store.create_login_code(user, app.config.web.login_code_minutes).map_err(|e| e.to_string())?;
    Ok(format!("{}/ui/login?code={code}", app.config.server.public_url.trim_end_matches('/')))
}

pub use views::CSS;
