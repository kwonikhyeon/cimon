//! Uploads over HTTP (design §11.7). `POST /uploads?kind=file|text&label=<name>` takes
//! the raw bytes with a user token and answers the handle a build call passes as
//! `uploads`; `GET /upload.ps1` / `GET /upload.sh` serve the script that sends them
//! from the caller's machine, so the bytes never pass through the conversation.

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use cimon_core::AuthUser;
use cimon_engine::App;
use std::sync::Arc;

const PS1: &str = include_str!("upload.ps1");
const SH: &str = include_str!("upload.sh");

pub async fn ps1(State(app): State<Arc<App>>) -> Response {
    crate::connect::script(&app, PS1)
}

pub async fn sh(State(app): State<Arc<App>>) -> Response {
    crate::connect::script(&app, SH)
}

#[derive(serde::Deserialize)]
pub struct UploadQuery {
    label: String,
    #[serde(default)]
    kind: Option<String>,
}

pub async fn receive(State(app): State<Arc<App>>, Extension(user): Extension<AuthUser>, Query(q): Query<UploadQuery>, body: Bytes) -> Response {
    let a = app.clone();
    let res = tokio::task::spawn_blocking(move || cimon_engine::uploads::receive(&a, &user.name, &q.label, q.kind.as_deref().unwrap_or("file"), &body)).await;
    match res {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => {
            let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST);
            (status, Json(serde_json::json!({"error": e.code, "message": e.message}))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
