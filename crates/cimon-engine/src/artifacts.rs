//! Artifacts (design §12): pre-clean before the producing step, collection by the
//! service account, inspections, links, provenance.

use crate::run::{JobCtx, StepRec};
use crate::App;
use cimon_core::{after_secs, job_label, now, FailureClass, Outcome, Verdict, Warning};
use cimon_recipe::model::{ArtifactDef, Inspect};
use cimon_store::{ArtifactRow, JobRow};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::path::{Path, PathBuf};

/// Link lifetime (design §12.3).
pub const LINK_SECS: i64 = 3600;
/// Default artifact retention (design §16.4).
pub const RETENTION_DAYS: i64 = 7;

fn requested<'a>(ctx: &'a JobCtx<'_>, step: &'a cimon_recipe::Step) -> Vec<&'a ArtifactDef> {
    step.def.artifact.iter().filter(|a| ctx.plan.plan.artifacts.contains(&a.name)).collect()
}

/// Expands a recipe string the way the step's own command line is expanded, so a glob
/// written with `${ws}` or `${param.x}` means the same thing for pre-clean, collection
/// and the judge. Globs come back relative to the work tree.
fn expand_globs(ctx: &JobCtx<'_>, step: &str, globs: &[String]) -> Result<Vec<String>, Outcome> {
    let out = ctx.app.layout.step_out(ctx.job.id, step);
    let lookup = ctx.lookup(step, &out);
    globs
        .iter()
        .map(|g| {
            cimon_recipe::vars::expand(g, &lookup)
                .map(|e| crate::run::relative_to_ws(&ctx.ws, &e))
                .map_err(|e| Outcome::inconclusive(FailureClass::Recipe, format!("{step}: cannot expand {g:?}: {e}")))
        })
        .collect()
}

fn expand_one(ctx: &JobCtx<'_>, step: &str, s: &str) -> Result<String, Outcome> {
    let out = ctx.app.layout.step_out(ctx.job.id, step);
    let lookup = ctx.lookup(step, &out);
    cimon_recipe::vars::expand(s, &lookup).map_err(|e| Outcome::inconclusive(FailureClass::Recipe, format!("{step}: cannot expand {s:?}: {e}")))
}

fn matches(ws: &Path, globs: &[String], exclude: &[String]) -> Vec<PathBuf> {
    let found = cimon_judge::find_files(ws, globs);
    if exclude.is_empty() {
        return found;
    }
    let ex = cimon_workspace::glob_set(exclude).ok();
    found
        .into_iter()
        .filter(|f| {
            let rel = f.strip_prefix(ws).unwrap_or(f).to_string_lossy().replace('\\', "/");
            let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            !ex.as_ref().is_some_and(|e| e.is_match(&rel) || e.is_match(&name))
        })
        .collect()
}

/// Deletes files the producing step must recreate. Returns log preamble lines. A file
/// that cannot be deleted stops the step: an old copy left in place would be collected
/// (or read as a report) as if this run had made it.
pub fn pre_clean(ctx: &JobCtx<'_>, step: &cimon_recipe::Step) -> Result<Vec<String>, Outcome> {
    let mut globs: Vec<(String, Vec<String>, Vec<String>)> = vec![];
    for a in requested(ctx, step) {
        globs.push((format!("artifact {}", a.name), expand_globs(ctx, step.id(), &a.glob.to_vec())?, expand_globs(ctx, step.id(), &a.exclude)?));
    }
    if let Some(j) = &step.def.judge {
        if j.type_ == "junit" {
            globs.push(("junit reports".into(), expand_globs(ctx, step.id(), &j.reports.as_ref().map(|r| r.to_vec()).unwrap_or_default())?, vec![]));
        }
    }
    if globs.is_empty() {
        return Ok(vec![]);
    }
    let tracked: std::collections::BTreeSet<String> = cimon_workspace::WorkTree { dir: ctx.ws.clone() }.tracked().unwrap_or_default().into_iter().collect();
    let mut lines = vec![];
    for (what, g, ex) in globs {
        let files = matches(&ctx.ws, &g, &ex);
        let mut removed = vec![];
        for f in files {
            let rel = f.strip_prefix(&ctx.ws).unwrap_or(&f).to_string_lossy().replace('\\', "/");
            if tracked.iter().any(|t| t.eq_ignore_ascii_case(&rel)) {
                return Err(Outcome::inconclusive(FailureClass::Recipe, format!("{what}: glob matches the tracked file {rel} — pre-clean never deletes tracked files; narrow the glob")));
            }
            match std::fs::remove_file(&f) {
                Ok(()) => removed.push(rel),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Outcome::inconclusive(FailureClass::Infra, format!("{what}: pre-clean could not delete {rel}: {e} — an old copy would be taken for this run's output"))),
            }
        }
        if !removed.is_empty() {
            lines.push(format!("pre-clean ({what}): removed {} file(s): {}", removed.len(), removed.iter().take(20).cloned().collect::<Vec<_>>().join(", ")));
        }
    }
    Ok(lines)
}

