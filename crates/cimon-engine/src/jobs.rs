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
    /// Intake findings the job result repeats as warnings (an ignored version file).
    #[serde(default)]
    pub notes: Vec<String>,
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
        let min = app.config.disk.min_free_gb.saturating_mul(1 << 30);
        if free < min {
            // the janitor needs the runner lock; it runs in the background and the caller retries
            app.engine.janitor.notify_one();
            return Err(Rejection::new(
                "disk_low",
                format!(
                    "only {} GB free on the build machine (min {}) — cleanup has started; retry in a few minutes, or an admin frees space",
                    free / (1 << 30),
                    app.config.disk.min_free_gb
                ),
            ));
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
    let uploads = crate::uploads::check_handles(app, user, &v, &args)?;
    let mut args = args;
    args.clean = args.effective_clean(&v.plan);
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
    let id = app.store.insert_job_with_uploads(&new, &task_id, via_task, None, &uploads).map_err(|e| match e {
        // an upload another call took between the check and the insert
        cimon_store::StoreError::Conflict(m) => Rejection::new("upload_invalid", m),
        e => e.into(),
    })?;
    if !uploads.is_empty() {
        let detail = uploads.iter().map(|(n, h)| format!("{n}={h}")).collect::<Vec<_>>().join(" ");
        let _ = app.store.audit(cimon_store::AuditEntry { user, action: "upload_used", job_id: Some(id), detail: Some(detail), ..Default::default() });
    }
    app.engine.intake.notify_one();
    let job = app.store.require_job(id)?;
    Ok(CreatedTask { task_id, job_id: id, created_at: job.created_at, status: "접수: 대기".into() })
}

pub const PRE_START: &[JobState] = &[JobState::Intake, JobState::Provisioning, JobState::Ready];

/// `cancel_job`: before the runner → cancelled now; running → `cancelling`, the runner
/// tears down and finishes it. Every step is compare-and-set, so a cancel that races
/// a finish never resurrects the job.
pub fn cancel(app: &App, job_id: i64, by: &str) -> Result<String, Rejection> {
    let job = app.store.job(job_id)?.ok_or_else(|| Rejection::new("unknown_job", format!("no job {}", job_label(job_id))))?;
    if job.state.is_terminal() {
        return Ok(format!("{} already finished ({}) — nothing to cancel", job_label(job_id), job.state.as_str()));
    }
    app.store.request_cancel(job_id, by)?;
    let _ = app.store.audit(cimon_store::AuditEntry { user: by, action: "cancel", job_id: Some(job_id), ..Default::default() });
    Ok(apply_cancel(app, job_id, &format!("{by} 가 취소함")))
}

/// After `cancel_requested_by` is set: finish a pre-start job, or move a running one to
/// `cancelling` and raise its flag. Returns the caller-facing message.
fn apply_cancel(app: &App, job_id: i64, why: &str) -> String {
    if finish_simple_if(app, job_id, Some(PRE_START), JobState::Cancelled, None, None, None, why) {
        return format!("{} cancelled", job_label(job_id));
    }
    let _ = app.store.transition(job_id, &[JobState::Running, JobState::Collecting], JobState::Cancelling, &format!("취소 중 — 정리하는 중({why})"));
    if let Some(flag) = app.engine.running.lock().unwrap().get(&job_id) {
        flag.store(true, Ordering::SeqCst);
    }
    match app.store.job(job_id).ok().flatten() {
        Some(j) if j.state.is_terminal() => format!("{} already finished ({}) — nothing to cancel", job_label(job_id), j.state.as_str()),
        _ => format!("cancellation requested — job_status shows cancelled once {} has cleaned up", job_label(job_id)),
    }
}

/// `tasks/cancel`: detaches one task; the job is cancelled only when none remain. The
/// count and the cancel request are one transaction (a merge cannot slip in between).
pub fn detach_task(app: &App, task_id: &str, by: &str) -> Result<(), Rejection> {
    if app.store.task(task_id)?.is_none() {
        return Err(Rejection::new("unknown_task", format!("unknown task {task_id}")));
    }
    let (job_id, remaining) = app.store.detach_task_cancel(task_id, &format!("tasks/cancel by {by}"), by)?;
    if remaining == 0 {
        let _ = apply_cancel(app, job_id, "요청한 task 가 모두 떨어짐");
    }
    Ok(())
}

/// Ends a job outside the runner, with a minimal result.
pub fn finish_simple(app: &App, job_id: i64, state: JobState, verdict: Option<Verdict>, class: Option<FailureClass>, reason_code: Option<&str>, message: &str) -> bool {
    finish_simple_if(app, job_id, None, state, verdict, class, reason_code, message)
}

