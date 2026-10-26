//! The runner (design §9.1): one job at a time, steps in order, every outcome recorded.

use crate::jobs::{self, JobPlan, JobRequest};
use crate::App;
use cimon_core::{job_label, now, FailureClass, JobState, Outcome, Verdict, Warning};
use cimon_recipe::model::Kind;
use cimon_recipe::resolve::Resolved;
use cimon_recipe::select::param_string;
use cimon_runner::{exec, ExecSpec};
use cimon_store::{JobRow, StepRow};
use cimon_workspace::{Mirror, WorkTree};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub async fn runner_loop(app: Arc<App>) {
    loop {
        if app.engine.shutdown.is_cancelled() {
            return;
        }
        let next = app.store.oldest_in_state(JobState::Ready).ok().flatten();
        let Some(job) = next else {
            tokio::select! {
                _ = app.engine.runner.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                _ = app.engine.shutdown.cancelled() => return,
            }
            continue;
        };
        let guard = app.engine.runner_lock.lock().await;
        if app.engine.shutdown.is_cancelled() {
            return;
        }
        let flag = Arc::new(AtomicBool::new(false));
        app.engine.running.lock().unwrap().insert(job.id, flag.clone());
        let a = app.clone();
        let id = job.id;
        let res = tokio::task::spawn_blocking(move || run_job(&a, id, &flag)).await;
        app.engine.running.lock().unwrap().remove(&id);
        if let Err(e) = res {
            tracing::error!("runner panicked on {}: {e}", job_label(id));
            if let Ok(Some(j)) = app.store.job(id) {
                if let Some(slot) = j.workspace_slot {
                    let _ = app.store.clear_warm_state(&j.project_key, slot);
                }
                if j.cancel_requested_by.is_some() {
                    jobs::finish_simple(&app, id, JobState::Cancelled, None, None, None, "취소 중 러너가 멈췄다 — 취소로 끝냄");
                } else {
                    jobs::finish_simple(&app, id, JobState::Done, Some(Verdict::Inconclusive), Some(FailureClass::Infra), Some("internal_error"), "the runner crashed — see the service log");
                }
                jobs::release_keeps(&app, &j);
                let _ = cimon_workspace::remove_dir_all_robust(&app.layout.sibling_isolated(&j.project_key, id));
            }
            cleanup_job_dirs(&app, id);
        }
        drop(guard);
        jobs::refresh_queue_positions(&app);
    }
}

/// The resolved recipe for a job: pinned recipe commit, with the `project.toml`
/// snapshot validation used (design §6.2).
pub fn resolve_pinned(app: &App, job: &JobRow, request: &JobRequest) -> Result<Resolved, String> {
    let commit = job.recipe_commit.clone().ok_or("job has no recipe commit")?;
    let tree = app.recipes.tree(&commit).map_err(|e| e.to_string())?;
    let mut r = cimon_recipe::resolve_project(&*tree, &job.project).map_err(|d| d.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("; "))?;
    r.project = request.project.clone();
    Ok(r)
}

/// One step's record.
#[derive(Debug, Clone)]
pub struct StepRec {
    pub seq: i64,
    pub id: String,
    pub kind: Kind,
    pub result: String,
    pub outcome: Option<Outcome>,
    pub exit_code: Option<i64>,
    pub started: Option<String>,
    pub finished: Option<String>,
    pub seconds: f64,
    pub excerpt: Option<String>,
    pub extra: Value,
}

impl StepRec {
    fn row(&self) -> StepRow {
        StepRow {
            seq: self.seq,
            step_id: self.id.clone(),
            kind: self.kind.as_str().into(),
            result: self.result.clone(),
            failure_class: self.outcome.as_ref().and_then(|o| o.class).map(|c| c.as_str().to_string()),
            reason: self.outcome.as_ref().map(|o| o.reason.clone()),
            exit_code: self.exit_code,
            started_at: self.started.clone(),
            finished_at: self.finished.clone(),
            excerpt: self.excerpt.clone(),
            judge_json: Some(self.extra.to_string()),
        }
    }
}

/// Everything a step needs from the job.
pub struct JobCtx<'a> {
    pub app: &'a Arc<App>,
    pub job: JobRow,
    pub request: JobRequest,
    pub plan: JobPlan,
    pub resolved: Resolved,
    pub commit: String,
    pub ws: PathBuf,
    pub recipes_dir: PathBuf,
    pub tool_env: crate::toolchains::ToolEnv,
    pub siblings: BTreeMap<String, PathBuf>,
    pub artifacts: BTreeMap<String, PathBuf>,
    pub warnings: Vec<Warning>,
    pub account: cimon_runner::Account,
    pub profile: Option<PathBuf>,
    pub build_user: Option<String>,
    pub cancel: &'a AtomicBool,
    pub deadline: Instant,
    pub clean: bool,
    /// This project's `cimon build-secret`s (design §11.6) — or why they could not be read.
    pub secrets: Result<BTreeMap<String, String>, String>,
}

