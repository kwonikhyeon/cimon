//! Uploads (design §11.7): a file or a one-line value the caller sends from their own
//! machine for one job — a release signing key, a site's TLS key, their passwords —
//! that the build machine does not keep. The caller's upload script sends it straight
//! to `POST /uploads` (the bytes never pass through the conversation) and gets a
//! handle; the `build`/`test`/`run` call names the handle under the recipe's
//! `[upload.<name>]`. The service keeps the bytes DPAPI-protected under
//! `state\uploads\`, hands them to that one job, and deletes them when it ends.
//!
//! What a job that gets uploads may be is narrow: only steps in the upload's
//! `applies_to` see it, the recipe must be main's (a trial recipe could send it
//! anywhere), the ref a branch the upload's `refs` allow, the work tree a one-off clone
//! and the package caches the job's own — another job cannot have left anything there.

use crate::recipes::{Rejection, RunArgs, Validated};
use crate::App;
use cimon_recipe::Resolved;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The longest text upload (a password, a key alias).
pub const TEXT_MAX: usize = 4096;

/// What `${upload.<name>}` expands to in a job.
#[derive(Debug, Clone)]
pub enum Received {
    File(PathBuf),
    Text(String),
}

/// Why `POST /uploads` refused, as the HTTP status and a code the script prints.
#[derive(Debug)]
pub struct UploadError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl UploadError {
    fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        UploadError { status, code, message: message.into() }
    }
}

fn blob(app: &App, handle: &str) -> PathBuf {
    app.layout.uploads().join(handle)
}

fn protect(data: &[u8]) -> std::io::Result<Vec<u8>> {
    #[cfg(windows)]
    return cimon_runner::win::protect(data);
    #[cfg(not(windows))]
    return Ok(data.to_vec());
}

fn unprotect(data: &[u8]) -> std::io::Result<Vec<u8>> {
    #[cfg(windows)]
    return cimon_runner::win::unprotect(data);
    #[cfg(not(windows))]
    return Ok(data.to_vec());
}

/// `POST /uploads`: keeps what a caller sent and returns its handle. `label` is the
/// name the caller gave it (normally the recipe's upload name); the call that uses it
/// decides which upload it is.
pub fn receive(app: &App, user: &str, label: &str, kind: &str, data: &[u8]) -> Result<Value, UploadError> {
    if !cimon_recipe::vars::valid_secret_name(label) {
        return Err(UploadError::new(400, "invalid_label", format!("{label:?} is not an upload name - [a-z0-9][a-z0-9_-]*, as the recipe writes it in [upload.<name>]")));
    }
    let data: Vec<u8> = match kind {
        "file" => {
            if data.len() as u64 > app.config.uploads.max_bytes {
                return Err(UploadError::new(413, "too_large", format!("{} bytes - the limit is {} ([uploads].max_bytes)", data.len(), app.config.uploads.max_bytes)));
            }
            data.to_vec()
        }
        "text" => {
            // one line: what a prompt or an environment variable holds
            let s = std::str::from_utf8(data).map_err(|_| UploadError::new(400, "invalid_text", "a text upload is UTF-8"))?;
            let s = s.strip_suffix('\n').map(|s| s.strip_suffix('\r').unwrap_or(s)).unwrap_or(s);
            if s.len() > TEXT_MAX {
                return Err(UploadError::new(413, "too_large", format!("a text upload is at most {TEXT_MAX} bytes")));
            }
            if s.chars().any(char::is_control) {
                return Err(UploadError::new(400, "invalid_text", "a text upload is one line without control characters"));
            }
            s.as_bytes().to_vec()
        }
        _ => return Err(UploadError::new(400, "invalid_kind", format!("kind must be file or text, got {kind:?}"))),
    };
    if data.is_empty() {
        return Err(UploadError::new(400, "empty", "nothing was sent"));
    }
    let pending = app.store.pending_uploads(user).map_err(|e| UploadError::new(500, "store_error", e.to_string()))?;
    if pending >= app.config.uploads.max_pending_per_user as i64 {
        return Err(UploadError::new(
            429,
            "too_many_uploads",
            format!("you have {pending} uploads no job has taken - use them or let them expire ({} min)", app.config.uploads.ttl_minutes),
        ));
    }
    let handle = format!("u_{}", cimon_core::random_id128());
    let sha = cimon_core::sha256_hex(&data);
    let path = blob(app, &handle);
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(app.layout.uploads())?;
        std::fs::write(&path, protect(&data)?)
    };
    write().map_err(|e| UploadError::new(500, "store_error", format!("could not keep the upload: {e}")))?;
    let expires = cimon_core::after_secs(app.config.uploads.ttl_minutes as i64 * 60);
    if let Err(e) = app.store.create_upload(&handle, user, label, kind, data.len() as i64, &sha, &expires) {
        let _ = std::fs::remove_file(&path);
        return Err(UploadError::new(500, "store_error", e.to_string()));
    }
    let _ = app.store.audit(cimon_store::AuditEntry {
        user,
        action: "upload",
        detail: Some(format!("{handle} {label} {kind} {} bytes sha256 {}", data.len(), &sha[..12])),
        ..Default::default()
    });
    Ok(json!({ "handle": handle, "label": label, "kind": kind, "bytes": data.len(), "sha256": sha, "expires_at": expires }))
}