/// Collects the requested artifacts of a step that passed. A missing or extra file
/// fails the step (`fail`/`code`); a failing `required` inspection makes it
/// `inconclusive`/`recipe`.
pub fn collect(ctx: &mut JobCtx<'_>, step: &cimon_recipe::Step) -> Result<(), Outcome> {
    let app = ctx.app;
    let id = ctx.job.id;
    let dest_dir = app.layout.job_artifacts(id);
    let retention = ctx.resolved.project.retention_days.map(|d| d as i64).unwrap_or(RETENTION_DAYS);
    let defs: Vec<ArtifactDef> = requested(ctx, step).into_iter().cloned().collect();
    for a in defs {
        let _ = app.store.set_status(id, &format!("산출물 수집: {}", a.name));
        let globs = expand_globs(ctx, step.id(), &a.glob.to_vec())?;
        let exclude = expand_globs(ctx, step.id(), &a.exclude)?;
        let files = matches(&ctx.ws, &globs, &exclude);
        let n = files.len() as u32;
        let ok = match (a.expect, a.expect_min) {
            (Some(e), _) => n == e,
            (None, Some(m)) => n >= m,
            _ => n >= 1,
        };
        if !ok {
            let want = a.expect.map(|e| format!("exactly {e}")).or(a.expect_min.map(|m| format!("at least {m}"))).unwrap_or_default();
            let listed: Vec<String> = files.iter().take(5).map(|f| f.strip_prefix(&ctx.ws).unwrap_or(f).to_string_lossy().into_owned()).collect();
            return Err(Outcome::fail(
                FailureClass::Code,
                format!("artifact {}: expected {want} file(s) for {}, found {n}{}", a.name, globs.join(", "), if listed.is_empty() { String::new() } else { format!(" ({})", listed.join(", ")) }),
            ));
        }
        std::fs::create_dir_all(&dest_dir).map_err(|e| Outcome::inconclusive(FailureClass::Infra, e.to_string()))?;
        let file_name = match &a.pack_name {
            Some(p) => expand_one(ctx, step.id(), p)?,
            None => a.name.clone(),
        };
        let dest = dest_dir.join(&a.name);
        let res = if a.pack.as_deref() == Some("zip") {
            pack_zip(&ctx.ws, &globs, &files, &dest)
        } else {
            copy_within(&ctx.ws, &files[0], &dest)
        };
        res.map_err(|e| Outcome::inconclusive(FailureClass::Infra, format!("artifact {}: copy failed: {e}", a.name)))?;
        let data_len = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        let sha = sha256_file(&dest).map_err(|e| Outcome::inconclusive(FailureClass::Infra, e))?;
        // inspections
        let mut inspect = serde_json::Map::new();
        for item in &a.inspect {
            let Ok(i) = item.normalize() else { continue };
            let (name, required, result) = match &i {
                Inspect::Builtin { name, required } => (name.clone(), *required, cimon_judge::inspect::run_builtin(name, &dest)),
                Inspect::Script { run, required } => ("script".to_string(), *required, run_inspect_script(ctx, run, &dest)),
            };
            match result {
                Ok(v) => {
                    inspect.insert(name, v);
                }
                Err(e) => {
                    if required {
                        return Err(Outcome::inconclusive(FailureClass::Recipe, format!("artifact {}: required inspection {name} failed: {e}", a.name)));
                    }
                    inspect.insert(format!("{name}_error"), json!(e));
                    ctx.warnings.push(Warning::new("inspect", format!("{}: {name} 검사 실패 — {e}", a.name)));
                }
            }
        }
        let row = ArtifactRow {
            job_id: id,
            name: a.name.clone(),
            path: dest.to_string_lossy().into(),
            bytes: data_len as i64,
            sha256: sha,
            inspect_json: Some(json!({ "inspect": inspect, "file_name": file_name, "description": a.description }).to_string()),
            created_at: now(),
            expires_at: after_secs(retention * 86400),
            deleted_at: None,
        };
        app.store.insert_artifact(&row).map_err(|e| Outcome::inconclusive(FailureClass::Infra, e.to_string()))?;
        ctx.artifacts.insert(a.name.clone(), dest);
    }
    Ok(())
}

