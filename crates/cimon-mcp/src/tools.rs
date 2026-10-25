//! Tool definitions and dispatch. Every rejection is an `isError: true` result with a
//! code and a message the caller can act on (design §7.7).

use crate::server::CallCtx;
use cimon_engine::recipes::{self, Rejection, RunArgs};
use cimon_engine::App;
use rmcp::model::{CallToolResult, ContentBlock, DetailedTask, JsonObject, Task, Tool};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) enum ToolOutcome {
    Result(CallToolResult),
    Task(Task),
}

#[derive(Debug)]
pub(crate) struct ToolError {
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl ToolError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        ToolError { code, message: message.into(), details: None }
    }
    pub fn into_result(self) -> CallToolResult {
        let mut v = json!({ "error": self.code, "message": self.message });
        let mut text = format!("[{}] {}", self.code, self.message);
        if let Some(d) = &self.details {
            v["details"] = d.clone();
            if let Some(list) = d.as_array() {
                for item in list.iter().take(20) {
                    text.push_str(&format!("\n  {}", item.as_str().map(str::to_string).unwrap_or_else(|| item.to_string())));
                }
            }
        }
        let mut r = CallToolResult::structured_error(v);
        r.content = vec![ContentBlock::text(text)];
        r
    }
}

impl From<cimon_store::StoreError> for ToolError {
    fn from(e: cimon_store::StoreError) -> Self {
        ToolError::new("store_error", e.to_string())
    }
}

impl From<Rejection> for ToolError {
    fn from(r: Rejection) -> Self {
        ToolError { code: r.code, message: r.message, details: r.details }
    }
}

pub(crate) type ToolResult = Result<ToolOutcome, ToolError>;

fn schema(v: Value) -> Arc<JsonObject> {
    match v {
        Value::Object(m) => Arc::new(m),
        _ => unreachable!("schemas are objects"),
    }
}

fn tool(name: &'static str, description: &'static str, input: Value) -> Tool {
    Tool::new(name, description, schema(input))
}

fn run_args_schema(with_tool: bool) -> Value {
    let mut props = json!({
        "project": {"type": "string", "description": "list_projects 의 id"},
        "ref": {"type": "string", "description": "브랜치 이름 또는 커밋 SHA. 기본은 프로젝트 default_ref"},
        "recipe_ref": {"type": "string", "description": "레시피 저장소의 recipe/* 브랜치 — 시도 실행"},
        "steps": {"type": "array", "items": {"type": "string"}, "description": "단계 id. tags 와 합집합"},
        "tags": {"type": "array", "items": {"type": "string"}},
        "artifacts": {"type": "array", "items": {"type": "string"}, "description": "받을 산출물 이름. [] 면 수집 안 함"},
        "params": {"type": "object", "description": "레시피에 선언된 파라미터만"},
        "clean": {"type": "boolean", "description": "warm 트리 대신 일회 클론"}
    });
    if with_tool {
        props["tool"] = json!({"type": "string", "enum": ["build", "test", "run"]});
    }
    json!({"type": "object", "properties": props, "required": if with_tool { json!(["project", "tool"]) } else { json!(["project"]) }, "additionalProperties": false})
}

fn job_schema() -> Value {
    json!({"type": "object", "properties": {"job": {"type": "string", "description": "j000123 또는 task id"}}, "required": ["job"], "additionalProperties": false})
}