pub fn run_job(app: &Arc<App>, id: i64, cancel: &AtomicBool) {
    // a stopping service starts nothing new (the job stays ready for the next start)
    if app.engine.shutdown.is_cancelled() {
        return;
    }
    // compare-and-set: a cancel or queue timeout that landed first wins
    if !app.store.transition(id, &[JobState::Ready], JobState::Running, "작업 공간 준비").unwrap_or(false) {
        if let Ok(Some(j)) = app.store.job(id) {
            if j.state == JobState::Ready && j.cancel_requested_by.is_some() {
                jobs::finish_simple_if(app, id, Some(jobs::PRE_START), JobState::Cancelled, None, None, None, "시작 전에 취소됨");
            }
        }
        return;
    }
    let Ok(job) = app.store.require_job(id) else { return };
    let started = Instant::now();
    let request: JobRequest = serde_json::from_str(&job.request_json).unwrap();
    let plan: JobPlan = serde_json::from_str(job.plan_json.as_deref().unwrap_or("{}")).unwrap_or_default();
    let resolved = match resolve_pinned(app, &job, &request) {
        Ok(r) => r,
        Err(e) => {
            jobs::finish_simple(app, id, JobState::Done, Some(Verdict::Fail), Some(FailureClass::Recipe), Some("recipe_invalid"), &e);
            return;
        }
    };
    let commit = job.commit_sha.clone().unwrap_or_default();
    let mut warnings: Vec<Warning> = plan.notes.iter().map(|n| Warning::new("toolchain", n.clone())).collect();
    let build = app.build_account();
    if build.is_some() {
        sweep(app, &mut warnings, "시작 전");
    }
    let deadline = started + Duration::from_secs(resolved.project.max_job_minutes as u64 * 60);

    // workspace (design §11) — its git runs as the build account
    let tree_git = BuildGit::new(app, id, build.as_deref());
    let ws_res = prepare_workspace(app, &tree_git, &job, &request, &resolved, &plan, &commit, &mut warnings);
    let (ws, recreated, slot) = match ws_res {
        Ok(x) => x,
        Err(e) => {
            // a half-recreated slot must not skip setup next time
            if let Some(s) = app.store.job(id).ok().flatten().and_then(|j| j.workspace_slot) {
                let _ = app.store.clear_warm_state(&job.project_key, s);
            }
            finish_infra(app, &job, &format!("작업 공간 준비 실패: {e}"), &warnings);
            cleanup(app, &job, None, request.args.clean, &plan, &resolved);
            return;
        }
    };
    let mut siblings = BTreeMap::new();
    for s in &plan.siblings {
        match prepare_sibling(app, &tree_git, &job, s, &ws) {
            Ok(dir) => {
                siblings.insert(s.project.clone(), dir);
            }
            Err(e) => {
                finish_infra(app, &job, &format!("형제 {} 준비 실패: {e}", s.project), &warnings);
                cleanup(app, &job, slot, request.args.clean, &plan, &resolved);
                return;
            }
        }
    }
    let recipes_dir = match app.recipes.checkout(job.recipe_commit.as_deref().unwrap_or_default(), &app.layout.recipe_checkout(job.recipe_commit.as_deref().unwrap_or_default())) {
        Ok(d) => d,
        Err(e) => {
            finish_infra(app, &job, &format!("레시피 체크아웃 실패: {e}"), &warnings);
            cleanup(app, &job, slot, request.args.clean, &plan, &resolved);
            return;
        }
    };
    let tool_env = match crate::toolchains::env_for(app, id, &resolved, &plan) {
        Ok(t) => t,
        Err(e) => {
            jobs::finish_simple(app, id, JobState::Done, Some(Verdict::Inconclusive), Some(FailureClass::Environment), Some("toolchain_not_provisioned"), &e);
            cleanup(app, &job, slot, request.args.clean, &plan, &resolved);
            return;
        }
    };
    let account = build.as_ref().map(|b| b.account()).unwrap_or(cimon_runner::Account::Current);
    let mut ctx = JobCtx {
        app,
        job: job.clone(),
        request: request.clone(),
        plan: plan.clone(),
        resolved: resolved.clone(),
        commit: commit.clone(),
        ws: ws.clone(),
        recipes_dir,
        tool_env,
        siblings,
        artifacts: BTreeMap::new(),
        warnings,
        account,
        profile: build.as_ref().map(|b| b.profile_dir.clone()),
        build_user: build.as_ref().map(|b| b.name.clone()),
        cancel,
        deadline,
        clean: request.args.clean,
        secrets: crate::build_secrets::load(&app.layout).map(|mut all| all.remove(&job.project).unwrap_or_default()),
    };
    crate::fixtures::account_env(&mut ctx, true);

    let records = run_steps(&mut ctx, slot, recreated);

    crate::fixtures::account_env(&mut ctx, false);
    let user_cancel = app.store.job(id).ok().flatten().is_some_and(|j| j.cancel_requested_by.is_some());
    let shutdown = app.engine.shutdown.is_cancelled();
    // interrupted only if the stop actually cut a step short
    let cut_short = records.iter().any(|r| r.outcome.as_ref().is_some_and(|o| o.reason == "cancelled"));
    if shutdown && !user_cancel && cut_short {
        // the service is stopping: nobody cancelled this job, it was interrupted
        if build.is_some() {
            sweep(app, &mut ctx.warnings, "서비스 중지");
        }
        if let Some(s) = slot {
            let _ = app.store.clear_warm_state(&job.project_key, s);
        }
        jobs::finish_simple(app, id, JobState::Interrupted, Some(Verdict::Inconclusive), Some(FailureClass::Infra), Some("interrupted"), "서비스 중지로 중단됨 — 다시 요청하라");
        cleanup(app, &job, slot, request.args.clean, &plan, &resolved);
        return;
    }
    // the flag is also set by a service stop; that alone is not a cancel
    let cancelled = user_cancel || (cancel.load(Ordering::SeqCst) && !shutdown);
    let _ = app.store.transition(id, &[JobState::Running], JobState::Collecting, "결과 정리");
    if build.is_some() {
        sweep(app, &mut ctx.warnings, "작업 끝");
    }
    let verdict = finish(&ctx, &records, cancelled);
    // anything but a clean pass (cancel, timeout, failure) forgets the slot's setup state
    if verdict != Some(Verdict::Pass) {
        if let Some(s) = slot {
            let _ = app.store.clear_warm_state(&job.project_key, s);
        }
    }
    cleanup(app, &job, slot, request.args.clean, &plan, &resolved);
}

fn finish_infra(app: &App, job: &JobRow, msg: &str, warnings: &[Warning]) {
    let _ = warnings;
    jobs::finish_simple(app, job.id, JobState::Done, Some(Verdict::Inconclusive), Some(FailureClass::Infra), Some("workspace_failed"), msg);
}

/// Work-tree git (clone, checkout, clean) run through the runner as the build account
/// (design §11.1), each command logged under `logs\j<id>\_workspace\` — step ids cannot
/// start with `_`. Not interrupted by a cancel: like the rest of the preparation, it
/// finishes and the cancel takes effect at the first step.
pub(crate) struct BuildGit<'a> {
    app: &'a App,
    job: i64,
    account: cimon_runner::Account,
    program: PathBuf,
    env: BTreeMap<String, String>,
    n: std::sync::atomic::AtomicUsize,
}

impl<'a> BuildGit<'a> {
    pub(crate) fn new(app: &'a App, job: i64, build: Option<&cimon_runner::BuildAccount>) -> Self {
        let program = git_program(app);
        let tmp = app.layout.root.join("tmp").to_string_lossy().into_owned();
        let mut env = cimon_runner::base_env(build.map(|b| b.profile_dir.as_path()), build.map(|b| b.name.as_str()));
        let mut path: Vec<PathBuf> = program.parent().map(|p| vec![p.to_path_buf()]).unwrap_or_default();
        path.extend(cimon_runner::system_path());
        env.insert("PATH".into(), cimon_runner::join_path(&path));
        env.insert("TEMP".into(), tmp.clone());
        env.insert("TMP".into(), tmp);
        env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
        BuildGit { app, job, account: build.map(|b| b.account()).unwrap_or(cimon_runner::Account::Current), program, env, n: Default::default() }
    }
}

impl cimon_workspace::TreeGit for BuildGit<'_> {
    fn run(&self, args: &[String]) -> cimon_workspace::Result<()> {
        let n = self.n.fetch_add(1, Ordering::SeqCst) + 1;
        let sub = args.iter().skip(cimon_workspace::GIT_OPTIONS.len()).find(|a| !a.starts_with('-') && !std::path::Path::new(a).is_absolute()).cloned().unwrap_or_default();
        let spec = ExecSpec {
            program: self.program.clone(),
            args: args.to_vec(),
            cwd: self.app.layout.root.join("tmp"),
            env: self.env.clone(),
            timeout: Duration::from_secs(30 * 60),
            log_path: self.app.layout.job_logs(self.job).join("_workspace").join(format!("{n:02}-git-{sub}.log")),
            log_cap: 4 << 20,
            error_patterns: vec![],
            fail_on_output: vec![],
            capture_prefixes: vec![],
            preamble: vec![format!("git {}", args[cimon_workspace::GIT_OPTIONS.len()..].join(" "))],
        };
        if let Some(p) = spec.log_path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let res = exec::execute(&spec, &self.account, &AtomicBool::new(false));
        if res.exit_code == Some(0) {
            return Ok(());
        }
        let why = res
            .spawn_error
            .clone()
            .or_else(|| res.timed_out.then(|| "timed out after 30 minutes".to_string()))
            .unwrap_or_else(|| format!("exit {:?}: {}", res.exit_code, res.tail.iter().rev().take(5).rev().cloned().collect::<Vec<_>>().join(" | ")));
        Err(cimon_workspace::WsError::Git { args: sub, stderr: why })
    }
}

