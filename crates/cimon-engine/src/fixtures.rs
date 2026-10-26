//! Fixtures (recipe-spec §11) and `[account_env]` (design §9.2).

use crate::run::{JobCtx, StepRec};
use cimon_core::{FailureClass, Outcome, Warning};
use cimon_recipe::model::{FixtureCmd, FixtureDef, Poll};
use cimon_recipe::select::Plan;
use cimon_recipe::Resolved;
use cimon_runner::{exec, ExecSpec};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
enum State {
    Up,
    Failed(Outcome),
    Down,
}

pub struct Fixtures {
    defs: BTreeMap<String, FixtureDef>,
    /// fixture → step ids that use it, in plan order
    users: BTreeMap<String, Vec<String>>,
    state: BTreeMap<String, State>,
}

impl Fixtures {
    pub fn new(r: &Resolved, plan: &Plan) -> Fixtures {
        let mut users: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for s in &plan.steps {
            if let Some(f) = r.step(&s.id).and_then(|x| x.def.fixture.clone()) {
                users.entry(f).or_default().push(s.id.clone());
            }
        }
        Fixtures { defs: r.fixtures.clone(), users, state: BTreeMap::new() }
    }

    /// Brings a fixture up before its first step. A failed `up` fails every step that
    /// uses it with the same outcome.
    pub fn ensure_up(&mut self, ctx: &mut JobCtx<'_>, name: &str) -> Result<(), Outcome> {
        match self.state.get(name) {
            Some(State::Up) => return Ok(()),
            Some(State::Failed(o)) => return Err(o.clone()),
            _ => {}
        }
        let def = self.defs.get(name).cloned().ok_or_else(|| Outcome::fail(FailureClass::Recipe, format!("fixture {name} does not exist")))?;
        let _ = ctx.app.store.set_status(ctx.job.id, &format!("fixture {name} 올리는 중"));
        let _ = ctx.app.store.set_fixture_state(ctx.job.id, Some(&format!("{name}: up")));
        let code_class = def.on_up_failure.as_deref() == Some("code");
        let up_fail = |msg: String| if code_class { Outcome::fail(FailureClass::Code, msg) } else { Outcome::inconclusive(FailureClass::Infra, msg) };
        for (i, cmd) in def.up.iter().enumerate() {
            let r = run_cmd(ctx, name, &format!("up-{i}"), cmd, &def);
            let ok = r.as_ref().map(|x| x.exit_code == Some(0) && x.spawn_error.is_none() && !x.timed_out).unwrap_or(false);
            if !ok {
                let why = match r {
                    Ok(x) => x.spawn_error.clone().or(x.first_error.clone()).unwrap_or_else(|| format!("exit {:?}{}", x.exit_code, if x.timed_out { " (timeout)" } else { "" })),
                    Err(e) => e,
                };
                let o = up_fail(format!("fixture {name}: up failed — {why}"));
                self.state.insert(name.to_string(), State::Failed(o.clone()));
                self.teardown(ctx, name);
                return Err(o);
            }
        }
        if let Some(ready) = &def.ready {
            if let Err(o) = self.wait_ready(ctx, name, ready, &def) {
                self.state.insert(name.to_string(), State::Failed(o.clone()));
                self.teardown(ctx, name);
                return Err(o);
            }
        }
        self.state.insert(name.to_string(), State::Up);
        let _ = ctx.app.store.set_fixture_state(ctx.job.id, Some(&format!("{name}: ready")));
        Ok(())
    }