/// `finish_simple` only when the job is in one of `from`. Returns whether it applied.
#[allow(clippy::too_many_arguments)]
pub fn finish_simple_if(
    app: &App,
    job_id: i64,
    from: Option<&[JobState]>,
    state: JobState,
    verdict: Option<Verdict>,
    class: Option<FailureClass>,
    reason_code: Option<&str>,
    message: &str,
) -> bool {
    let Ok(Some(job)) = app.store.job(job_id) else { return false };
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
    let applied = match app.store.finish_job_if(
        job_id,
        from,
        state,
        verdict.map(|v| v.as_str()),
        class.map(|c| c.as_str()),
        reason_code,
        message,
        &summary,
        "[]",
        &result.to_string(),
    ) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("could not record the end of {}: {e}", job_label(job_id));
            false
        }
    };
    if applied {
        release_keeps(app, &job);
        crate::uploads::release_job(app, job_id);
    }
    applied
}

/// Drops the keep refs intake placed on the job's commits (design §11.1).
pub fn release_keeps(app: &App, job: &cimon_store::JobRow) {
    if let Some(m) = Mirror::open(&app.layout.mirror(&job.project_key)) {
        m.unkeep(&format!("j{}", job.id));
    }
    let sibs: Vec<SiblingPin> = job.siblings_json.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();
    for s in sibs {
        if let Some(m) = Mirror::open(&app.layout.mirror(&s.project_key)) {
            m.unkeep(&format!("j{}-sibling", job.id));
        }
    }
}

fn fail_intake(app: &App, job_id: i64, verdict: Verdict, class: FailureClass, code: &str, message: &str) {
    finish_simple(app, job_id, JobState::Done, Some(verdict), Some(class), Some(code), &format!("{code}: {message}"));
}

/// Intake must never leave a job behind without a state change: a job that is no
/// longer wanted (cancel requested) is finished as cancelled here. Returns true when
/// intake should stop working on it.
fn abandon_if_unwanted(app: &App, id: i64) -> bool {
    match app.store.job(id).ok().flatten() {
        None => true,
        Some(j) if j.state.is_terminal() => true,
        Some(j) if j.cancel_requested_by.is_some() => {
            let by = j.cancel_requested_by.clone().unwrap_or_default();
            if !finish_simple_if(app, id, Some(PRE_START), JobState::Cancelled, None, None, None, &format!("{by} 가 취소함")) {
                // it left pre-start meanwhile (e.g. merged): nothing more to do here
            }
            true
        }
        Some(j) if !j.state.is_pre_start() => true,
        _ => false,
    }
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
    // the mirror lock keeps GC away while intake fetches and reads (design §11.1)
    let lock = app.engine.mirror_lock(&job.project_key);
    let _mirror_guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    let mirror = match Mirror::fetch_with(&mirror_dir, &project.repo, &auth) {
        Ok(m) => m,
        Err(e) => return fail_intake(app, id, Verdict::Inconclusive, FailureClass::Infra, "fetch_failed", &format!("git fetch {}: {e}", project.repo)),
    };
    if abandon_if_unwanted(app, id) {
        return;
    }
    let ref_ = job.ref_.clone().unwrap_or_else(|| project.default_ref.clone());
    let allowed = |b: &str| cimon_recipe::select::ref_allowed(&project.allowed_refs, b);
    let commit = match mirror.resolve(&ref_, allowed) {
        Ok((c, _)) => c,
        Err(ResolveError::RefNotFound(m)) => return fail_intake(app, id, Verdict::Fail, FailureClass::Request, "ref_not_found", &m),
        Err(ResolveError::CommitNotAllowed(m)) => return fail_intake(app, id, Verdict::Fail, FailureClass::Request, "commit_not_allowed", &m),
    };
    // protect the pinned commit from GC until the job ends (released on every terminal path)
    if let Err(e) = mirror.keep(&format!("j{id}"), &commit) {
        return fail_intake(app, id, Verdict::Inconclusive, FailureClass::Infra, "git_error", &format!("keep ref: {e}"));
    }
    // a cancel or timeout that finished the job meanwhile would leave the ref behind
    if abandon_if_unwanted(app, id) {
        mirror.unkeep(&format!("j{id}"));
        return;
    }

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
        let slock = app.engine.mirror_lock(&s.project);
        let _sibling_guard = if s.project == job.project_key { None } else { Some(slock.lock().unwrap_or_else(|p| p.into_inner())) };
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
        let _ = sm.keep(&format!("j{id}-sibling"), &sc);
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
    let mut notes = vec![];
    for kind in &plan.toolchains {
        let Some(req) = resolved.toolchains.get(kind) else { continue };
        if let Some(v) = &req.version {
            versions.insert(kind.clone(), v.clone());
        } else if let Some(f) = &req.version_file {
            match read_version_file(&mirror, &commit, f, &tracked, &req.ignore_version_files) {
                Ok((v, n)) => {
                    versions.insert(kind.clone(), v);
                    notes.extend(n);
                }
                Err((code, msg)) => return fail_intake(app, id, Verdict::Fail, FailureClass::Recipe, code, &msg),
            }
        }
    }
    if let Some(msg) = tracked_glob_violation(&resolved, &plan, &tracked) {
        return fail_intake(app, id, Verdict::Fail, FailureClass::Recipe, "glob_matches_tracked", &msg);
    }

    let job_plan = JobPlan { plan: plan.clone(), versions, siblings: siblings.clone(), repo: project.repo.clone(), notes };
    let merge_key = merge_key(&job, &commit, &siblings, &plan, request.args.clean, &request.args);
    let _ = app.store.set_intake(id, &commit, &serde_json::to_string(&siblings).unwrap(), &serde_json::to_string(&job_plan).unwrap(), &merge_key);
    let _ = app.store.set_excluded(id, &plan.excluded);
    drop(_mirror_guard);
    if abandon_if_unwanted(app, id) {
        return;
    }
    if try_merge(app, id, &merge_key) {
        return;
    }

    // provisioning (design §10.4)
    if !app.store.transition(id, &[JobState::Intake], JobState::Provisioning, "툴체인 확인").unwrap_or(false) {
        abandon_if_unwanted(app, id);
        return;
    }
    match crate::toolchains::provide(app, id, &job, &resolved, &job_plan) {
        Ok(()) => {}
        Err(p) => return fail_intake(app, id, p.verdict, p.class, p.code, &p.message),
    }
    if abandon_if_unwanted(app, id) {
        return;
    }
    // a duplicate may have become ready while this one provisioned
    if try_merge(app, id, &merge_key) {
        return;
    }
    if !app.store.transition(id, &[JobState::Provisioning], JobState::Ready, "대기열").unwrap_or(false) {
        abandon_if_unwanted(app, id);
        return;
    }
    refresh_queue_positions(app);
}

