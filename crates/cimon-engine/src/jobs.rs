//! Job lifecycle (design §8): create (validated, committed, then answered) → intake
//! (fetch, pin commit, intake checks, plan, merge) → provisioning → ready → runner.

use crate::recipes::{self, Rejection, RunArgs, Validated};
use crate::App;
use cimon_core::{after_secs, job_label, now, FailureClass, JobState, Tool, Verdict};
use cimon_recipe::model::ProjectFile;
use cimon_recipe::select::Plan;
use cimon_store::NewJob;
use cimon_workspace::{Mirror, ResolveError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Task / job record retention (design §16.4): 30 days, also the task `ttlMs`.
pub const RECORD_DAYS: i64 = 30;
pub const TTL_MS: u64 = RECORD_DAYS as u64 * 24 * 3600 * 1000;
pub const POLL_INTERVAL_MS: u64 = 5000;

/// What the job store keeps in `request_json`: the call as made plus the project
/// snapshot validation used (main's `project.toml` for registered projects).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    pub args: RunArgs,
    pub project: ProjectFile,
}

/// What intake pins in `plan_json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JobPlan {
    pub plan: Plan,
    /// kind → exact version (from the recipe or the repository's version file)
    pub versions: BTreeMap<String, String>,
    pub siblings: Vec<SiblingPin>,
    pub repo: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SiblingPin {
    pub project: String,
    pub project_key: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    pub commit: String,
    pub repo: String,
    pub env: Vec<String>,
    pub place: String,
}

pub struct CreatedTask {
    pub task_id: String,
    pub job_id: i64,
    pub created_at: String,
    pub status: String,
}

/// Validates and records a job; the task id is returned only after the commit.
pub fn create(app: &Arc<App>, user: &str, tool: Tool, args: RunArgs, via_task: bool) -> Result<CreatedTask, Rejection> {
    if app.layout.draining_flag().exists() {
        return Err(Rejection::new("draining", "cimon is draining for an upgrade — no new jobs until it restarts"));
    }
    if let Some(free) = cimon_runner::free_bytes(&app.layout.root) {
        let min = app.config.disk.min_free_gb * 1024 * 1024 * 1024;
        if free < min {
            crate::janitor::free_space(app);
            let free = cimon_runner::free_bytes(&app.layout.root).unwrap_or(free);
            if free < min {
                return Err(Rejection::new(
                    "disk_low",
                    format!("only {} GB free on the build machine (min {}) even after cleanup — an admin must free space", free / (1 << 30), app.config.disk.min_free_gb),
                ));
            }
        }
    }
    let queued = app.store.count_pre_start_by_user(user)?;
    if queued >= app.config.queue.max_queued_per_user as i64 {
        let mine: Vec<String> = app
            .store
            .jobs_in_states(&[JobState::Intake, JobState::Provisioning, JobState::Ready])?
            .into_iter()
            .filter(|j| j.created_by == user)
            .map(|j| job_label(j.id))
            .collect();
        return Err(Rejection::new("queue_full", format!("you already have {queued} jobs waiting ({}) — wait or cancel_job", mine.join(", "))));
    }
    let v: Validated = recipes::validate(app, tool, &args)?;
    let request = JobRequest { args: args.clone(), project: v.resolved.project.clone() };
    let task_id = format!("t_{}", cimon_core::random_id128());
    let new = NewJob {
        tool: tool.as_str().into(),
        project: args.project.clone(),
        project_key: v.project_key.clone(),
        ref_: Some(v.ref_.clone()),
        recipe_source: "central".into(),
        recipe_ref: v.recipe_ref.clone(),
        recipe_commit: v.recipe_commit.clone(),
        recipe_sha256: v.recipe_sha256.clone(),
        trial: v.trial,
        request_json: serde_json::to_string(&request).unwrap(),
        created_by: user.to_string(),
        status_message: "접수: 대기".into(),
        expires_at: after_secs(RECORD_DAYS * 86400),
    };
    let id = app.store.insert_job_with_task(&new, &task_id, via_task, None)?;
    app.engine.intake.notify_one();
    let job = app.store.require_job(id)?;
    Ok(CreatedTask { task_id, job_id: id, created_at: job.created_at, status: "접수: 대기".into() })
}