pub(crate) fn definitions(_app: &App) -> Vec<Tool> {
    let empty = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    vec![
        tool("list_projects", "등록된 프로젝트 목록(레시피 저장소 main 기준).", empty.clone()),
        tool(
            "describe_project",
            "겹친 레시피 요약 — 단계·산출물·파라미터·fixture·툴체인. recipe_ref 로 시도 브랜치 기준도.",
            json!({"type": "object", "properties": {"project": {"type": "string"}, "recipe_ref": {"type": "string"}}, "required": ["project"], "additionalProperties": false}),
        ),
        tool("plan", "build/test/run 이 무엇을 할지 실행 없이(프로젝트를 fetch 하지 않는다).", run_args_schema(true)),
        tool("build", "build·package 단계를 돌린다(task). 기본은 default = true 인 build. 끝나면 판정·요약·산출물 링크.", run_args_schema(false)),
        tool("test", "test·scenario 단계를 돌린다(task). 기본은 default = true 인 test. 필요한 빌드는 after 로 따라온다.", run_args_schema(false)),
        tool("run", "build·package·test·scenario 를 한 작업으로(task). 기본은 기본 build + 기본 test.", run_args_schema(false)),
        tool("job_status", "작업의 상태·판정·요약·경고. job 은 j000123 또는 task id.", job_schema()),
        tool(
            "job_log",
            "단계 로그의 끝 N 줄 또는 패턴 검색. step 을 빼면 단계 목록.",
            json!({"type": "object", "properties": {"job": {"type": "string"}, "step": {"type": "string"}, "tail": {"type": "integer", "minimum": 1, "maximum": 2000}, "grep": {"type": "string"}}, "required": ["job"], "additionalProperties": false}),
        ),
        tool("artifacts", "작업의 산출물 목록과 새로 발급한 다운로드 링크(1시간).", job_schema()),
        tool("cancel_job", "작업 취소 요청 — 바로 돌아오고, 정리가 끝나면 job_status 가 cancelled.", job_schema()),
        tool("queue", "접수·대기·실행 중 작업.", empty.clone()),
    ]
    .into_iter()
    .chain(crate::recipe_tools::definitions())
    .chain([
        tool(
            "recipe_show",
            "레시피 저장소의 파일 하나 또는 디렉터리 목록(ref 기준, 기본 main). 결과의 commit 을 recipe_edit 의 base 로 쓴다.",
            json!({"type": "object", "properties": {"ref": {"type": "string"}, "path": {"type": "string"}}, "additionalProperties": false}),
        ),
        tool(
            "recipe_export",
            "한 ref 의 레시피 저장소 전체(경로→내용)와 서버 cimon 버전 — 로컬 검사·편집용.",
            json!({"type": "object", "properties": {"ref": {"type": "string"}}, "additionalProperties": false}),
        ),
        tool(
            "recipe_history",
            "레시피 저장소의 커밋 이력(ref, 선택적으로 path).",
            json!({"type": "object", "properties": {"ref": {"type": "string"}, "path": {"type": "string"}, "limit": {"type": "integer", "minimum": 1, "maximum": 200}}, "additionalProperties": false}),
        ),
        tool(
            "recipe_diff",
            "두 ref 의 파일 차이(unified diff).",
            json!({"type": "object", "properties": {"from": {"type": "string"}, "to": {"type": "string"}, "path": {"type": "string"}}, "required": ["from", "to"], "additionalProperties": false}),
        ),
    ])
    .collect()
}

pub(crate) async fn call(app: &Arc<App>, ctx: &CallCtx, name: &str, args: Value) -> ToolResult {
    let app2 = app.clone();
    let user = ctx.user.clone();
    let name_owned = name.to_string();
    if matches!(name, "build" | "test" | "run") {
        let tool = cimon_core::Tool::parse(name).unwrap();
        let supports = ctx.supports_tasks;
        return tokio::task::spawn_blocking(move || start_job(&app2, &user, tool, args, supports))
            .await
            .unwrap_or_else(|e| Err(ToolError::new("internal_error", e.to_string())));
    }
    // Recipe and store work is blocking (git, SQLite); keep it off the async workers.
    tokio::task::spawn_blocking(move || call_blocking(&app2, &user, &name_owned, args))
        .await
        .unwrap_or_else(|e| Err(ToolError::new("internal_error", e.to_string())))
}