fn run_inspect_script(ctx: &JobCtx<'_>, run: &[String], file: &Path) -> Result<Value, String> {
    let out_dir = ctx.app.layout.step_out(ctx.job.id, "_inspect");
    let _ = std::fs::create_dir_all(&out_dir);
    let lookup = ctx.lookup("_inspect", &out_dir);
    let argv: Vec<String> = run.iter().map(|a| cimon_recipe::vars::expand(a, &lookup)).collect::<Result<_, _>>()?;
    let mut env = cimon_runner::base_env(ctx.profile.as_deref(), ctx.build_user.as_deref());
    let mut path = ctx.tool_env.path.clone();
    path.extend(cimon_runner::system_path());
    env.insert("PATH".into(), cimon_runner::join_path(&path));
    for (k, v) in &ctx.tool_env.exports {
        env.insert(k.clone(), v.clone());
    }
    env.insert("CIMON_ARTIFACT".into(), file.to_string_lossy().into());
    let tmp = ctx.app.layout.tmp_job(ctx.job.id);
    let _ = std::fs::create_dir_all(&tmp);
    env.insert("TEMP".into(), tmp.to_string_lossy().into());
    env.insert("TMP".into(), tmp.to_string_lossy().into());
    let dirs: Vec<PathBuf> = path;
    let program = cimon_runner::resolve_program(&argv[0], &ctx.ws, &dirs, env.get("PATHEXT").map(|s| s.as_str()).unwrap_or(".EXE;.BAT;.CMD"))?;
    let spec = cimon_runner::ExecSpec {
        program,
        args: argv[1..].to_vec(),
        cwd: ctx.ws.clone(),
        env,
        timeout: std::time::Duration::from_secs(300),
        log_path: out_dir.join("inspect.txt"),
        log_cap: 10 << 20,
        error_patterns: vec![],
        fail_on_output: vec![],
        capture_prefixes: vec!["{".into()],
        preamble: vec![],
    };
    let r = cimon_runner::execute(&spec, &ctx.account, ctx.cancel);
    if r.exit_code != Some(0) {
        return Err(format!("exited with {:?}", r.exit_code));
    }
    r.captured.iter().rev().find_map(|l| serde_json::from_str::<Value>(l).ok()).ok_or_else(|| "no JSON on stdout".to_string())
}

fn pack_zip(ws: &Path, globs: &[String], files: &[PathBuf], dest: &Path) -> Result<(), String> {
    use std::io::Write;
    let base = common_base(ws, globs);
    let f = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut z = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true);
    for file in files {
        let rel = file.strip_prefix(&base).or_else(|_| file.strip_prefix(ws)).unwrap_or(file).to_string_lossy().replace('\\', "/");
        let mut src = cimon_runner::open_within(ws, file).map_err(|e| e.to_string())?;
        z.start_file(rel, opts).map_err(|e| e.to_string())?;
        std::io::copy(&mut src, &mut z).map_err(|e| e.to_string())?;
    }
    z.finish().map_err(|e| e.to_string())?.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// Copies one file out of the work tree, refusing anything that leads outside it.
fn copy_within(ws: &Path, src: &Path, dest: &Path) -> Result<(), String> {
    let mut from = cimon_runner::open_within(ws, src).map_err(|e| e.to_string())?;
    let mut to = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    std::io::copy(&mut from, &mut to).map_err(|e| e.to_string())?;
    to.sync_all().map_err(|e| e.to_string())
}

/// The directory before the first wildcard of the first glob.
fn common_base(ws: &Path, globs: &[String]) -> PathBuf {
    let Some(g) = globs.first() else { return ws.to_path_buf() };
    let mut base = ws.to_path_buf();
    for comp in g.replace('\\', "/").split('/') {
        if comp.contains(['*', '?', '[', '{']) {
            break;
        }
        base.push(comp);
    }
    if base.is_file() {
        base.pop();
    }
    base
}

pub fn sha256_file(p: &Path) -> Result<String, String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// HMAC link token over (job, name, expiry) — design §12.3.
pub fn link_token(key: &[u8], job: i64, name: &str, expires: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac key");
    mac.update(format!("{job}\n{name}\n{expires}").as_bytes());
    format!("{expires}.{}", hex::encode(mac.finalize().into_bytes()))
}

pub fn verify_token(key: &[u8], job: i64, name: &str, token: &str) -> Result<(), &'static str> {
    let (exp, sig) = token.split_once('.').ok_or("malformed token")?;
    let expires: i64 = exp.parse().map_err(|_| "malformed token")?;
    if chrono::Utc::now().timestamp() > expires {
        return Err("link expired — ask for a new one with the artifacts tool");
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac key");
    mac.update(format!("{job}\n{name}\n{expires}").as_bytes());
    let want = hex::decode(sig).map_err(|_| "malformed token")?;
    mac.verify_slice(&want).map_err(|_| "invalid token")
}