/// `cancel_job`: before the runner → cancelled now; running → `cancelling`, the runner
/// tears down and finishes it.
pub fn cancel(app: &App, job_id: i64, by: &str) -> Result<String, Rejection> {
    let job = app.store.job(job_id)?.ok_or_else(|| Rejection::new("unknown_job", format!("no job {}", job_label(job_id))))?;
    if job.state.is_terminal() {
        return Ok(format!("{} already finished ({}) — nothing to cancel", job_label(job_id), job.state.as_str()));
    }
    app.store.request_cancel(job_id, by)?;
    let _ = app.store.audit(cimon_store::AuditEntry { user: by, action: "cancel", job_id: Some(job_id), ..Default::default() });
    if job.state.is_pre_start() {
        finish_simple(app, job_id, JobState::Cancelled, None, None, None, &format!("{by} 가 취소함"));
        return Ok(format!("{} cancelled", job_label(job_id)));
    }
    app.store.set_state(job_id, JobState::Cancelling, &format!("취소 중 — 정리하는 중({by} 요청)"))?;
    if let Some(flag) = app.engine.running.lock().unwrap().get(&job_id) {
        flag.store(true, Ordering::SeqCst);
    }
    Ok(format!("cancellation requested — job_status shows cancelled once {} has cleaned up", job_label(job_id)))
}

/// `tasks/cancel`: detaches one task; the job is cancelled only when none remain.
pub fn detach_task(app: &App, task_id: &str, by: &str) -> Result<(), Rejection> {
    let t = app.store.task(task_id)?.ok_or_else(|| Rejection::new("unknown_task", format!("unknown task {task_id}")))?;
    let remaining = app.store.detach_task(task_id, &format!("tasks/cancel by {by}"))?;
    if remaining == 0 {
        let job = app.store.require_job(t.job_id)?;
        if !job.state.is_terminal() {
            app.store.request_cancel(t.job_id, by)?;
            if job.state.is_pre_start() {
                finish_simple(app, t.job_id, JobState::Cancelled, None, None, None, "요청한 task 가 모두 떨어짐");
            } else {
                app.store.set_state(t.job_id, JobState::Cancelling, "취소 중 — 요청한 task 가 모두 떨어짐")?;
                if let Some(flag) = app.engine.running.lock().unwrap().get(&t.job_id) {
                    flag.store(true, Ordering::SeqCst);
                }
            }
        }
    }
    Ok(())
}

/// Ends a job that never reached the runner, with a minimal result.
pub fn finish_simple(app: &App, job_id: i64, state: JobState, verdict: Option<Verdict>, class: Option<FailureClass>, reason_code: Option<&str>, message: &str) {
    let Ok(Some(job)) = app.store.job(job_id) else { return };
    let summary = match verdict {
        Some(v) => format!(
            "{} {} {} — {}{}",
            job.project,
            job.ref_.clone().unwrap_or_default(),
            v.as_str(),
            class.map(|c| format!("[{}] ", c.as_str())).unwrap_or_default(),
            message
        ),
        None => format!("{} {} — {message}", job_label(job_id), state.as_str()),
    };
    let result = crate::result::build(app, &job, verdict, class, &summary, &[], reason_code);
    let _ = app.store.finish_job(
        job_id,
        state,
        verdict.map(|v| v.as_str()),
        class.map(|c| c.as_str()),
        reason_code,
        message,
        &summary,
        "[]",
        &result.to_string(),
    );
}

fn fail_intake(app: &App, job_id: i64, verdict: Verdict, class: FailureClass, code: &str, message: &str) {
    finish_simple(app, job_id, JobState::Done, Some(verdict), Some(class), Some(code), &format!("{code}: {message}"));
}

/// The intake worker: one at a time (design §8.1).
pub async fn intake_loop(app: Arc<App>) {
    loop {
        let next = app.store.oldest_in_state(JobState::Intake).ok().flatten();
        match next {
            Some(job) => {
                let a = app.clone();
                let id = job.id;
                let res = tokio::task::spawn_blocking(move || intake_one(&a, id)).await;
                if let Err(e) = res {
                    tracing::error!("intake of {} panicked: {e}", job_label(id));
                    fail_intake(&app, id, Verdict::Inconclusive, FailureClass::Infra, "internal_error", "intake crashed — see the service log");
                }
                app.engine.runner.notify_one();
            }
            None => {
                tokio::select! {
                    _ = app.engine.intake.notified() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                    _ = app.engine.shutdown.cancelled() => return,
                }
            }
        }
        if app.engine.shutdown.is_cancelled() {
            return;
        }
    }
}

