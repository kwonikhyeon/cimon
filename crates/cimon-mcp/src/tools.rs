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
    ]
}

pub(crate) async fn call(app: &Arc<App>, ctx: &CallCtx, name: &str, args: Value) -> ToolResult {
    let app2 = app.clone();
    let user = ctx.user.clone();
    let name_owned = name.to_string();
    // Recipe and store work is blocking (git, SQLite); keep it off the async workers.
    tokio::task::spawn_blocking(move || call_blocking(&app2, &user, &name_owned, args))
        .await
        .unwrap_or_else(|e| Err(ToolError::new("internal_error", e.to_string())))
}

fn call_blocking(app: &Arc<App>, _user: &cimon_core::AuthUser, name: &str, args: Value) -> ToolResult {
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
        other => Err(ToolError::new("unknown_tool", format!("unknown tool {other:?}"))),
    }
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

pub(crate) fn get_task(_app: &Arc<App>, task_id: &str) -> Result<DetailedTask, ToolError> {
    Err(ToolError::new("unknown_task", format!("unknown or expired task {task_id}")))
}

pub(crate) fn cancel_task(_app: &Arc<App>, _user: &cimon_core::AuthUser, task_id: &str) -> Result<(), ToolError> {
    Err(ToolError::new("unknown_task", format!("unknown or expired task {task_id}")))
}