/// The link key, created on first use if `cimon setup` has not made one.
pub fn link_key(app: &App) -> Vec<u8> {
    let p = app.layout.link_key();
    if let Ok(k) = std::fs::read(&p) {
        if k.len() >= 32 {
            return k;
        }
    }
    let key = cimon_core::random_secret_hex().into_bytes();
    let _ = std::fs::create_dir_all(app.layout.config_dir());
    let _ = std::fs::write(&p, &key);
    key
}

/// Artifact list with fresh links.
pub fn listing(app: &App, job: &JobRow) -> Vec<Value> {
    let rows = app.store.artifacts(job.id).unwrap_or_default();
    if rows.is_empty() {
        return vec![];
    }
    let key = link_key(app);
    let expires = chrono::Utc::now().timestamp() + LINK_SECS;
    let base = app.config.server.public_url.trim_end_matches('/');
    rows.into_iter()
        .map(|a| {
            let meta: Value = a.inspect_json.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_else(|| json!({}));
            let mut v = json!({
                "name": a.name,
                "file_name": meta.get("file_name").cloned().unwrap_or(json!(a.name)),
                "bytes": a.bytes,
                "sha256": a.sha256,
                "inspect": meta.get("inspect").cloned().unwrap_or(json!({})),
                "retained_until": a.expires_at,
            });
            if a.deleted_at.is_some() {
                v["expired"] = json!(format!("{} 의 산출물은 {} 에 만료돼 지웠다", job_label(job.id), a.deleted_at.unwrap_or_default()));
            } else {
                let token = link_token(&key, job.id, &a.name, expires);
                v["url"] = json!(format!("{base}/artifacts/{}/{}?t={}", job_label(job.id), urlencode(&a.name), token));
                v["url_expires"] = json!(chrono::DateTime::from_timestamp(expires, 0).map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)));
            }
            v
        })
        .collect()
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// `provenance.json` (design §12.4).
pub fn write_provenance(ctx: &JobCtx<'_>, recs: &[StepRec], verdict: Verdict) {
    let app = ctx.app;
    let job = app.store.require_job(ctx.job.id).unwrap_or_else(|_| ctx.job.clone());
    let arts: Vec<Value> = app.store.artifacts(job.id).unwrap_or_default().into_iter().map(|a| json!({"name": a.name, "sha256": a.sha256, "bytes": a.bytes, "inspect": a.inspect_json.and_then(|s| serde_json::from_str::<Value>(&s).ok())})).collect();
    let v = json!({
        "job": job_label(job.id),
        "project": job.project,
        "ref": job.ref_,
        "commit": job.commit_sha,
        "siblings": ctx.plan.siblings,
        "recipe": {"source": job.recipe_source, "ref": job.recipe_ref, "commit": job.recipe_commit, "sha256": job.recipe_sha256},
        "params": ctx.plan.plan.params,
        "toolchains": app.store.job_toolchains(job.id).unwrap_or_default().into_iter().map(|(k, v, h, p)| json!({"kind": k, "version": v, "entry_sha256": h, "probe": p})).collect::<Vec<_>>(),
        "workspace": {"mode": job.workspace_mode, "slot": job.workspace_slot},
        "steps": recs.iter().map(|r| json!({"id": r.id, "result": r.result, "seconds": r.seconds})).collect::<Vec<_>>(),
        "artifacts": arts,
        "warnings": ctx.warnings,
        "new_executables": crate::toolchains::new_executables(ctx),
        "runner": app.config.runner.name,
        "trial": job.trial,
        "verdict": verdict.as_str(),
        "signed": false,
        "created_at": now(),
    });
    let dir = app.layout.job_artifacts(job.id);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("provenance.json"), serde_json::to_vec_pretty(&v).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let key = b"0123456789abcdef0123456789abcdef";
        let exp = chrono::Utc::now().timestamp() + 60;
        let t = link_token(key, 7, "a.apk", exp);
        assert!(verify_token(key, 7, "a.apk", &t).is_ok());
        assert!(verify_token(key, 8, "a.apk", &t).is_err());
        assert!(verify_token(key, 7, "b.apk", &t).is_err());
        let old = link_token(key, 7, "a.apk", chrono::Utc::now().timestamp() - 1);
        assert!(verify_token(key, 7, "a.apk", &old).is_err());
        assert!(verify_token(key, 7, "a.apk", "garbage").is_err());
    }
}