fn still_wanted(app: &App, id: i64) -> bool {
    app.store.job(id).ok().flatten().is_some_and(|j| j.cancel_requested_by.is_none() && j.state.is_pre_start())
}

/// Fetch, pin, intake checks, plan, merge — then provisioning and ready.
pub fn intake_one(app: &Arc<App>, id: i64) {
    let Ok(job) = app.store.require_job(id) else { return };
    let request: JobRequest = match serde_json::from_str(&job.request_json) {
        Ok(r) => r,
        Err(e) => return fail_intake(app, id, Verdict::Inconclusive, FailureClass::Infra, "internal_error", &format!("bad request record: {e}")),
    };
    let tool = Tool::parse(&job.tool).unwrap_or(Tool::Run);
    let _ = app.store.set_status(id, "접수: git fetch");

    // recipe pinned at call time
    let resolved = match crate::run::resolve_pinned(app, &job, &request) {
        Ok(r) => r,
        Err(e) => return fail_intake(app, id, Verdict::Fail, FailureClass::Recipe, "recipe_invalid", &e),
    };
    let project = request.project.clone();
    let mirror_dir = app.layout.mirror(&job.project_key);
    let auth = crate::git_auth(app);
    let mirror = match Mirror::fetch_with(&mirror_dir, &project.repo, &auth) {
        Ok(m) => m,
        Err(e) => return fail_intake(app, id, Verdict::Inconclusive, FailureClass::Infra, "fetch_failed", &format!("git fetch {}: {e}", project.repo)),
    };
    if !still_wanted(app, id) {
        return;
    }
    let ref_ = job.ref_.clone().unwrap_or_else(|| project.default_ref.clone());
    let allowed = |b: &str| cimon_recipe::select::ref_allowed(&project.allowed_refs, b);
    let commit = match mirror.resolve(&ref_, allowed) {
        Ok((c, _)) => c,
        Err(ResolveError::RefNotFound(m)) => return fail_intake(app, id, Verdict::Fail, FailureClass::Request, "ref_not_found", &m),
        Err(ResolveError::CommitNotAllowed(m)) => return fail_intake(app, id, Verdict::Fail, FailureClass::Request, "commit_not_allowed", &m),
    };

    // siblings (from main's registration)
    let mut siblings = vec![];
    for s in &project.sibling {
        let _ = app.store.set_status(id, &format!("접수: 형제 {} fetch", s.project));
        let main = match app.recipes.require_main().and_then(|m| app.recipes.tree(&m)) {
            Ok(t) => t,
            Err(e) => return fail_intake(app, id, Verdict::Inconclusive, FailureClass::Infra, "recipes_unavailable", &e.to_string()),
        };
        let sp = match cimon_recipe::resolve::parse_project(&*main, &s.project) {
            Ok(p) => p,
            Err(d) => return fail_intake(app, id, Verdict::Fail, FailureClass::Recipe, "sibling_invalid", &d.to_string()),
        };
        let sm = match Mirror::fetch_with(&app.layout.mirror(&s.project), &sp.repo, &auth) {
            Ok(m) => m,
            Err(e) => return fail_intake(app, id, Verdict::Inconclusive, FailureClass::Infra, "fetch_failed", &format!("sibling {}: {e}", s.project)),
        };
        let sallowed = |b: &str| cimon_recipe::select::ref_allowed(&sp.allowed_refs, b) || b == s.ref_;
        let sc = match sm.resolve(&s.ref_, sallowed) {
            Ok((c, _)) => c,
            Err(ResolveError::RefNotFound(m)) | Err(ResolveError::CommitNotAllowed(m)) => {
                return fail_intake(app, id, Verdict::Fail, FailureClass::Recipe, "sibling_ref_not_found", &format!("sibling {}: {m}", s.project))
            }
        };
        siblings.push(SiblingPin {
            project: s.project.clone(),
            project_key: s.project.clone(),
            ref_: s.ref_.clone(),
            commit: sc,
            repo: sp.repo.clone(),
            env: s.env.to_vec(),
            place: s.place.clone(),
        });
    }

    let _ = app.store.set_status(id, "접수: 검사");
    // intake checks (recipe-spec §15.2)
    let plan = match cimon_recipe::select::select(
        &resolved,
        tool,
        &cimon_recipe::select::SelectRequest {
            steps: request.args.steps.clone(),
            tags: request.args.tags.clone(),
            artifacts: request.args.artifacts.clone(),
            params: request.args.params.clone(),
            capabilities: app.config.runner.capabilities.clone(),
        },
    ) {
        Ok(p) => p,
        Err(e) => return fail_intake(app, id, Verdict::Fail, FailureClass::Recipe, e.code, &e.message),
    };
    let tracked = match mirror.tracked(&commit) {
        Ok(t) => t,
        Err(e) => return fail_intake(app, id, Verdict::Inconclusive, FailureClass::Infra, "git_error", &e.to_string()),
    };
    let mut versions = BTreeMap::new();
    for kind in &plan.toolchains {
        let Some(req) = resolved.toolchains.get(kind) else { continue };
        if let Some(v) = &req.version {
            versions.insert(kind.clone(), v.clone());
        } else if let Some(f) = &req.version_file {
            match read_version_file(&mirror, &commit, f, &tracked) {
                Ok(v) => {
                    versions.insert(kind.clone(), v);
                }
                Err((code, msg)) => return fail_intake(app, id, Verdict::Fail, FailureClass::Recipe, code, &msg),
            }
        }
    }
    if let Some(msg) = tracked_glob_violation(&resolved, &plan, &tracked) {
        return fail_intake(app, id, Verdict::Fail, FailureClass::Recipe, "glob_matches_tracked", &msg);
    }

    let job_plan = JobPlan { plan: plan.clone(), versions, siblings: siblings.clone(), repo: project.repo.clone() };
    let merge_key = merge_key(&job, &commit, &siblings, &plan, request.args.clean, &request.args);
    let _ = app.store.set_intake(id, &commit, &serde_json::to_string(&siblings).unwrap(), &serde_json::to_string(&job_plan).unwrap(), &merge_key);
    let _ = app.store.set_excluded(id, &plan.excluded);
    if !still_wanted(app, id) {
        return;
    }
    if let Ok(Some(target)) = app.store.merge_target(&merge_key, id) {
        let _ = app.store.merge_job(id, target, &format!("{} 과 같은 요청 — 그 작업을 따른다", job_label(target)));
        return;
    }

    // provisioning (design §10.4)
    let _ = app.store.set_state(id, JobState::Provisioning, "툴체인 확인");
    match crate::toolchains::provide(app, id, &job, &resolved, &job_plan) {
        Ok(()) => {}
        Err(p) => return fail_intake(app, id, p.verdict, p.class, p.code, &p.message),
    }
    if !still_wanted(app, id) {
        return;
    }
    // a duplicate may have become ready while this one provisioned
    if let Ok(Some(target)) = app.store.merge_target(&merge_key, id) {
        let _ = app.store.merge_job(id, target, &format!("{} 과 같은 요청 — 그 작업을 따른다", job_label(target)));
        return;
    }
    let _ = app.store.set_state(id, JobState::Ready, "대기열");
    refresh_queue_positions(app);
}