/// How a caller uploads `names` — what a refusal and `plan` say.
fn how_to(r: &Resolved, names: &[&String]) -> String {
    let specs: Vec<String> = names
        .iter()
        .map(|n| match r.uploads.get(*n).is_some_and(|u| u.is_text()) {
            true => format!("{n}=env:<변수>|prompt"),
            false => format!("{n}=<파일>"),
        })
        .collect();
    format!(
        "요청하는 PC 에서 `powershell -NoProfile -ExecutionPolicy Bypass -File $HOME\\.cimon\\upload.ps1 {0}`(macOS·Linux: `sh ~/.cimon/upload.sh {0}`)로 올리고, 출력된 핸들을 uploads 에 준다 — 파일 내용을 대화에 넣지 않는다",
        specs.join(" ")
    )
}

/// The checks every call makes (`plan` too): the uploads named exist and go to a
/// selected step, and a job that gets uploads runs main's recipe on a branch the
/// uploads allow.
pub fn check_declared(v: &Validated, args: &RunArgs) -> Result<(), Rejection> {
    let r = &v.resolved;
    for name in args.uploads.keys() {
        let Some(u) = r.uploads.get(name) else {
            let known: Vec<&String> = r.uploads.keys().collect();
            return Err(Rejection::new(
                "unknown_upload",
                format!("{} has no upload {name:?} — {}", r.id, if known.is_empty() { "it takes none".to_string() } else { format!("it takes {}", known.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")) }),
            ));
        };
        if !v.plan.uploads.contains(name) {
            return Err(Rejection::new("upload_unused", format!("upload {name} goes to {}, and none of them is selected — select the step or leave the upload out", u.applies_to.join(", "))));
        }
    }
    if v.plan.uploads.is_empty() {
        return Ok(());
    }
    let steps: Vec<&str> = v.plan.steps.iter().map(|s| s.id.as_str()).filter(|s| r.uploads.values().any(|u| u.applies_to.iter().any(|a| a == s))).collect();
    if v.trial {
        return Err(Rejection::new(
            "upload_trial_recipe",
            format!("{} get uploads and run only with the recipe on main — a trial recipe could send what you upload anywhere", steps.join(", ")),
        ));
    }
    let is_sha = v.ref_.len() >= 7 && v.ref_.len() <= 40 && v.ref_.bytes().all(|b| b.is_ascii_hexdigit());
    for name in &v.plan.uploads {
        let refs = r.uploads[name].refs.clone().unwrap_or_else(|| vec![r.project.default_ref.clone()]);
        if is_sha || !cimon_recipe::select::ref_allowed(&refs, &v.ref_) {
            return Err(Rejection::new("upload_ref_not_allowed", format!("upload {name} goes only to jobs on {} — not {:?}{}", refs.join(", "), v.ref_, if is_sha { " (name the branch, not a commit)" } else { "" })));
        }
    }
    Ok(())
}

/// One handle checked for `user`: theirs, waiting, of the upload's kind.
fn waiting(app: &App, user: &str, handle: &str) -> Option<cimon_store::UploadRow> {
    let shaped = handle.len() <= 64 && handle.strip_prefix("u_").is_some_and(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric()));
    if !shaped {
        return None;
    }
    app.store.upload(handle).ok().flatten().filter(|u| u.user == user && u.job_id.is_none() && u.deleted_at.is_none() && u.expires_at > cimon_core::now())
}

/// `create`: every upload the plan needs is handed over as a waiting upload of the
/// caller's, of the declared kind. Returns (name, handle) for the insert to take.
pub fn check_handles(app: &App, user: &str, v: &Validated, args: &RunArgs) -> Result<Vec<(String, String)>, Rejection> {
    let r = &v.resolved;
    let missing: Vec<&String> = v.plan.uploads.iter().filter(|n| !args.uploads.contains_key(*n)).collect();
    if !missing.is_empty() {
        return Err(Rejection::new("upload_missing", format!("이 작업은 업로드 {} 가 필요하다 — {}", missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "), how_to(r, &missing))));
    }
    let mut out = vec![];
    for (name, handle) in &args.uploads {
        let u = &r.uploads[name];
        let Some(row) = waiting(app, user, handle) else {
            return Err(Rejection::new(
                "upload_invalid",
                format!("upload {name}: {handle} is not a waiting upload of yours — it expired ({} min), another job took it, or it never existed; upload again", app.config.uploads.ttl_minutes),
            ));
        };
        if row.kind != u.kind_str() {
            return Err(Rejection::new("upload_invalid", format!("upload {name} is a {} upload; {handle} was sent as {}", u.kind_str(), row.kind)));
        }
        out.push((name.clone(), handle.clone()));
    }
    Ok(out)
}

/// `plan`: each upload the selected steps get, and whether this call hands it over.
pub fn plan_view(app: &App, user: &str, v: &Validated, args: &RunArgs) -> Vec<Value> {
    let r = &v.resolved;
    v.plan
        .uploads
        .iter()
        .map(|name| {
            let u = &r.uploads[name];
            let state = match args.uploads.get(name) {
                None => format!("필요 — {}", how_to(r, &[name])),
                Some(h) => match waiting(app, user, h) {
                    Some(row) if row.kind == u.kind_str() => format!("받음 — {} bytes, sha256 {}…, {} 까지", row.bytes, &row.sha256[..12], row.expires_at),
                    Some(row) => format!("종류가 다르다 — {} 업로드가 필요한데 {h} 는 {}", u.kind_str(), row.kind),
                    None => format!("{h} 는 기다리는 업로드가 아니다(만료·사용됨·없음) — 다시 올린다"),
                },
            };
            let steps: Vec<&String> = u.applies_to.iter().filter(|s| v.plan.runs(s)).collect();
            json!({"name": name, "kind": u.kind_str(), "description": u.description, "steps": steps, "state": state})
        })
        .collect()
}

/// Writes a job's file uploads into its upload folder (removed with the job's homes)
/// and returns what `${upload.<name>}` expands to. Called as the job starts.
pub fn materialize(app: &App, job_id: i64, r: &Resolved) -> Result<BTreeMap<String, Received>, String> {
    let rows = app.store.job_uploads(job_id).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    if rows.is_empty() {
        return Ok(out);
    }
    let dir = app.layout.home_job(job_id, "uploads");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for row in rows {
        let name = row.name.clone().unwrap_or_default();
        let def = r.uploads.get(&name).ok_or_else(|| format!("upload {name} is not in this recipe"))?;
        let data = std::fs::read(blob(app, &row.handle)).and_then(|b| unprotect(&b)).map_err(|e| format!("upload {name} ({}): {e}", row.handle))?;
        if cimon_core::sha256_hex(&data) != row.sha256 {
            return Err(format!("upload {name} ({}) does not match what was received", row.handle));
        }
        let v = if row.kind == "text" {
            Received::Text(String::from_utf8(data).map_err(|_| format!("upload {name} is not UTF-8"))?)
        } else {
            let p = dir.join(def.file.clone().unwrap_or_else(|| name.clone()));
            std::fs::write(&p, &data).map_err(|e| format!("{}: {e}", p.display()))?;
            Received::File(p)
        };
        out.insert(name, v);
    }
    Ok(out)
}

fn delete(app: &App, handle: &str) {
    match std::fs::remove_file(blob(app, handle)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => tracing::warn!("upload {handle} not deleted: {e}"),
        _ => {
            let _ = app.store.mark_upload_deleted(handle);
        }
    }
}

/// Deletes the bytes of a finished job's uploads; the rows stay as the record.
pub fn release_job(app: &App, job_id: i64) {
    for row in app.store.job_uploads(job_id).unwrap_or_default() {
        if row.deleted_at.is_none() {
            delete(app, &row.handle);
        }
    }
}

/// Every minute: uploads nobody took in time and those of finished jobs (a job that
/// ended before its runner started — intake failure, cancel, queue timeout).
pub fn sweep(app: &App) {
    for h in app.store.uploads_to_delete().unwrap_or_default() {
        delete(app, &h);
    }
}

/// The job result's and provenance's record: fingerprints, never the bytes.
pub fn record(app: &App, job_id: i64) -> Option<Value> {
    let rows = app.store.job_uploads(job_id).unwrap_or_default();
    if rows.is_empty() {
        return None;
    }
    let m: serde_json::Map<String, Value> = rows
        .into_iter()
        .map(|u| (u.name.clone().unwrap_or_default(), json!({"kind": u.kind, "bytes": u.bytes, "sha256": u.sha256, "by": u.user})))
        .collect();
    Some(Value::Object(m))
}