/// Merges `id` into a ready/running job with the same key, if there is one and both
/// sides are still eligible (checked inside the store transaction).
fn try_merge(app: &App, id: i64, merge_key: &str) -> bool {
    let Ok(Some(target)) = app.store.merge_target(merge_key, id) else { return false };
    match app.store.merge_job(id, target, &format!("{} 과 같은 요청 — 그 작업을 따른다", job_label(target))) {
        Ok(true) => {
            if let Ok(Some(j)) = app.store.job(id) {
                release_keeps(app, &j);
            }
            true
        }
        _ => false,
    }
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
    // a job that gets uploads is its caller's alone: nothing merges into it, and it
    // merges into nothing (design §11.7)
    let mut v = v;
    if !args.uploads.is_empty() {
        v["uploads_job"] = json!(job.id);
    }
    cimon_core::sha256_hex(v.to_string().as_bytes())
}

/// `version_file` (recipe-spec §5): read at the pinned commit; every other
/// `rust-toolchain.toml` in the repository must agree, except those the recipe lists in
/// `ignore_version_files` — returned as notes the job result repeats, so building them
/// with this version is never silent.
fn read_version_file(mirror: &Mirror, commit: &str, path: &str, tracked: &[String], ignore: &[String]) -> Result<(String, Vec<String>), (&'static str, String)> {
    let Some(bytes) = mirror.show(commit, path) else {
        return Err(("version_file_missing", format!("{path} does not exist at {}", &commit[..10])));
    };
    let v = parse_toolchain_channel(&bytes).map_err(|e| ("version_file_missing", format!("{path}: {e}")))?;
    let exact = regex::Regex::new(cimon_recipe::check::EXACT_VERSION).unwrap();
    if !exact.is_match(&v) {
        return Err(("version_file_mismatch", format!("{path}: channel {v:?} is not an exact version")));
    }
    let mut notes = vec![];
    for other in tracked {
        let name = other.rsplit('/').next().unwrap_or(other);
        if other != path && (name == "rust-toolchain.toml" || name == "rust-toolchain") {
            if let Some(b) = mirror.show(commit, other) {
                if let Ok(ov) = parse_toolchain_channel(&b) {
                    if ov != v {
                        if ignore.iter().any(|i| i == other) {
                            notes.push(format!("{other} 은 {ov} 를 고정하지만 레시피의 ignore_version_files 에 있어 {v}({path})로 짓는다"));
                            continue;
                        }
                        return Err((
                            "version_file_mismatch",
                            format!("rust-toolchain files disagree: {path} = {v}, {other} = {ov} — if that is intended, list {other} in the recipe's ignore_version_files"),
                        ));
                    }
                }
            }
        }
    }
    for i in ignore {
        if !tracked.iter().any(|t| t == i) {
            notes.push(format!("ignore_version_files 의 {i} 가 이 커밋에 없다 — 레시피에서 지워도 된다"));
        }
    }
    Ok((v, notes))
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
    crate::uploads::sweep(app);
    let limit = app.config.queue.max_queue_hours as i64 * 3600;
    if let Ok(jobs) = app.store.jobs_in_states(&[JobState::Intake, JobState::Provisioning, JobState::Ready]) {
        for j in jobs {
            if let Some(t) = cimon_core::parse_time(&j.created_at) {
                if (chrono::Utc::now() - t).num_seconds() > limit {
                    finish_simple_if(app, j.id, Some(PRE_START), JobState::QueueTimeout, None, None, Some("queue_timeout"), &format!("{}시간 동안 시작하지 못해 취소함", app.config.queue.max_queue_hours));
                }
            }
        }
    }
}