/// Merge key (design §8.3): everything that changes the result.
fn merge_key(job: &cimon_store::JobRow, commit: &str, siblings: &[SiblingPin], plan: &Plan, clean: bool, args: &RunArgs) -> String {
    let mut steps = args.steps.clone();
    steps.sort();
    let mut tags = args.tags.clone();
    tags.sort();
    let mut artifacts = args.artifacts.clone();
    if let Some(a) = artifacts.as_mut() {
        a.sort();
    }
    let v = json!({
        "project": job.project_key,
        "commit": commit,
        "siblings": siblings.iter().map(|s| (&s.project, &s.commit)).collect::<Vec<_>>(),
        "recipe_commit": job.recipe_commit,
        "recipe_sha256": job.recipe_sha256,
        "tool": job.tool,
        "steps": steps,
        "tags": tags,
        "artifacts": artifacts,
        "params": plan.params,
        "clean": clean,
    });
    cimon_core::sha256_hex(v.to_string().as_bytes())
}

/// `version_file` (recipe-spec §5): read at the pinned commit; every other
/// `rust-toolchain.toml` in the repository must agree.
fn read_version_file(mirror: &Mirror, commit: &str, path: &str, tracked: &[String]) -> Result<String, (&'static str, String)> {
    let Some(bytes) = mirror.show(commit, path) else {
        return Err(("version_file_missing", format!("{path} does not exist at {}", &commit[..10])));
    };
    let v = parse_toolchain_channel(&bytes).map_err(|e| ("version_file_missing", format!("{path}: {e}")))?;
    let exact = regex::Regex::new(cimon_recipe::check::EXACT_VERSION).unwrap();
    if !exact.is_match(&v) {
        return Err(("version_file_mismatch", format!("{path}: channel {v:?} is not an exact version")));
    }
    for other in tracked {
        let name = other.rsplit('/').next().unwrap_or(other);
        if other != path && (name == "rust-toolchain.toml" || name == "rust-toolchain") {
            if let Some(b) = mirror.show(commit, other) {
                if let Ok(ov) = parse_toolchain_channel(&b) {
                    if ov != v {
                        return Err(("version_file_mismatch", format!("rust-toolchain files disagree: {path} = {v}, {other} = {ov}")));
                    }
                }
            }
        }
    }
    Ok(v)
}