fn call_blocking(app: &Arc<App>, user: &cimon_core::AuthUser, name: &str, args: Value) -> ToolResult {
    match name {
        "list_projects" => ok_structured(recipes::list_projects(app)?),
        "describe_project" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                project: String,
                #[serde(default)]
                recipe_ref: Option<String>,
            }
            let a: A = parse(args)?;
            ok_structured(recipes::describe(app, &a.project, a.recipe_ref.as_deref())?)
        }
        "plan" => {
            let a: RunArgs = parse(args)?;
            ok_structured(recipes::plan(app, &a)?)
        }
        "recipe_show" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                #[serde(default, rename = "ref")]
                ref_: Option<String>,
                #[serde(default)]
                path: Option<String>,
            }
            let a: A = parse(args)?;
            let rref = a.ref_.unwrap_or_else(|| app.config.recipes.default_branch.clone());
            let commit = app.recipes.resolve(&rref).map_err(Rejection::from)?;
            let shown = app.recipes.show(&commit, a.path.as_deref().unwrap_or("")).map_err(Rejection::from)?;
            let mut v = json!({ "ref": rref, "commit": commit, "result": shown });
            let path = a.path.unwrap_or_default();
            v["summary"] = json!(format!("{rref} @ {} — {}", recipes::short(&commit), if path.is_empty() { "/" } else { &path }));
            with_backup_warnings(app, &mut v);
            ok_structured(v)
        }
        "recipe_export" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                #[serde(default, rename = "ref")]
                ref_: Option<String>,
            }
            let a: A = parse(args)?;
            let rref = a.ref_.unwrap_or_else(|| app.config.recipes.default_branch.clone());
            let commit = app.recipes.resolve(&rref).map_err(Rejection::from)?;
            let tree = app.recipes.tree(&commit).map_err(Rejection::from)?;
            let files: serde_json::Map<String, Value> =
                tree.files.iter().map(|(p, d)| (p.clone(), Value::String(String::from_utf8_lossy(d).into_owned()))).collect();
            let n = files.len();
            let mut v = json!({
                "ref": rref,
                "commit": commit,
                "cimon_version": env!("CARGO_PKG_VERSION"),
                "files": files,
                "summary": format!("{rref} @ {} — 파일 {n}개. 로컬 검사는 cimon {} 로", recipes::short(&commit), env!("CARGO_PKG_VERSION")),
            });
            with_backup_warnings(app, &mut v);
            ok_structured(v)
        }
        "recipe_history" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                #[serde(default, rename = "ref")]
                ref_: Option<String>,
                #[serde(default)]
                path: Option<String>,
                #[serde(default)]
                limit: Option<usize>,
            }
            let a: A = parse(args)?;
            let rref = a.ref_.unwrap_or_else(|| app.config.recipes.default_branch.clone());
            let commit = app.recipes.resolve(&rref).map_err(Rejection::from)?;
            let h = app.recipes.history(&commit, a.path.as_deref(), a.limit.unwrap_or(20)).map_err(Rejection::from)?;
            let n = h.len();
            let mut v = json!({ "ref": rref, "commits": h, "summary": format!("{rref}: 커밋 {n}개") });
            with_backup_warnings(app, &mut v);
            ok_structured(v)
        }
        "recipe_diff" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                from: String,
                to: String,
                #[serde(default)]
                path: Option<String>,
            }
            let a: A = parse(args)?;
            let from = app.recipes.resolve(&a.from).map_err(Rejection::from)?;
            let to = app.recipes.resolve(&a.to).map_err(Rejection::from)?;
            let mut diff = app.recipes.diff_text(&from, &to, a.path.as_deref()).map_err(Rejection::from)?;
            let truncated = diff.len() > 40_000;
            if truncated {
                let mut cut = 40_000;
                while !diff.is_char_boundary(cut) {
                    cut -= 1;
                }
                diff.truncate(cut);
                diff.push_str("\n… (잘림 — path 로 좁혀라)");
            }
            let mut v = json!({
                "from": from, "to": to, "diff": diff, "truncated": truncated,
                "summary": format!("{} → {}{}", recipes::short(&from), recipes::short(&to), if truncated { " (잘림)" } else { "" }),
            });
            with_backup_warnings(app, &mut v);
            ok_structured(v)
        }
        "job_status" => {
            let job = find_job(app, &args)?;
            let task = task_arg(&args);
            ok_structured(if job.state.is_terminal() && job.result_json.is_some() {
                cimon_engine::result::view(app, &job, task.as_deref())
            } else {
                cimon_engine::result::progress(app, &job)
            })
        }
        "job_log" => job_log(app, args),
        "artifacts" => {
            let job = find_job(app, &args)?;
            let list = cimon_engine::artifacts::listing(app, &job);
            let n = list.len();
            let note = if n == 0 { " (요청한 산출물이 없거나 아직 수집 전)" } else { "" };
            ok_structured(json!({
                "job_id": cimon_core::job_label(job.id),
                "artifacts": list,
                "summary": format!("{} — 산출물 {n}개{note}", cimon_core::job_label(job.id)),
                "download_hint": "Invoke-WebRequest <url> -OutFile <path>; (Get-FileHash <path>).Hash 를 sha256 과 대조",
            }))
        }
        "cancel_job" => {
            let job = find_job(app, &args)?;
            let msg = cimon_engine::jobs::cancel(app, job.id, &user.name)?;
            ok_structured(json!({ "job_id": cimon_core::job_label(job.id), "summary": msg }))
        }
        "queue" => {
            use cimon_core::JobState::*;
            let jobs = app.store.jobs_in_states(&[Running, Collecting, Cancelling, Ready, Provisioning, Intake])?;
            let list: Vec<Value> = jobs
                .iter()
                .map(|j| {
                    json!({"job_id": cimon_core::job_label(j.id), "state": j.state.as_str(), "project": j.project, "tool": j.tool,
                           "ref": j.ref_, "by": j.created_by, "status": j.status_message, "created_at": j.created_at})
                })
                .collect();
            let n = list.len();
            ok_structured(json!({ "jobs": list, "summary": format!("진행 중·대기 작업 {n}개") }))
        }
        other => match crate::recipe_tools::call(app, user, other, args) {
            Some(r) => r,
            None => Err(ToolError::new("unknown_tool", format!("unknown tool {other:?}"))),
        },
    }
}