/// Restart recovery (design §8.4).
pub fn recover(app: &App) -> Vec<String> {
    let mut notes = vec![];
    // a cancel requested before the restart is honoured first (intake would otherwise
    // refuse to move the job and leave it to the queue timeout)
    if let Ok(jobs) = app.store.jobs_in_states(PRE_START) {
        for j in jobs.into_iter().filter(|j| j.cancel_requested_by.is_some()) {
            let by = j.cancel_requested_by.clone().unwrap_or_default();
            finish_simple_if(app, j.id, Some(PRE_START), JobState::Cancelled, None, None, None, &format!("{by} 가 취소함(서비스 재시작 뒤)"));
            notes.push(format!("{} cancelled (requested before the restart)", job_label(j.id)));
        }
    }
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
            let _ = cimon_workspace::remove_dir_all_robust(&app.layout.sibling_isolated(&j.project_key, j.id));
            if j.workspace_mode.as_deref() == Some("clean") {
                let _ = cimon_workspace::remove_dir_all_robust(&app.layout.clean_clone(&j.project_key, j.id));
            }
            release_keeps(app, &j);
            notes.push(format!("{} interrupted by the restart", job_label(j.id)));
        }
    }
    if let Ok(jobs) = app.store.jobs_in_states(&[JobState::Provisioning]) {
        for j in jobs {
            let _ = app.store.transition(j.id, &[JobState::Provisioning], JobState::Intake, "접수: 서비스 재시작으로 다시");
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
    fn other_version_files_must_agree_unless_listed() {
        let d = tempfile::tempdir().unwrap();
        let w = d.path().join("w");
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git").arg("-C").arg(&w).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        for (p, v) in [("eb/rust-toolchain.toml", "1.94.0"), ("tools/pack/rust-toolchain.toml", "1.91.1")] {
            let f = w.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, format!("[toolchain]\nchannel = \"{v}\"\n")).unwrap();
        }
        std::process::Command::new("git").args(["init", "-q", "-b", "main"]).arg(&w).status().unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "two pins"]);
        let commit = git(&["rev-parse", "HEAD"]);
        let mirror = Mirror::fetch(&d.path().join("m.git"), &w.to_string_lossy().replace('\\', "/")).unwrap();
        let tracked = mirror.tracked(&commit).unwrap();
        let (code, msg) = read_version_file(&mirror, &commit, "eb/rust-toolchain.toml", &tracked, &[]).unwrap_err();
        assert_eq!(code, "version_file_mismatch");
        assert!(msg.contains("ignore_version_files"), "the refusal says how to allow it: {msg}");
        let ignore = ["tools/pack/rust-toolchain.toml".to_string(), "gone/rust-toolchain.toml".to_string()];
        let (v, notes) = read_version_file(&mirror, &commit, "eb/rust-toolchain.toml", &tracked, &ignore).unwrap();
        assert_eq!(v, "1.94.0");
        assert!(notes.iter().any(|n| n.contains("tools/pack/rust-toolchain.toml") && n.contains("1.91.1")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("gone/rust-toolchain.toml")), "a stale entry is reported: {notes:?}");
    }

    #[test]
    fn toolchain_channel() {
        assert_eq!(parse_toolchain_channel(b"[toolchain]\nchannel = \"1.94.0\"\ncomponents = [\"clippy\"]\n").unwrap(), "1.94.0");
        assert_eq!(parse_toolchain_channel(b"1.91.1\n").unwrap(), "1.91.1");
        assert!(parse_toolchain_channel(b"[toolchain]\nprofile = \"minimal\"\n").is_err());
    }
}
