//! Tool definitions and dispatch. Every rejection is an `isError: true` result with a
//! code and a message the caller can act on (design §7.7).

use crate::server::CallCtx;
use cimon_engine::App;
use rmcp::model::{CallToolResult, ContentBlock, DetailedTask, JsonObject, Task, Tool};
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
}

impl ToolError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        ToolError { code, message: message.into() }
    }
    pub fn into_result(self) -> CallToolResult {
        let mut r = CallToolResult::structured_error(json!({ "error": self.code, "message": self.message }));
        r.content = vec![ContentBlock::text(format!("[{}] {}", self.code, self.message))];
        r
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

pub(crate) fn definitions(_app: &App) -> Vec<Tool> {
    vec![tool(
        "list_projects",
        "등록된 프로젝트 목록(레시피 저장소 main 기준).",
        json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    )]
}

pub(crate) async fn call(app: &Arc<App>, ctx: &CallCtx, name: &str, args: Value) -> ToolResult {
    let _ = (ctx, &args);
    match name {
        "list_projects" => list_projects(app),
        other => Err(ToolError::new("unknown_tool", format!("unknown tool {other:?}"))),
    }
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

fn list_projects(_app: &Arc<App>) -> ToolResult {
    ok_structured(json!({ "projects": [], "summary": "레시피 저장소가 아직 없다 — 관리자가 `cimon recipe import --init` 을 실행해야 한다" }))
}

pub(crate) fn get_task(_app: &Arc<App>, task_id: &str) -> Result<DetailedTask, ToolError> {
    Err(ToolError::new("unknown_task", format!("unknown or expired task {task_id}")))
}

pub(crate) fn cancel_task(_app: &Arc<App>, _user: &cimon_core::AuthUser, task_id: &str) -> Result<(), ToolError> {
    Err(ToolError::new("unknown_task", format!("unknown or expired task {task_id}")))
}