fn task_arg(args: &Value) -> Option<String> {
    args.get("job").and_then(|v| v.as_str()).filter(|s| s.starts_with("t_")).map(str::to_string)
}

/// `job` argument: `j000123`, `123` or a task id.
fn find_job(app: &App, args: &Value) -> Result<cimon_store::JobRow, ToolError> {
    let key = args.get("job").and_then(|v| v.as_str()).ok_or_else(|| ToolError::new("invalid_param", "job is required"))?;
    let id = if key.starts_with("t_") {
        app.store.task(key)?.map(|t| t.job_id).ok_or_else(|| ToolError::new("unknown_job", format!("unknown or expired task {key}")))?
    } else {
        cimon_core::parse_job_label(key).ok_or_else(|| ToolError::new("invalid_param", format!("{key:?} is not a job id (j000123) or task id")))?
    };
    app.store
        .job(id)?
        .ok_or_else(|| ToolError::new("unknown_job", format!("{} does not exist or its record expired (records are kept 30 days)", cimon_core::job_label(id))))
}

fn job_log(app: &App, args: Value) -> ToolResult {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[allow(dead_code)]
        job: String,
        #[serde(default)]
        step: Option<String>,
        #[serde(default)]
        tail: Option<usize>,
        #[serde(default)]
        grep: Option<String>,
    }
    let a: A = parse(args.clone())?;
    let job = find_job(app, &args)?;
    let steps = app.store.steps(job.id)?;
    let Some(step) = a.step else {
        let list: Vec<Value> = steps.iter().map(|s| json!({"step": s.step_id, "result": s.result})).collect();
        return ok_structured(json!({ "job_id": cimon_core::job_label(job.id), "steps": list, "summary": "step 을 주면 그 단계의 로그를 보여 준다" }));
    };
    if !steps.iter().any(|s| s.step_id == step) {
        return Err(ToolError::new("unknown_step", format!("{} has no step {step:?}", cimon_core::job_label(job.id))));
    }
    let path = app.layout.step_log(job.id, &step);
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => {
            let expired = cimon_core::parse_time(&job.expires_at).is_some_and(|t| t < chrono::Utc::now());
            let why = if expired { format!("its log expired on {}", job.expires_at) } else { "it did not run".to_string() };
            return Err(ToolError::new("log_unavailable", format!("no log for {step} — {why}")));
        }
    };
    // a step log can be 200 MB: grep streams it, tail reads only the end
    let (selected, total): (Vec<String>, String) = match &a.grep {
        Some(pat) => {
            if pat.len() > 1000 {
                return Err(ToolError::new("invalid_param", "grep pattern is longer than 1000 characters"));
            }
            let re = regex::Regex::new(pat).map_err(|e| ToolError::new("invalid_param", format!("grep: {e}")))?;
            use std::io::BufRead;
            let mut out = vec![];
            let mut n = 0usize;
            let mut buf = vec![];
            let mut r = std::io::BufReader::new(file);
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        n += 1;
                        let line = String::from_utf8_lossy(&buf);
                        let line = line.trim_end_matches(['\r', '\n']);
                        if out.len() < 200 && re.is_match(line) {
                            out.push(format!("{n}: {line}"));
                        }
                    }
                }
            }
            (out, format!("{n}"))
        }
        None => {
            use std::io::{Read, Seek, SeekFrom};
            let n = a.tail.unwrap_or(100).min(2000);
            let mut f = file;
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            let window = (n as u64 * 2048).min(len);
            let _ = f.seek(SeekFrom::Start(len - window));
            let mut bytes = vec![];
            let _ = f.take(window).read_to_end(&mut bytes);
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let mut lines: Vec<&str> = text.lines().collect();
            if window < len && !lines.is_empty() {
                lines.remove(0); // the first line of the window is probably cut
            }
            (lines[lines.len().saturating_sub(n)..].iter().map(|s| s.to_string()).collect(), if window < len { "?".into() } else { lines.len().to_string() })
        }
    };
    let mut body = selected.join("\n");
    if body.len() > 7000 {
        let mut cut = body.len() - 7000;
        while !body.is_char_boundary(cut) {
            cut += 1;
        }
        body = format!("…\n{}", &body[cut..]);
    }
    let summary = format!("{} {step} — {total}줄 중 {}줄", cimon_core::job_label(job.id), selected.len());
    ok_structured(json!({ "job_id": cimon_core::job_label(job.id), "step": step, "lines": total, "log": body, "summary": summary }))
}