/// The channel of a `rust-toolchain.toml` / `rust-toolchain` file.
pub fn channel_of(bytes: &[u8]) -> Result<String, String> {
    parse_toolchain_channel(bytes)
}

fn parse_toolchain_channel(bytes: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(bytes);
    if text.lines().any(|l| l.trim_start().starts_with('[')) {
        return toml_like_channel(&text);
    }
    // legacy `rust-toolchain` file: just the channel
    let line = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).ok_or("empty file")?;
    Ok(line.to_string())
}

fn toml_like_channel(text: &str) -> Result<String, String> {
    let v: toml_value::Table = text.parse().map_err(|e: toml_value::TomlErr| e.0)?;
    v.channel.ok_or_else(|| "no toolchain.channel".into())
}

/// Minimal TOML read of `[toolchain] channel` without pulling the full recipe model.
mod toml_value {
    pub struct TomlErr(pub String);
    pub struct Table {
        pub channel: Option<String>,
    }
    impl std::str::FromStr for Table {
        type Err = TomlErr;
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            let mut in_tc = false;
            let mut saw_table = false;
            for line in s.lines() {
                let l = line.split('#').next().unwrap_or("").trim();
                if l.starts_with('[') {
                    saw_table = true;
                    in_tc = l == "[toolchain]";
                    continue;
                }
                if in_tc {
                    if let Some(rest) = l.strip_prefix("channel") {
                        let v = rest.trim_start().strip_prefix('=').ok_or_else(|| TomlErr("bad channel line".into()))?.trim();
                        return Ok(Table { channel: Some(v.trim_matches('"').trim_matches('\'').to_string()) });
                    }
                }
            }
            if saw_table {
                Ok(Table { channel: None })
            } else {
                Err(TomlErr("not a TOML toolchain file".into()))
            }
        }
    }
}