/// git for work-tree commands: Git for Windows under `[base].git`, else the first git
/// on the service's PATH (the build account gets the absolute path either way).
fn git_program(app: &App) -> PathBuf {
    if let Some(root) = app.config.base.get("git") {
        let p = root.join("cmd").join("git.exe");
        if p.is_file() {
            return p;
        }
    }
    let path: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE".into());
    cimon_runner::resolve_program("git", Path::new("."), &path, &pathext).unwrap_or_else(|_| PathBuf::from("git"))
}

/// Refuses a directory the service is about to create or delete under a tree the
/// build account can change when a parent on the way is a link or junction.
fn check_no_links(root: &Path, dir: &Path) -> Result<(), String> {
    match cimon_runner::link_on_path(root, dir) {
        Some(l) => Err(format!("{} is a link or junction — refusing to work under it", l.display())),
        None => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_workspace(
    app: &App,
    git: &dyn cimon_workspace::TreeGit,
    job: &JobRow,
    request: &JobRequest,
    r: &Resolved,
    plan: &JobPlan,
    commit: &str,
    warnings: &mut Vec<Warning>,
) -> Result<(PathBuf, bool, Option<i64>), String> {
    let mirror = Mirror::open(&app.layout.mirror(&job.project_key)).ok_or("mirror missing")?;
    let keep: Vec<String> = r
        .keep
        .iter()
        .map(|k| cimon_recipe::vars::expand(k, &|n| n.strip_prefix("vars.").and_then(|x| r.vars.get(x).cloned())).unwrap_or_else(|_| k.clone()))
        .collect();
    let _ = plan;
    if request.args.clean {
        let dir = app.layout.clean_clone(&job.project_key, job.id);
        check_no_links(&app.layout.ws(), &dir)?;
        mirror.keep(&format!("j{}", job.id), commit).map_err(|e| e.to_string())?;
        WorkTree::create(git, &mirror, &dir, commit).map_err(|e| e.to_string())?;
        let _ = app.store.set_workspace(job.id, "clean", None);
        return Ok((dir, true, None));
    }
    let n = r.project.warm_slots.unwrap_or(app.config.workspace.warm_slots_per_project).max(1) as i64;
    let slots = app.store.warm_slots(&job.project_key).map_err(|e| e.to_string())?;
    let ref_ = job.ref_.clone().unwrap_or_default();
    let slot = slots
        .iter()
        .find(|(s, last_ref, _, _)| *s <= n && last_ref.as_deref() == Some(ref_.as_str()))
        .map(|(s, ..)| *s)
        .or_else(|| (1..=n).find(|s| !slots.iter().any(|(x, ..)| x == s)))
        .or_else(|| slots.iter().filter(|(s, ..)| *s <= n).min_by(|a, b| a.3.cmp(&b.3)).map(|(s, ..)| *s))
        .unwrap_or(1);
    let dir = app.layout.warm_slot(&job.project_key, slot as u32);
    check_no_links(&app.layout.ws(), &dir)?;
    let _ = app.store.set_workspace(job.id, "warm", Some(slot));
    let (_, recreated, note) = WorkTree::prepare_warm(git, &mirror, &dir, commit, &keep).map_err(|e| e.to_string())?;
    if let Some(note) = note {
        warnings.push(Warning::new("workspace", note));
    }
    if recreated {
        let _ = app.store.clear_warm_state(&job.project_key, slot);
    }
    mirror.keep(&format!("slot-{slot}"), commit).map_err(|e| e.to_string())?;
    let _ = app.store.use_warm_slot(&job.project_key, slot, &ref_, commit);
    let _ = app.store.set_workspace(job.id, "warm", Some(slot));
    Ok((dir, recreated, Some(slot)))
}

fn prepare_sibling(app: &App, git: &dyn cimon_workspace::TreeGit, job: &JobRow, s: &jobs::SiblingPin, ws: &Path) -> Result<PathBuf, String> {
    let mirror = Mirror::open(&app.layout.mirror(&s.project_key)).ok_or("sibling mirror missing")?;
    let dir = if s.place == "beside" {
        ws.parent().ok_or("work tree has no parent")?.join(&s.project)
    } else {
        app.layout.sibling_isolated(&job.project_key, job.id).join(&s.project)
    };
    check_no_links(&app.layout.ws(), &dir)?;
    let (_, _, _) = WorkTree::prepare_warm(git, &mirror, &dir, &s.commit, &[]).map_err(|e| e.to_string())?;
    mirror.keep(&format!("j{}-sibling", job.id), &s.commit).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Removes per-job directories (design §9.1 step 7, §16.4 crash leftovers).
pub fn cleanup_job_dirs(app: &App, id: i64) {
    let _ = cimon_workspace::remove_dir_all_robust(&app.layout.tmp_job(id));
    let _ = cimon_workspace::remove_dir_all_robust(&app.layout.homes_job_dir(id));
}

fn cleanup(app: &App, job: &JobRow, _slot: Option<i64>, clean: bool, plan: &JobPlan, _r: &Resolved) {
    cleanup_job_dirs(app, job.id);
    if let Some(m) = Mirror::open(&app.layout.mirror(&job.project_key)) {
        m.unkeep(&format!("j{}", job.id));
    }
    for s in &plan.siblings {
        if let Some(m) = Mirror::open(&app.layout.mirror(&s.project_key)) {
            m.unkeep(&format!("j{}-sibling", job.id));
        }
    }
    let mut dirs = vec![app.layout.sibling_isolated(&job.project_key, job.id)];
    if clean {
        dirs.push(app.layout.clean_clone(&job.project_key, job.id));
    }
    for d in dirs {
        match check_no_links(&app.layout.ws(), &d) {
            Ok(()) => {
                let _ = cimon_workspace::remove_dir_all_robust(&d);
            }
            Err(e) => tracing::warn!("{} not removed: {e}", d.display()),
        }
    }
}

/// Build-account leftover sweep (design §9.2), run as that account.
fn sweep(app: &App, warnings: &mut Vec<Warning>, when: &str) {
    let Some(acct) = app.build_account() else { return };
    // an install script or probe running as the build account finishes first
    let _guard = app.build_guard();
    let tmp = app.layout.root.join("tmp");
    let spec = ExecSpec {
        program: acct.helper.clone(),
        args: vec!["__sweep".into()],
        cwd: tmp.clone(),
        env: cimon_runner::base_env(Some(&acct.profile_dir), Some(&acct.name)),
        timeout: Duration::from_secs(120),
        log_path: app.layout.service_logs().join("sweep.log"),
        log_cap: 1 << 20,
        error_patterns: vec![],
        fail_on_output: vec![],
        capture_prefixes: vec!["{".into()],
        preamble: vec![],
    };
    let res = exec::execute(&spec, &acct.account(), &AtomicBool::new(false));
    let report: Option<cimon_runner::SweepReport> = res.captured.iter().rev().find_map(|l| serde_json::from_str(l).ok());
    match report {
        Some(r) => {
            if !r.killed.is_empty() || !r.tasks_deleted.is_empty() {
                warnings.push(Warning::new(
                    "sweep",
                    format!("{when}: 빌드 계정에 남아 있던 프로세스 {}개·예약 작업 {}개를 정리했다 ({})", r.killed.len(), r.tasks_deleted.len(), r.killed.iter().chain(r.tasks_deleted.iter()).cloned().collect::<Vec<_>>().join(", ")),
                ));
            }
            for e in r.errors {
                warnings.push(Warning::new("sweep", format!("{when}: {e}")));
            }
        }
        None => warnings.push(Warning::new("sweep", format!("{when}: 빌드 계정 잔류 프로세스 정리 결과를 읽지 못했다 — exit {:?}", res.exit_code))),
    }
}

fn dur(secs: f64) -> String {
    let s = secs as u64;
    if s >= 60 {
        format!("{}분 {}초", s / 60, s % 60)
    } else {
        format!("{s}초")
    }
}

/// Steps in order with dependency gating, setup `when`, fixtures, artifacts.
fn run_steps(ctx: &mut JobCtx<'_>, slot: Option<i64>, recreated: bool) -> Vec<StepRec> {
    let app = ctx.app;
    let id = ctx.job.id;
    let r = ctx.resolved.clone();
    let plan = ctx.plan.plan.clone();
    let mut order: Vec<(String, Kind)> = plan.setup.iter().map(|s| (s.clone(), Kind::Setup)).collect();
    order.extend(plan.steps.iter().map(|s| (s.id.clone(), s.kind)));
    let mut recs: Vec<StepRec> = order
        .iter()
        .enumerate()
        .map(|(i, (sid, kind))| StepRec {
            seq: i as i64,
            id: sid.clone(),
            kind: *kind,
            result: "pending".into(),
            outcome: None,
            exit_code: None,
            started: None,
            finished: None,
            seconds: 0.0,
            excerpt: None,
            extra: json!({}),
        })
        .collect();
    let _ = app.store.replace_steps(id, &recs.iter().map(|r| r.row()).collect::<Vec<_>>());
    let producers = r.artifact_producers();
    let mut fixtures = crate::fixtures::Fixtures::new(&r, &plan);
    let total = plan.steps.len();
    let mut setup_failed: Option<String> = None;
    for i in 0..recs.len() {
        let (sid, kind) = (recs[i].id.clone(), recs[i].kind);
        let step = r.step(&sid).unwrap().clone();
        if ctx.cancel.load(Ordering::SeqCst) {
            recs[i].result = "not_run".into();
            recs[i].outcome = Some(Outcome::inconclusive(FailureClass::Infra, "cancelled"));
            continue;
        }
        if let Some(f) = &setup_failed {
            recs[i].result = "not_run".into();
            recs[i].outcome = Some(Outcome { verdict: Verdict::Inconclusive, class: None, reason: format!("돌지 않음: {f} 실패") });
            let _ = app.store.update_step(id, &recs[i].row());
            continue;
        }
        // dependency gating
        let mut deps: Vec<String> = step.def.after.clone();
        if let Some(f) = step.def.fixture.as_ref().and_then(|f| r.fixtures.get(f)) {
            deps.extend(f.after.clone());
            deps.extend(f.requires_artifacts.iter().filter_map(|a| producers.get(a).cloned()));
        }
        if let Some(bad) = deps.iter().find(|d| recs.iter().any(|x| &x.id == *d && !matches!(x.result.as_str(), "pass" | "skipped"))) {
            recs[i].result = "not_run".into();
            recs[i].outcome = Some(Outcome { verdict: Verdict::Inconclusive, class: None, reason: format!("돌지 않음: {bad} 실패") });
            let _ = app.store.update_step(id, &recs[i].row());
            fixtures.step_done(ctx, &sid, &mut recs);
            continue;
        }
        if Instant::now() >= ctx.deadline {
            recs[i].result = "fail".into();
            recs[i].outcome = Some(Outcome::fail(FailureClass::Code, format!("작업 시간 제한 {}분을 넘겨 돌지 않음", ctx.resolved.project.max_job_minutes)));
            let _ = app.store.update_step(id, &recs[i].row());
            continue;
        }
        // setup `when` (design §11.3)
        if kind == Kind::Setup {
            if let Some(reason) = setup_skip(ctx, &step, slot, recreated) {
                recs[i].result = "skipped".into();
                recs[i].outcome = Some(Outcome::pass(reason));
                let _ = app.store.update_step(id, &recs[i].row());
                continue;
            }
        }
        // fixture up
        if let Some(fname) = step.def.fixture.clone() {
            if let Err(out) = fixtures.ensure_up(ctx, &fname) {
                recs[i].result = verdict_result(out.verdict);
                recs[i].outcome = Some(out);
                recs[i].finished = Some(now());
                let _ = app.store.update_step(id, &recs[i].row());
                fixtures.step_done(ctx, &sid, &mut recs);
                continue;
            }
        }
        let label = if kind == Kind::Setup {
            format!("setup {sid}")
        } else {
            let n = plan.steps.iter().position(|p| p.id == sid).map(|p| p + 1).unwrap_or(0);
            format!("{sid} ({n}/{total})")
        };
        recs[i].started = Some(now());
        recs[i].result = "running".into();
        let _ = app.store.update_step(id, &recs[i].row());
        run_one(ctx, &step, &label, &mut recs[i]);
        recs[i].finished = Some(now());
        if kind == Kind::Setup {
            if recs[i].result == "pass" {
                if let Some(s) = slot {
                    record_setup_ok(ctx, &step, s);
                }
            } else {
                setup_failed = Some(sid.clone());
                if let Some(s) = slot {
                    let _ = app.store.clear_warm_state(&ctx.job.project_key, s);
                }
            }
        }
        let _ = app.store.update_step(id, &recs[i].row());
        fixtures.step_done(ctx, &sid, &mut recs);
    }
    fixtures.teardown_all(ctx, &mut recs);
    let _ = app.store.replace_steps(id, &recs.iter().map(|r| r.row()).collect::<Vec<_>>());
    recs
}

fn verdict_result(v: Verdict) -> String {
    v.as_str().to_string()
}

/// Runs one step: environment, command, pre-clean, judge, artifacts.
fn run_one(ctx: &mut JobCtx<'_>, step: &cimon_recipe::Step, label: &str, rec: &mut StepRec) {
    let app = ctx.app;
    let id = ctx.job.id;
    let sid = step.id().to_string();
    let out_dir = app.layout.step_out(id, &sid);
    let _ = std::fs::create_dir_all(&out_dir);
    let fail_recipe = |rec: &mut StepRec, msg: String| {
        rec.result = "fail".into();
        rec.outcome = Some(Outcome::fail(FailureClass::Recipe, msg));
    };
    // the logs tree is service-only; the build account may write this step's out dir
    if let Some(user) = ctx.build_user.clone() {
        if let Err(e) = cimon_runner::grant_modify(&out_dir, &user) {
            rec.result = "inconclusive".into();
            rec.outcome = Some(Outcome::inconclusive(FailureClass::Infra, format!("${{step.out}} 권한을 주지 못했다: {e}")));
            return;
        }
    }
    if let Err(o) = ctx.check_secrets(step) {
        rec.result = o.verdict.as_str().into();
        rec.outcome = Some(o);
        return;
    }
    // Everything that needs `${…}` expansion, while `ctx` is only borrowed.
    let prepared: Result<(BTreeMap<String, String>, PathBuf, Vec<String>, PathBuf, Vec<String>), String> = (|| {
        let lookup = ctx.lookup(&sid, &out_dir);
        let expand = |s: &str| cimon_recipe::vars::expand(s, &lookup);
        let env = ctx.step_env(step, &out_dir)?;
        let mut argv: Vec<String> = step.def.run.iter().map(|a| expand(a)).collect::<Result<_, _>>()?;
        for (pname, pdef) in &ctx.resolved.params {
            if pdef.applies_to.as_ref().is_some_and(|a| a.contains(&sid)) {
                if let (Some(args), Some(v)) = (&pdef.args, ctx.plan.plan.params.get(pname)) {
                    let val = param_string(v);
                    for a in args {
                        argv.push(a.replace("${value}", &val));
                    }
                }
            }
        }
        let cwd = match &step.def.cwd {
            None => ctx.ws.clone(),
            Some(c) => {
                let c = expand(c)?;
                let p = PathBuf::from(&c);
                let p = if p.is_absolute() { p } else { ctx.ws.join(p) };
                if !cimon_core::layout::is_within(&ctx.ws, &p) {
                    return Err(format!("cwd {c} leaves the work tree"));
                }
                p
            }
        };
        let path_dirs: Vec<PathBuf> = env.get("PATH").map(|p| p.split(';').map(PathBuf::from).collect()).unwrap_or_default();
        let pathext = env.get("PATHEXT").cloned().unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
        let (program, args) = match step.def.shell.as_deref() {
            Some("bash") => {
                let p = cimon_runner::resolve_program("bash", &cwd, &path_dirs, &pathext).map_err(|e| format!("shell = \"bash\": {e} (declare git_bash in [toolchains])"))?;
                (p, argv.clone())
            }
            Some("powershell") => {
                let p = cimon_runner::resolve_program("powershell", &cwd, &path_dirs, &pathext)?;
                let mut a = vec!["-NoProfile".to_string(), "-NonInteractive".into(), "-ExecutionPolicy".into(), "Bypass".into(), "-File".into()];
                a.extend(argv.clone());
                (p, a)
            }
            _ => {
                let first = argv.first().cloned().ok_or("run is empty")?;
                (cimon_runner::resolve_program(&first, &cwd, &path_dirs, &pathext)?, argv[1..].to_vec())
            }
        };
        let reports: Vec<String> = step
            .def
            .judge
            .as_ref()
            .and_then(|j| j.reports.as_ref())
            .map(|r| r.to_vec())
            .unwrap_or_default()
            .iter()
            .map(|g| expand(g).map(|e| relative_to_ws(&ctx.ws, &e)))
            .collect::<Result<_, _>>()?;
        Ok((env, cwd, args, program, reports))
    })();
    let (env, cwd, args, program, reports) = match prepared {
        Ok(p) => p,
        Err(e) => return fail_recipe(rec, e),
    };
    // pre-clean of requested artifacts and junit reports (design §12.2)
    let preamble = match crate::artifacts::pre_clean(ctx, step) {
        Ok(p) => p,
        Err(o) => {
            rec.result = o.verdict.as_str().into();
            rec.outcome = Some(o);
            return;
        }
    };
    let mut patterns = exec::default_error_patterns();
    for p in &step.def.error_patterns {
        if let Ok(re) = regex::Regex::new(p) {
            patterns.push(re);
        }
    }
    let remaining = ctx.deadline.saturating_duration_since(Instant::now());
    let step_timeout = Duration::from_secs(step.timeout_min() as u64 * 60);
    let job_limited = remaining < step_timeout;
    let spec = ExecSpec {
        program,
        args,
        cwd,
        env: env.clone(),
        timeout: step_timeout.min(remaining.max(Duration::from_secs(1))),
        log_path: app.layout.step_log(id, &sid),
        log_cap: app.config.disk.log_max_mb_per_step * 1024 * 1024,
        error_patterns: patterns,
        fail_on_output: step.def.fail_on_output.clone(),
        capture_prefixes: vec!["test result:".into()],
        preamble,
    };
    // status ticker
    let done = Arc::new(AtomicBool::new(false));
    let ticker = {
        let done = done.clone();
        let a = app.clone();
        let label = label.to_string();
        std::thread::spawn(move || {
            let start = Instant::now();
            while !done.load(Ordering::SeqCst) {
                let _ = a.store.set_status(id, &format!("{label} · {}", dur(start.elapsed().as_secs_f64())));
                for _ in 0..50 {
                    if done.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })
    };
    let res = exec::execute(&spec, &ctx.account, ctx.cancel);
    done.store(true, Ordering::SeqCst);
    let _ = ticker.join();
    rec.exit_code = res.exit_code;
    rec.seconds = res.seconds;
    rec.excerpt = res.first_error.clone().or_else(|| res.tail.last().cloned());
    if res.truncated {
        ctx.warnings.push(Warning::new("log", format!("{sid}: 로그가 {} MB 를 넘어 가운데를 잘랐다", app.config.disk.log_max_mb_per_step)));
    }
    if res.leftover > 0 {
        ctx.warnings.push(Warning::new("process", format!("{sid}: 단계가 끝난 뒤 남은 프로세스 {}개를 종료했다", res.leftover)));
    }
    if res.cancelled {
        rec.result = "inconclusive".into();
        rec.outcome = Some(Outcome::inconclusive(FailureClass::Infra, "cancelled"));
        return;
    }
    // judge
    let judgement = match cimon_judge::precheck(&res, step.timeout_min()) {
        Some(mut o) => {
            if res.timed_out && job_limited {
                o.reason = format!("작업 시간 제한 {}분을 넘김", ctx.resolved.project.max_job_minutes);
            }
            cimon_judge::StepJudgement { outcome: o, tests: None, metrics: None }
        }
        None => {
            let j = step.def.judge.clone();
            match j.as_ref().map(|j| j.type_.as_str()).unwrap_or("exit") {
                "cargo-test" => cimon_judge::judge_cargo(&res, j.as_ref().and_then(|j| j.min_tests).unwrap_or(1)),
                "junit" => cimon_judge::judge_junit(&res, &ctx.ws, &reports, j.as_ref().and_then(|j| j.min_tests).unwrap_or(1)),
                "script" => run_script_judge(ctx, step, &res, &out_dir, &env),
                _ => cimon_judge::judge_exit(&res),
            }
        }
    };
    rec.outcome = Some(judgement.outcome.clone());
    rec.result = judgement.outcome.verdict.as_str().into();
    let mut extra = json!({ "log": format!("{}/{sid}", job_label(id)) });
    if let Some(t) = &judgement.tests {
        extra["tests"] = json!(t);
    }
    if let Some(m) = &judgement.metrics {
        extra["metrics"] = m.clone();
    }
    if step.def.uncounted {
        extra["uncounted"] = json!(true);
    }
    rec.extra = extra;
    // artifacts
    if rec.result == "pass" {
        if let Err(o) = crate::artifacts::collect(ctx, step) {
            rec.result = o.verdict.as_str().into();
            rec.outcome = Some(o);
        }
    }
}

fn run_script_judge(ctx: &mut JobCtx<'_>, step: &cimon_recipe::Step, res: &cimon_runner::ExecResult, out_dir: &Path, env: &BTreeMap<String, String>) -> cimon_judge::StepJudgement {
    let sid = step.id().to_string();
    let lookup = ctx.lookup(&sid, out_dir);
    let run: Vec<String> = step.def.judge.as_ref().and_then(|j| j.run.clone()).unwrap_or_default();
    let argv: Result<Vec<String>, String> = run.iter().map(|a| cimon_recipe::vars::expand(a, &lookup)).collect();
    let broken = |m: String| cimon_judge::StepJudgement { outcome: Outcome::inconclusive(FailureClass::Recipe, format!("judge script: {m}")), tests: None, metrics: None };
    let argv = match argv {
        Ok(a) if !a.is_empty() => a,
        Ok(_) => return broken("run is empty".into()),
        Err(e) => return broken(e),
    };
    let mut env = env.clone();
    cimon_runner::env_set(&mut env, "CIMON_STEP_EXIT", res.exit_code.map(|c| c.to_string()).unwrap_or_default());
    cimon_runner::env_set(&mut env, "CIMON_STEP_OUT", out_dir.to_string_lossy().into());
    let path_dirs: Vec<PathBuf> = env.get("PATH").map(|p| p.split(';').map(PathBuf::from).collect()).unwrap_or_default();
    let pathext = env.get("PATHEXT").cloned().unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
    let program = match cimon_runner::resolve_program(&argv[0], &ctx.ws, &path_dirs, &pathext) {
        Ok(p) => p,
        Err(e) => return broken(e),
    };
    let spec = ExecSpec {
        program,
        args: argv[1..].to_vec(),
        cwd: ctx.ws.clone(),
        env,
        timeout: Duration::from_secs(300),
        log_path: out_dir.parent().unwrap_or(out_dir).join("judge.txt"),
        log_cap: 10 << 20,
        error_patterns: exec::default_error_patterns(),
        fail_on_output: vec![],
        capture_prefixes: vec!["{".into()],
        preamble: vec![],
    };
    let jr = exec::execute(&spec, &ctx.account, ctx.cancel);
    cimon_judge::judge_script_output(&jr, &jr.captured)
}

/// Why a setup step is skipped (`when`), or None to run it (design §11.3).
fn setup_skip(ctx: &JobCtx<'_>, step: &cimon_recipe::Step, slot: Option<i64>, recreated: bool) -> Option<String> {
    let when = step.def.when.as_deref().unwrap_or("always");
    if when == "always" {
        return None;
    }
    let slot = slot?;
    if recreated || ctx.clean {
        return None;
    }
    let state = ctx.app.store.warm_state(&ctx.job.project_key, slot, step.id()).ok().flatten()?;
    if state.def_sha256.as_deref() != Some(setup_fingerprint(ctx, step).as_str()) || state.toolchains_hash.as_deref() != Some(ctx.tool_env.hash.as_str()) {
        return None;
    }
    if when == "changed" {
        // inputs that match nothing would hash the same forever — run instead
        let h = inputs_hash(ctx, step)?;
        if state.inputs_sha256.as_deref() != Some(h.as_str()) {
            return None;
        }
    }
    Some("setup 생략: 변경 없음".into())
}

fn record_setup_ok(ctx: &mut JobCtx<'_>, step: &cimon_recipe::Step, slot: i64) {
    let inputs = if step.def.when.as_deref() == Some("changed") {
        let h = inputs_hash(ctx, step);
        if h.is_none() {
            ctx.warnings.push(Warning::new("setup", format!("{}: inputs 가 어떤 파일에도 맞지 않는다 — 매번 setup 을 돌린다", step.id())));
        }
        h
    } else {
        None
    };
    let _ = ctx.app.store.set_warm_state(
        &ctx.job.project_key,
        slot,
        &cimon_store::WarmStateRow {
            step_id: step.id().to_string(),
            last_ok_commit: Some(ctx.commit.clone()),
            inputs_sha256: inputs,
            toolchains_hash: Some(ctx.tool_env.hash.clone()),
            def_sha256: Some(setup_fingerprint(ctx, step)),
        },
    );
}

/// Everything besides the work tree that decides what a setup step does: its
/// definition, the recipe's vars and environment layers, parameter values that apply
/// to it, and sibling commits. Per-job values (`${job.id}`, temp dirs) are left out.
fn setup_fingerprint(ctx: &JobCtx<'_>, step: &cimon_recipe::Step) -> String {
    let params: BTreeMap<&String, &serde_json::Value> = ctx
        .resolved
        .params
        .iter()
        .filter(|(_, d)| d.applies_to.as_ref().is_some_and(|a| a.iter().any(|x| x == step.id())))
        .filter_map(|(n, _)| ctx.plan.plan.params.get(n).map(|v| (n, v)))
        .collect();
    // the scripts a setup runs are part of what it does: their contents (and helpers
    // they can load) go into the fingerprint, so a changed setup script runs again
    let scripts = ctx.job.recipe_commit.as_deref().and_then(|c| ctx.app.recipes.tree(c).ok()).map(|t| {
        use cimon_recipe::refs::{project_refs, ref_files, Scope};
        let mine: Vec<_> = project_refs(&ctx.resolved).into_iter().filter(|(s, _)| *s == Scope::Project || *s == Scope::Step(step.id().to_string())).collect();
        let mut files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (_, rr) in &mine {
            files.extend(ref_files(&*t, rr));
        }
        if !mine.is_empty() {
            let own = format!("projects/{}/", ctx.resolved.id);
            files.extend(cimon_recipe::Tree::files(&*t).into_iter().filter(|f| f.starts_with("scripts/") || f.starts_with(&own)).map(String::from));
        }
        t.digest(&files.into_iter().collect::<Vec<_>>())
    });
    let v = json!({
        "def": step.def,
        "scripts": scripts,
        "vars": ctx.resolved.vars,
        "env": ctx.resolved.env,
        "isolate": ctx.resolved.isolate,
        "account_env": ctx.resolved.account_env,
        "params": params,
        "siblings": ctx.plan.siblings.iter().map(|s| (&s.project, &s.commit)).collect::<Vec<_>>(),
    });
    cimon_core::sha256_hex(v.to_string().as_bytes())
}

/// Hash of the files `inputs` match (expanded like every other recipe string); None
/// when they match nothing.
fn inputs_hash(ctx: &JobCtx<'_>, step: &cimon_recipe::Step) -> Option<String> {
    let out = ctx.app.layout.step_out(ctx.job.id, step.id());
    let lookup = ctx.lookup(step.id(), &out);
    let globs: Vec<String> = step.def.inputs.iter().filter_map(|g| cimon_recipe::vars::expand(g, &lookup).ok()).map(|g| relative_to_ws(&ctx.ws, &g)).collect();
    let files = cimon_judge::find_files(&ctx.ws, &globs);
    if files.is_empty() {
        return None;
    }
    let mut h = String::new();
    for f in files {
        let rel = f.strip_prefix(&ctx.ws).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        let data = std::fs::read(&f).unwrap_or_default();
        h.push_str(&format!("{rel}:{}\n", cimon_core::sha256_hex(&data)));
    }
    Some(cimon_core::sha256_hex(h.as_bytes()))
}

/// Globs are matched relative to the work tree; `${ws}/x` becomes `x`.
pub fn relative_to_ws(ws: &Path, g: &str) -> String {
    let wsn = ws.to_string_lossy().replace('\\', "/");
    let gn = g.replace('\\', "/");
    gn.strip_prefix(&format!("{wsn}/")).map(str::to_string).unwrap_or(gn)
}

impl JobCtx<'_> {
    /// `${…}` lookup for a step (recipe-spec §13).
    pub fn lookup<'b>(&'b self, _step: &str, out_dir: &'b Path) -> impl Fn(&str) -> Option<String> + 'b {
        move |name: &str| -> Option<String> {
            let (head, rest) = name.split_once('.').unwrap_or((name, ""));
            match head {
                "ws" => Some(self.ws.to_string_lossy().into()),
                "recipes" => Some(self.recipes_dir.to_string_lossy().into()),
                "vars" => self.resolved.vars.get(rest).cloned(),
                "param" => self.plan.plan.params.get(rest).map(param_string),
                "job" => match rest {
                    "id" => Some(job_label(self.job.id)),
                    "commit" => Some(self.commit.clone()),
                    "commit_short" => Some(self.commit.chars().take(10).collect()),
                    _ => None,
                },
                "step" if rest == "out" => Some(out_dir.to_string_lossy().into()),
                "artifact" => self.artifacts.get(rest).map(|p| p.to_string_lossy().into()),
                "toolchain" => {
                    let (kind, field) = rest.split_once('.')?;
                    self.tool_env.field(kind, field)
                }
                "sibling" => {
                    let (p, field) = rest.split_once('.')?;
                    match field {
                        "root" => self.siblings.get(p).map(|d| d.to_string_lossy().into()),
                        "commit" => self.plan.siblings.iter().find(|s| s.project == p).map(|s| s.commit.clone()),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
    }

    /// [`Self::lookup`] plus `${secret.<name>}` — for environment values only (design §11.6).
    pub fn env_lookup<'b>(&'b self, step: &str, out_dir: &'b Path) -> impl Fn(&str) -> Option<String> + 'b {
        let lookup = self.lookup(step, out_dir);
        move |name: &str| match name.strip_prefix("secret.") {
            Some(s) => self.secrets.as_ref().ok().and_then(|m| m.get(s).cloned()),
            None => lookup(name),
        }
    }

    /// Every secret `step`'s environment names is stored: run_one checks before it
    /// expands, so a missing one reads as the admin's to fix rather than an unknown
    /// variable.
    pub fn check_secrets(&self, step: &cimon_recipe::Step) -> Result<(), Outcome> {
        let needs = crate::build_secrets::step_needs(&self.resolved, step, self.clean);
        if needs.is_empty() {
            return Ok(());
        }
        let have = self.secrets.as_ref().map_err(|e| Outcome::inconclusive(FailureClass::Environment, format!("비밀을 읽지 못했다: {e}")))?;
        match needs.iter().find(|n| !have.contains_key(*n)) {
            Some(n) => Err(Outcome::inconclusive(FailureClass::Environment, crate::build_secrets::missing(&self.job.project, n))),
            None => Ok(()),
        }
    }

    /// The step environment, layered as design §9.4 lists.
    pub fn step_env(&self, step: &cimon_recipe::Step, out_dir: &Path) -> Result<BTreeMap<String, String>, String> {
        let app = self.app;
        let id = self.job.id;
        let pk = &self.job.project_key;
        let mut env = cimon_runner::base_env(self.profile.as_deref(), self.build_user.as_deref());
        let tmp = app.layout.tmp_job(id);
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        cimon_runner::env_set(&mut env, "TEMP", tmp.to_string_lossy().into());
        cimon_runner::env_set(&mut env, "TMP", tmp.to_string_lossy().into());
        let mut path = self.tool_env.path.clone();
        path.extend(cimon_runner::system_path());
        cimon_runner::env_set(&mut env, "PATH", cimon_runner::join_path(&path));
        for (k, v) in &self.tool_env.exports {
            cimon_runner::env_set(&mut env, k, v.clone());
        }
        let caches = app.layout.caches(pk);
        for (k, sub) in [("CARGO_HOME", "cargo"), ("npm_config_cache", "npm"), ("UV_CACHE_DIR", "uv"), ("GRADLE_USER_HOME", "gradle"), ("NUGET_PACKAGES", "nuget")] {
            let d = caches.join(sub);
            let _ = std::fs::create_dir_all(&d);
            cimon_runner::env_set(&mut env, k, d.to_string_lossy().into());
        }
        if let Some(rh) = &self.tool_env.rustup_home {
            cimon_runner::env_set(&mut env, "RUSTUP_HOME", rh.to_string_lossy().into());
        }
        for (name, scope) in &self.resolved.isolate {
            let d = if scope == "job" { app.layout.home_job(id, name) } else { app.layout.home_project(pk, name) };
            std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
            cimon_runner::env_set(&mut env, name, d.to_string_lossy().into());
        }
        for s in &self.plan.siblings {
            if let Some(dir) = self.siblings.get(&s.project) {
                for e in &s.env {
                    cimon_runner::env_set(&mut env, e, dir.to_string_lossy().into());
                }
            }
        }
        let lookup = self.env_lookup(step.id(), out_dir);
        let ex = |v: &str| cimon_recipe::vars::expand(v, &lookup);
        for (k, v) in &self.resolved.env {
            cimon_runner::env_set(&mut env, k, ex(v)?);
        }
        if self.clean {
            for (k, v) in &self.resolved.clean_env {
                cimon_runner::env_set(&mut env, k, ex(v)?);
            }
        }
        if let Some(f) = step.def.fixture.as_ref().and_then(|f| self.resolved.fixtures.get(f)) {
            for (k, v) in &f.env {
                cimon_runner::env_set(&mut env, k, ex(v)?);
            }
        }
        for (k, v) in &step.def.env {
            cimon_runner::env_set(&mut env, k, ex(v)?);
        }
        for (pname, pdef) in &self.resolved.params {
            if pdef.applies_to.as_ref().is_some_and(|a| a.iter().any(|x| x == step.id())) {
                if let (Some(var), Some(val)) = (&pdef.env, self.plan.plan.params.get(pname)) {
                    cimon_runner::env_set(&mut env, var, param_string(val));
                }
            }
        }
        // git credentials for private dependencies (design §11.6) — after the recipe's
        // layers, which it adds to rather than replaces
        let tokens = crate::build_git::load(&app.layout)?;
        let mirrors = match self.job.recipe_commit.as_deref().map(|c| app.recipes.tree(c)) {
            Some(Ok(t)) => crate::build_git::mirrors(&app.layout, &*t),
            _ => vec![],
        };
        crate::build_git::apply(&tokens, &mirrors, &mut env);
        Ok(env)
    }
}

/// Folds step results into the job verdict and writes the final record. Returns the
/// verdict (None when cancelled).
fn finish(ctx: &JobCtx<'_>, recs: &[StepRec], cancelled: bool) -> Option<Verdict> {
    let app = ctx.app;
    let job = app.store.require_job(ctx.job.id).unwrap_or_else(|_| ctx.job.clone());
    if cancelled {
        let by = job.cancel_requested_by.clone().unwrap_or_else(|| "?".into());
        let summary = format!("{} {} 취소됨 — {by} 요청", job.project, job.ref_.clone().unwrap_or_default());
        let result = crate::result::build_with_steps(app, &job, None, None, &summary, recs, &ctx.warnings, None);
        if let Err(e) = app.store.finish_job(job.id, JobState::Cancelled, None, None, None, &format!("{by} 가 취소함"), &summary, &serde_json::to_string(&ctx.warnings).unwrap(), &result.to_string()) {
            tracing::error!("could not record the end of {}: {e}", job_label(job.id));
        }
        return None;
    }
    let judged: Vec<&StepRec> = recs.iter().filter(|r| matches!(r.result.as_str(), "pass" | "fail" | "inconclusive")).collect();
    let mut verdict = Verdict::Pass;
    for r in &judged {
        if let Some(o) = &r.outcome {
            if o.verdict.severity() > verdict.severity() {
                verdict = o.verdict;
            }
        }
    }
    // pass needs evidence: every planned step judged, every requested artifact collected
    let mut synthetic: Option<(String, Outcome)> = None;
    if verdict == Verdict::Pass {
        if let Some(r) = recs.iter().find(|r| r.kind != Kind::Setup && !matches!(r.result.as_str(), "pass" | "fail" | "inconclusive")) {
            let o = Outcome::inconclusive(FailureClass::Infra, format!("{} 가 돌지 않았다({}) — 통과로 칠 수 없다", r.id, r.result));
            verdict = Verdict::Inconclusive;
            synthetic = Some((r.id.clone(), o));
        } else if let Some(a) = ctx.plan.plan.artifacts.iter().find(|a| !ctx.artifacts.contains_key(*a)) {
            let o = Outcome::fail(FailureClass::Code, format!("요청한 산출물 {a} 이 수집되지 않았다"));
            verdict = Verdict::Fail;
            synthetic = Some((a.clone(), o));
        }
    }
    let ran_non_setup = recs.iter().any(|r| r.kind != Kind::Setup && matches!(r.result.as_str(), "pass" | "fail" | "inconclusive"));
    if verdict == Verdict::Pass && !ran_non_setup {
        verdict = Verdict::Inconclusive;
        synthetic = Some(("steps".into(), Outcome::inconclusive(FailureClass::Infra, "setup 말고 돈 단계가 없다")));
    }
    // class from the first step with the job's verdict (design §8.2)
    let first = judged.iter().find(|r| r.outcome.as_ref().is_some_and(|o| o.verdict == verdict));
    let (first_id, first_outcome, first_excerpt): (String, Option<Outcome>, Option<String>) = match (first, &synthetic) {
        (Some(r), _) => (r.id.clone(), r.outcome.clone(), r.excerpt.clone()),
        (None, Some((id, o))) => (id.clone(), Some(o.clone()), None),
        (None, None) => ("job".into(), None, None),
    };
    let class = if verdict == Verdict::Pass {
        None
    } else {
        first_outcome.as_ref().and_then(|o| o.class).or(Some(FailureClass::Infra))
    };
    let names: Vec<&str> = recs.iter().filter(|r| r.kind != Kind::Setup).map(|r| r.id.as_str()).collect();
    let short_commit: String = ctx.commit.chars().take(7).collect();
    let summary = if verdict == Verdict::Pass {
        format!(
            "{} {}({short_commit}) {} 통과 — {}. 산출물 {}개.{}{}",
            job.project,
            job.ref_.clone().unwrap_or_default(),
            job.tool,
            names.join(", "),
            ctx.artifacts.len(),
            excluded_note(&ctx.plan.plan),
            warnings_note(&ctx.warnings)
        )
    } else {
        let reason = first_outcome.as_ref().map(|o| o.reason.clone()).unwrap_or_else(|| "판정 근거 없음".into());
        format!(
            "[{}] {} {}({short_commit}) {} {} — {}: {}{}{}{}",
            class.map(|c| c.as_str()).unwrap_or("?"),
            job.project,
            job.ref_.clone().unwrap_or_default(),
            job.tool,
            verdict.as_str(),
            first_id,
            reason,
            first_excerpt.as_ref().filter(|e| !reason.contains(e.as_str())).map(|e| format!("\n첫 에러 줄: {e}")).unwrap_or_default(),
            excluded_note(&ctx.plan.plan),
            warnings_note(&ctx.warnings)
        )
    };
    let result = crate::result::build_with_steps(app, &job, Some(verdict), class, &summary, recs, &ctx.warnings, None);
    if let Err(e) = app.store.finish_job(
        job.id,
        JobState::Done,
        Some(verdict.as_str()),
        class.map(|c| c.as_str()),
        None,
        &format!("완료 — {}", verdict.as_str()),
        &summary,
        &serde_json::to_string(&ctx.warnings).unwrap(),
        &result.to_string(),
    ) {
        tracing::error!("could not record the end of {}: {e}", job_label(job.id));
    }
    crate::artifacts::write_provenance(ctx, recs, verdict);
    Some(verdict)
}

fn excluded_note(p: &cimon_recipe::select::Plan) -> String {
    if p.excluded.is_empty() {
        String::new()
    } else {
        format!(" 제외 {}.", p.excluded.len())
    }
}

fn warnings_note(w: &[Warning]) -> String {
    if w.is_empty() {
        String::new()
    } else {
        format!(" 경고 {}.", w.len())
    }
}