/// `build` / `test` / `run`: validate, commit, answer with a task (design §7.5).
fn start_job(app: &Arc<App>, user: &cimon_core::AuthUser, tool: cimon_core::Tool, args: Value, supports_tasks: bool) -> ToolResult {
    let a: RunArgs = parse(args)?;
    if a.tool.is_some() {
        return Err(ToolError::new("invalid_param", "tool is only for plan"));
    }
    let created = cimon_engine::jobs::create(app, &user.name, tool, a, supports_tasks)?;
    if !supports_tasks {
        let label = cimon_core::job_label(created.job_id);
        let hours = app.config.queue.max_queue_hours;
        return ok_structured(json!({
            "job_id": label,
            "done": false,
            "accepted": true,
            "summary": format!("{label} 접수 — 아직 끝나지 않았다. job_status 로 확인하라. 시작 전 작업은 {hours}시간까지 기다린다"),
        }));
    }
    let task = Task::new(created.task_id, rmcp::model::TaskStatus::Working, created.created_at.clone(), created.created_at)
        .with_status_message(created.status)
        .with_ttl_ms(cimon_engine::jobs::TTL_MS)
        .with_poll_interval_ms(cimon_engine::jobs::POLL_INTERVAL_MS);
    Ok(ToolOutcome::Task(task))
}

/// Every `recipe_*` result carries backup warnings (design §16.5).

pub(crate) fn with_backup_warnings(app: &App, v: &mut Value) {
    let w = cimon_engine::backup::warnings(app);
    if !w.is_empty() {
        v["backup_warnings"] = json!(w);
        if let Some(s) = v.get("summary").and_then(|s| s.as_str()).map(str::to_string) {
            v["summary"] = json!(format!("{s} ⚠ {}", w[0]));
        }
    }
}

pub(crate) fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::new("invalid_param", format!("bad arguments: {e}")))
}