    fn wait_ready(&self, ctx: &mut JobCtx<'_>, name: &str, poll: &Poll, def: &FixtureDef) -> Result<(), Outcome> {
        let every = Duration::from_secs(poll.every_s.unwrap_or(3).max(1) as u64);
        let deadline = Instant::now() + Duration::from_secs(poll.timeout_s.unwrap_or(120) as u64);
        loop {
            if let Ok(r) = run_poll(ctx, name, "ready", &poll.run, def) {
                if until_met(&poll.until, &r) {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline || ctx.cancel.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(every);
        }
        // why: a component that exited during start-up is the branch's fault
        if let Some(ot) = &poll.on_timeout {
            if let Ok(r) = run_poll(ctx, name, "on-timeout", &ot.run, def) {
                if let Some(path) = &ot.exited_path {
                    let out = r.captured.join("\n");
                    let names = jsonpath_strings(&out, path);
                    if !names.is_empty() {
                        return Err(Outcome::fail(FailureClass::Code, format!("fixture {name}: {} exited while starting", names.join(", "))));
                    }
                }
            }
        }
        Err(Outcome::inconclusive(FailureClass::Infra, format!("fixture {name}: not ready within {}s", poll.timeout_s.unwrap_or(120))))
    }

    /// After a step finishes: when it was the fixture's last user, tear it down.
    pub fn step_done(&mut self, ctx: &mut JobCtx<'_>, step: &str, recs: &mut [StepRec]) {
        let _ = recs;
        let names: Vec<String> = self.users.iter().filter(|(_, u)| u.last().map(|s| s.as_str()) == Some(step)).map(|(n, _)| n.clone()).collect();
        for n in names {
            if matches!(self.state.get(&n), Some(State::Up)) {
                self.teardown(ctx, &n);
            }
        }
    }

    pub fn teardown_all(&mut self, ctx: &mut JobCtx<'_>, _recs: &mut [StepRec]) {
        let names: Vec<String> = self.state.iter().filter(|(_, s)| matches!(s, State::Up)).map(|(n, _)| n.clone()).collect();
        for n in names {
            self.teardown(ctx, &n);
        }
    }

    /// collect → down (always) → gone check (design §11).
    fn teardown(&mut self, ctx: &mut JobCtx<'_>, name: &str) {
        let Some(def) = self.defs.get(name).cloned() else { return };
        let _ = ctx.app.store.set_status(ctx.job.id, &format!("fixture {name} 내리는 중"));
        // evidence first: the next job's git clean would delete it
        let dest = ctx.app.layout.job_logs(ctx.job.id).join(format!("fixture-{name}"));
        let lookup_dir = ctx.app.layout.step_out(ctx.job.id, &format!("fixture-{name}"));
        let globs: Vec<String> = {
            let l = ctx.lookup(name, &lookup_dir);
            def.collect.iter().filter_map(|g| cimon_recipe::vars::expand(g, &l).ok()).map(|g| crate::run::relative_to_ws(&ctx.ws, &g)).collect()
        };
        if !globs.is_empty() {
            let files = cimon_judge::find_files(&ctx.ws, &globs);
            for f in files.iter().take(500) {
                let rel = f.strip_prefix(&ctx.ws).unwrap_or(f).to_path_buf();
                let to = dest.join(rel);
                if let Some(p) = to.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                // the work tree is build-writable: never follow a link out of it
                if let Ok(mut src) = cimon_runner::open_within(&ctx.ws, f) {
                    if let Ok(mut dst) = std::fs::File::create(&to) {
                        let _ = std::io::copy(&mut src, &mut dst);
                    }
                }
            }
        }
        for (i, cmd) in def.down.iter().enumerate() {
            match run_cmd(ctx, name, &format!("down-{i}"), cmd, &def) {
                Ok(r) if r.exit_code == Some(0) && r.spawn_error.is_none() && !r.timed_out => {}
                Ok(r) => {
                    let why = r.spawn_error.clone().unwrap_or_else(|| format!("exit {:?}{}", r.exit_code, if r.timed_out { " (timeout)" } else { "" }));
                    ctx.warnings.push(Warning::new("fixture", format!("{name}: down-{i} 명령 실패 — {why}")));
                }
                Err(e) => ctx.warnings.push(Warning::new("fixture", format!("{name}: down-{i} 명령을 띄우지 못했다 — {e}"))),
            }
        }
        if let Some(gone) = &def.gone {
            let deadline = Instant::now() + Duration::from_secs(gone.timeout_s.unwrap_or(60) as u64);
            let every = Duration::from_secs(gone.every_s.unwrap_or(3).max(1) as u64);
            let mut ok = false;
            loop {
                if let Ok(r) = run_poll(ctx, name, "gone", &gone.run, &def) {
                    if until_met(&gone.until, &r) {
                        ok = true;
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(every);
            }
            if !ok {
                ctx.warnings.push(Warning::new("fixture", format!("{name}: down 뒤에도 남아 있다(fixture 잔류) — 작업 끝 정리가 마무리한다")));
            }
        }
        // a failed fixture stays failed: later steps that use it get the same outcome
        if !matches!(self.state.get(name), Some(State::Failed(_))) {
            self.state.insert(name.to_string(), State::Down);
        }
        let _ = ctx.app.store.set_fixture_state(ctx.job.id, None);
    }
}

fn until_met(u: &cimon_recipe::model::Until, r: &cimon_runner::ExecResult) -> bool {
    if let Some(e) = u.exit {
        return r.exit_code == Some(e as i64);
    }
    if let Some(e) = u.exit_not {
        return r.exit_code.is_some() && r.exit_code != Some(e as i64);
    }
    if let Some(j) = &u.json_all {
        let out = r.captured.join("\n");
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) else { return false };
        let Ok(path) = serde_json_path::JsonPath::parse(&j.path) else { return false };
        let nodes = path.query(&v).all();
        let want = serde_json::to_value(&j.equals).unwrap_or(serde_json::Value::Null);
        return !nodes.is_empty() && nodes.iter().all(|n| **n == want);
    }
    false
}

fn jsonpath_strings(out: &str, path: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(out.trim()) else { return vec![] };
    let Ok(p) = serde_json_path::JsonPath::parse(path) else { return vec![] };
    p.query(&v).all().into_iter().map(|n| n.as_str().map(str::to_string).unwrap_or_else(|| n.to_string())).collect()
}

fn fixture_env(ctx: &JobCtx<'_>, def: &FixtureDef, out_dir: &std::path::Path) -> Result<BTreeMap<String, String>, String> {
    let mut env = cimon_runner::base_env(ctx.profile.as_deref(), ctx.build_user.as_deref());
    let tmp = ctx.app.layout.tmp_job(ctx.job.id);
    let _ = std::fs::create_dir_all(&tmp);
    cimon_runner::env_set(&mut env, "TEMP", tmp.to_string_lossy().into());
    cimon_runner::env_set(&mut env, "TMP", tmp.to_string_lossy().into());
    let mut path = ctx.tool_env.path.clone();
    path.extend(cimon_runner::system_path());
    cimon_runner::env_set(&mut env, "PATH", cimon_runner::join_path(&path));
    for (k, v) in &ctx.tool_env.exports {
        cimon_runner::env_set(&mut env, k, v.clone());
    }
    for (name, scope) in &ctx.resolved.isolate {
        let d = if scope == "job" || ctx.private_caches() { ctx.app.layout.home_job(ctx.job.id, name) } else { ctx.app.layout.home_project(&ctx.job.project_key, name) };
        let _ = std::fs::create_dir_all(&d);
        cimon_runner::env_set(&mut env, name, d.to_string_lossy().into());
    }
    let lookup = ctx.env_lookup("fixture", out_dir);
    for (k, v) in &ctx.resolved.env {
        cimon_runner::env_set(&mut env, k, cimon_recipe::vars::expand(v, &lookup)?);
    }
    for (k, v) in &def.env {
        cimon_runner::env_set(&mut env, k, cimon_recipe::vars::expand(v, &lookup)?);
    }
    Ok(env)
}

fn run_argv(ctx: &JobCtx<'_>, name: &str, what: &str, argv: &[String], timeout: Duration, def: &FixtureDef) -> Result<cimon_runner::ExecResult, String> {
    // why: teardown must finish even after a cancel, or the fixture outlives the job
    let cleanup = what.starts_with("down") || what == "gone";
    let never = AtomicBool::new(false);
    let cancel = if cleanup { &never } else { ctx.cancel };
    let out_dir = ctx.app.layout.step_out(ctx.job.id, &format!("fixture-{name}"));
    let _ = std::fs::create_dir_all(&out_dir);
    if let Some(user) = &ctx.build_user {
        // teardown must run even if the grant fails — the fixture would outlive the job
        if let Err(e) = cimon_runner::grant_modify(&out_dir, user) {
            if !cleanup {
                return Err(e);
            }
        }
    }
    let lookup = ctx.lookup("fixture", &out_dir);
    let argv: Vec<String> = argv.iter().map(|a| cimon_recipe::vars::expand(a, &lookup)).collect::<Result<_, _>>()?;
    let env = fixture_env(ctx, def, &out_dir)?;
    let path: Vec<PathBuf> = env.get("PATH").map(|p| p.split(';').map(PathBuf::from).collect()).unwrap_or_default();
    let program = cimon_runner::resolve_program(argv.first().ok_or("empty command")?, &ctx.ws, &path, env.get("PATHEXT").map(|s| s.as_str()).unwrap_or(".EXE;.BAT;.CMD"))?;
    let spec = ExecSpec {
        program,
        args: argv[1..].to_vec(),
        cwd: ctx.ws.clone(),
        env,
        timeout,
        log_path: ctx.app.layout.step_log_dir(ctx.job.id, &format!("fixture-{name}")).join(format!("{what}.txt")),
        log_cap: 50 << 20,
        error_patterns: exec::default_error_patterns(),
        fail_on_output: vec![],
        capture_prefixes: vec![String::new()],
        preamble: vec![],
        redact: ctx.redactions(),
    };
    Ok(exec::execute(&spec, &ctx.account, cancel))
}

fn run_cmd(ctx: &JobCtx<'_>, name: &str, what: &str, cmd: &FixtureCmd, def: &FixtureDef) -> Result<cimon_runner::ExecResult, String> {
    run_argv(ctx, name, what, &cmd.run, Duration::from_secs(cmd.timeout_min.unwrap_or(5) as u64 * 60), def)
}

fn run_poll(ctx: &JobCtx<'_>, name: &str, what: &str, argv: &[String], def: &FixtureDef) -> Result<cimon_runner::ExecResult, String> {
    run_argv(ctx, name, what, argv, Duration::from_secs(60), def)
}

/// Writes (`set = true`) or clears the build account's user environment for
/// `[account_env]`. Single-account mode skips it with a warning — writing the
/// developer's own HKCU would leak into their session.
pub fn account_env(ctx: &mut JobCtx<'_>, set: bool) {
    if ctx.resolved.account_env.is_empty() {
        return;
    }
    let Some(acct) = ctx.app.build_account() else {
        if set {
            ctx.warnings.push(Warning::new("account_env", "단일 계정 모드라 [account_env] 를 쓰지 않았다 — 예약 작업으로 뜨는 프로세스는 작업 홈을 보지 못한다"));
        }
        return;
    };
    let mut args = vec!["__account-env".to_string(), if set { "set".into() } else { "clear".into() }];
    for (name, scope) in &ctx.resolved.account_env {
        let d = if scope == "job" || ctx.private_caches() { ctx.app.layout.home_job(ctx.job.id, name) } else { ctx.app.layout.home_project(&ctx.job.project_key, name) };
        let _ = std::fs::create_dir_all(&d);
        args.push(if set { format!("{name}={}", d.display()) } else { name.clone() });
    }
    let spec = ExecSpec {
        program: acct.helper.clone(),
        args,
        cwd: ctx.app.layout.root.join("tmp"),
        env: cimon_runner::base_env(Some(&acct.profile_dir), Some(&acct.name)),
        timeout: Duration::from_secs(60),
        log_path: ctx.app.layout.step_log_dir(ctx.job.id, "_account_env").join(if set { "set.txt" } else { "clear.txt" }),
        log_cap: 1 << 20,
        error_patterns: vec![],
        fail_on_output: vec![],
        capture_prefixes: vec![],
        preamble: vec![],
        redact: vec![],
    };
    let r = exec::execute(&spec, &acct.account(), &AtomicBool::new(false));
    if r.exit_code != Some(0) {
        ctx.warnings.push(Warning::new("account_env", format!("빌드 계정 환경 변수를 {}하지 못했다(exit {:?})", if set { "쓰" } else { "지우" }, r.exit_code)));
    }
}