/// Requested artifact globs and junit report globs must not match tracked files
/// (pre-clean would delete them — design §12.2).
fn tracked_glob_violation(r: &cimon_recipe::Resolved, plan: &Plan, tracked: &[String]) -> Option<String> {
    let vars = |s: &str| cimon_recipe::vars::expand(s, &|n| n.strip_prefix("vars.").and_then(|k| r.vars.get(k).cloned())).ok();
    let mut globs: Vec<(String, String)> = vec![];
    for a in &plan.artifacts {
        if let Some((_, def)) = r.artifact(a) {
            for g in def.glob.to_vec() {
                if let Some(g) = vars(&g) {
                    globs.push((format!("artifact {a}"), g));
                }
            }
        }
    }
    for ps in &plan.steps {
        if let Some(j) = r.step(&ps.id).and_then(|s| s.def.judge.as_ref()) {
            if j.type_ == "junit" {
                for g in j.reports.as_ref().map(|x| x.to_vec()).unwrap_or_default() {
                    if let Some(g) = vars(&g) {
                        globs.push((format!("step {} junit reports", ps.id), g));
                    }
                }
            }
        }
    }
    for (what, g) in globs {
        if g.contains("${") {
            continue;
        }
        let Ok(set) = cimon_workspace::glob_set(&[g.clone()]) else { continue };
        if let Some(hit) = tracked.iter().find(|t| set.is_match(t)) {
            return Some(format!("{what}: glob {g:?} matches the tracked file {hit} — pre-clean would delete it; narrow the glob"));
        }
    }
    None
}

/// Rewrites "대기열 N번째" messages after the queue changes.
pub fn refresh_queue_positions(app: &App) {
    if let Ok(ready) = app.store.jobs_in_states(&[JobState::Ready]) {
        let running = app.store.jobs_in_states(&[JobState::Running, JobState::Collecting, JobState::Cancelling]).unwrap_or_default();
        let ahead = running.first().map(|j| format!(" — 실행 중: {} {} {}", job_label(j.id), j.project, j.tool)).unwrap_or_default();
        for (i, j) in ready.iter().enumerate() {
            let _ = app.store.set_status(j.id, &format!("대기열 {}번째{}", i + 1, if i == 0 { ahead.clone() } else { String::new() }));
        }
    }
}

/// Periodic housekeeping: queue timeouts (design §8.3).
pub fn tick(app: &App) {
    let limit = app.config.queue.max_queue_hours as i64 * 3600;
    if let Ok(jobs) = app.store.jobs_in_states(&[JobState::Intake, JobState::Provisioning, JobState::Ready]) {
        for j in jobs {
            if let Some(t) = cimon_core::parse_time(&j.created_at) {
                if (chrono::Utc::now() - t).num_seconds() > limit {
                    finish_simple(app, j.id, JobState::QueueTimeout, None, None, Some("queue_timeout"), &format!("{}시간 동안 시작하지 못해 취소함", app.config.queue.max_queue_hours));
                }
            }
        }
    }
}

/// Restart recovery (design §8.4).
pub fn recover(app: &App) -> Vec<String> {
    let mut notes = vec![];
    if let Ok(jobs) = app.store.jobs_in_states(&[JobState::Running, JobState::Collecting, JobState::Cancelling]) {
        for j in jobs {
            if let Some(slot) = j.workspace_slot {
                let _ = app.store.clear_warm_state(&j.project_key, slot);
            }
            if j.state == JobState::Cancelling {
                finish_simple(app, j.id, JobState::Cancelled, None, None, None, "취소 중 서비스가 재시작됨 — 취소로 끝냄");
            } else {
                finish_simple(app, j.id, JobState::Interrupted, Some(Verdict::Inconclusive), Some(FailureClass::Infra), Some("interrupted"), "서비스 재시작으로 중단됨");
            }
            crate::run::cleanup_job_dirs(app, j.id);
            notes.push(format!("{} interrupted by the restart", job_label(j.id)));
        }
    }
    if let Ok(jobs) = app.store.jobs_in_states(&[JobState::Provisioning]) {
        for j in jobs {
            let _ = app.store.set_state(j.id, JobState::Intake, "접수: 서비스 재시작으로 다시");
        }
    }
    notes
}

/// Registry of cancel flags for jobs on the runner.
#[derive(Default)]
pub struct CancelRegistry(pub std::sync::Mutex<std::collections::HashMap<i64, Arc<AtomicBool>>>);

pub fn now_str() -> String {
    now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolchain_channel() {
        assert_eq!(parse_toolchain_channel(b"[toolchain]\nchannel = \"1.94.0\"\ncomponents = [\"clippy\"]\n").unwrap(), "1.94.0");
        assert_eq!(parse_toolchain_channel(b"1.91.1\n").unwrap(), "1.91.1");
        assert!(parse_toolchain_channel(b"[toolchain]\nprofile = \"minimal\"\n").is_err());
    }
}