/// A structured success result whose text block repeats `summary`.
pub(crate) fn ok_structured(value: Value) -> ToolResult {
    let summary = value.get("summary").and_then(|s| s.as_str()).map(str::to_string);
    let mut r = CallToolResult::structured(value);
    if let Some(s) = summary {
        r.content = vec![ContentBlock::text(s)];
    }
    Ok(ToolOutcome::Result(r))
}

/// `tasks/get` (design §7.5): every field present, `statusMessage` on every answer.
pub(crate) fn get_task(app: &Arc<App>, task_id: &str) -> Result<DetailedTask, ToolError> {
    use cimon_core::JobState::*;
    use rmcp::model::{TaskPayload, TaskStatus};
    let t = app.store.task(task_id)?.ok_or_else(|| ToolError::new("unknown_task", format!("unknown or expired task {task_id}")))?;
    let _ = app.store.touch_task(task_id);
    let mut job = app.store.require_job(t.job_id)?;
    // a merged job's tasks were moved; follow defensively anyway
    for _ in 0..5 {
        match job.merged_into {
            Some(into) if job.state == Merged => job = app.store.require_job(into)?,
            _ => break,
        }
    }
    let base = |status: TaskStatus, msg: String| {
        Task::new(task_id.to_string(), status, t.created_at.clone(), job.last_updated_at.clone())
            .with_status_message(msg)
            .with_ttl_ms(cimon_engine::jobs::TTL_MS)
            .with_poll_interval_ms(cimon_engine::jobs::POLL_INTERVAL_MS)
    };
    if t.detached_at.is_some() {
        let msg = t.detached_reason.clone().unwrap_or_else(|| "task 를 뗐다".into());
        return Ok(DetailedTask::new(base(TaskStatus::Cancelled, msg), TaskPayload::Cancelled));
    }
    let status_msg = job.status_message.clone().filter(|m| !m.is_empty()).unwrap_or_else(|| job.state.as_str().to_string());
    Ok(match job.state {
        Intake | Provisioning | Ready | Running | Collecting | Cancelling | Merged => DetailedTask::new(base(TaskStatus::Working, status_msg), TaskPayload::Working),
        Cancelled | QueueTimeout => DetailedTask::new(base(TaskStatus::Cancelled, status_msg), TaskPayload::Cancelled),
        Done | Interrupted => {
            let view = cimon_engine::result::view(app, &job, Some(task_id));
            let result = completion_result(&view);
            let obj = match serde_json::to_value(&result).unwrap_or_default() {
                Value::Object(m) => m,
                _ => Default::default(),
            };
            DetailedTask::new(base(TaskStatus::Completed, status_msg), TaskPayload::Completed { result: obj })
        }
    })
}

/// The `CallToolResult` a finished task carries: structured result, the summary as
/// text, artifact `resource_link`s; `isError` unless the verdict is pass.
pub(crate) fn completion_result(view: &Value) -> CallToolResult {
    let pass = view.get("verdict").and_then(|v| v.as_str()) == Some("pass");
    let summary = view.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let mut r = if pass { CallToolResult::structured(view.clone()) } else { CallToolResult::structured_error(view.clone()) };
    let mut content = vec![ContentBlock::text(summary)];
    if let Some(arts) = view.get("artifacts").and_then(|a| a.as_array()) {
        for a in arts {
            if let (Some(url), Some(name)) = (a.get("url").and_then(|u| u.as_str()), a.get("name").and_then(|n| n.as_str())) {
                let sha = a.get("sha256").and_then(|s| s.as_str()).unwrap_or("");
                if let Ok(res) = serde_json::from_value::<rmcp::model::Resource>(
                    json!({"uri": url, "name": name, "mimeType": "application/octet-stream", "description": format!("sha256 {sha}")}),
                ) {
                    content.push(ContentBlock::resource_link(res));
                }
            }
        }
    }
    r.content = content;
    r
}

pub(crate) fn cancel_task(app: &Arc<App>, user: &cimon_core::AuthUser, task_id: &str) -> Result<(), ToolError> {
    cimon_engine::jobs::detach_task(app, task_id, &user.name)?;
    Ok(())
}

