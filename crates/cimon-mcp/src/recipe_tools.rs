//! Recipe editing, proposals and promotion (design §6.4–6.5).

use crate::tools::{ok_structured, parse, with_backup_warnings, ToolError, ToolResult};
use cimon_core::{proposal_label, AuthUser};
use cimon_engine::proposals;
use cimon_engine::recipes::Rejection;
use cimon_engine::App;
use rmcp::model::Tool;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

fn tool(name: &'static str, description: &'static str, input: Value) -> Tool {
    let Value::Object(m) = input else { unreachable!() };
    Tool::new(name, description, Arc::new(m))
}

pub(crate) fn definitions() -> Vec<Tool> {
    let prop = json!({"type": "string", "description": "p0012 또는 recipe/<설명>"});
    vec![
        tool(
            "recipe_edit",
            "recipe/* 브랜치에 여러 파일의 변경을 커밋 하나로. base 는 읽은 커밋(새 브랜치면 main 의 현재 커밋). 저장 전 정적 검사 — 새로 만든 오류가 있으면 저장하지 않는다. 새 브랜치면 제안을 연다.",
            json!({"type": "object", "properties": {
                "branch": {"type": "string"},
                "base": {"type": "string"},
                "message": {"type": "string"},
                "changes": {"type": "array", "items": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}, "delete": {"type": "boolean"}}, "required": ["path"], "additionalProperties": false}}
            }, "required": ["branch", "base", "message", "changes"], "additionalProperties": false}),
        ),
        tool(
            "recipe_proposals",
            "제안 목록(브랜치·상태·작성자·head·반영 조건 요약) — 시도 브랜치 목록을 겸한다.",
            json!({"type": "object", "properties": {"status": {"type": "string", "enum": ["open", "promoted", "closed", "all"]}}, "additionalProperties": false}),
        ),
        tool(
            "recipe_proposal",
            "제안 하나: 의미 차이(위험 표시 포함), 반영 조건 상태, 시도 실행, 댓글. 리뷰 화면이 생기기 전 관리자의 리뷰 수단.",
            json!({"type": "object", "properties": {"proposal": prop}, "required": ["proposal"], "additionalProperties": false}),
        ),
        tool(
            "recipe_propose",
            "제안의 제목·설명을 쓴다(작성자·관리자).",
            json!({"type": "object", "properties": {"proposal": prop, "title": {"type": "string"}, "description": {"type": "string"}}, "required": ["proposal"], "additionalProperties": false}),
        ),
        tool(
            "recipe_comment",
            "제안에 댓글.",
            json!({"type": "object", "properties": {"proposal": prop, "body": {"type": "string"}}, "required": ["proposal", "body"], "additionalProperties": false}),
        ),
        tool(
            "recipe_close",
            "제안 닫기(작성자·관리자). 커밋은 refs/cimon/closed 에 30일 남고, 같은 브랜치 이름은 다시 쓸 수 있다.",
            json!({"type": "object", "properties": {"proposal": prop, "reason": {"type": "string"}}, "required": ["proposal"], "additionalProperties": false}),
        ),
        tool(
            "recipe_sync",
            "main 을 그 브랜치에 합친다. 충돌이면 충돌 파일마다 base·main·브랜치 내용을 돌려준다 — 해결한 내용을 resolutions 로 다시 보내면 병합 커밋을 만든다. 새 head 이므로 시도 실행을 다시 돌려야 한다.",
            json!({"type": "object", "properties": {"branch": {"type": "string"}, "head": {"type": "string"}, "resolutions": {"type": "object", "additionalProperties": {"type": "string"}}}, "required": ["branch", "head"], "additionalProperties": false}),
        ),
        tool(
            "recipe_promote",
            "제안을 main 에 반영(관리자). 본 head 를 함께 보낸다. fast-forward 만. 조건을 반영 순간 다시 계산한다. 시도 실행 조건·시험할 수 없는 변경은 override_reason 으로만 덮어쓴다.",
            json!({"type": "object", "properties": {"proposal": prop, "head": {"type": "string"}, "override_reason": {"type": "string"}}, "required": ["proposal", "head"], "additionalProperties": false}),
        ),
        tool(
            "recipe_revert",
            "main 의 커밋 하나를 되돌린다(관리자, 급한 복구). 사유 필수, 이력에 영구히 남는다.",
            json!({"type": "object", "properties": {"commit": {"type": "string"}, "reason": {"type": "string"}}, "required": ["commit", "reason"], "additionalProperties": false}),
        ),
    ]
}

fn admin(user: &AuthUser, what: &str) -> Result<(), ToolError> {
    if user.is_admin() {
        Ok(())
    } else {
        Err(ToolError::new("forbidden", format!("{what} is admin-only — ask an admin")))
    }
}

fn audit(app: &App, user: &AuthUser, action: &str, proposal: Option<i64>, commit: Option<&str>, detail: Option<String>) {
    let _ = app.store.audit(cimon_store::AuditEntry { user: &user.name, action, proposal_id: proposal, recipe_commit: commit, detail, ..Default::default() });
}

pub(crate) fn call(app: &Arc<App>, user: &AuthUser, name: &str, args: Value) -> Option<ToolResult> {
    Some(match name {
        "recipe_edit" => edit(app, user, args),
        "recipe_proposals" => list(app, args),
        "recipe_proposal" => (|| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                proposal: String,
            }
            let a: A = parse(args)?;
            let p = proposals::find(app, &a.proposal)?;
            let mut v = proposals::view(app, &p);
            with_backup_warnings(app, &mut v);
            ok_structured(v)
        })(),
        "recipe_propose" => (|| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                proposal: String,
                #[serde(default)]
                title: Option<String>,
                #[serde(default)]
                description: Option<String>,
            }
            let a: A = parse(args)?;
            let p = proposals::find(app, &a.proposal)?;
            if p.author != user.name && !user.is_admin() {
                return Err(ToolError::new("forbidden", "only the author or an admin edits a proposal's title and description"));
            }
            if p.status != "open" {
                return Err(ToolError::new("proposal_closed", format!("{} is {}", proposal_label(p.id), p.status)));
            }
            app.store.update_proposal_text(p.id, a.title.as_deref(), a.description.as_deref())?;
            let p = app.store.proposal(p.id)?.unwrap();
            ok_structured(proposals::view(app, &p))
        })(),
        "recipe_comment" => (|| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                proposal: String,
                body: String,
            }
            let a: A = parse(args)?;
            if a.body.trim().is_empty() || a.body.len() > 20_000 {
                return Err(ToolError::new("invalid_param", "comment body must be 1–20000 characters"));
            }
            let p = proposals::find(app, &a.proposal)?;
            let head = app.recipes.branch_head(&p.branch).or(p.promoted_commit.clone()).unwrap_or_default();
            app.store.add_comment(p.id, &user.name, &a.body, &head)?;
            audit(app, user, "comment", Some(p.id), None, None);
            ok_structured(json!({ "proposal": proposal_label(p.id), "summary": format!("{} 에 댓글을 달았다", proposal_label(p.id)) }))
        })(),
        "recipe_close" => (|| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                proposal: String,
                #[serde(default)]
                reason: Option<String>,
            }
            let a: A = parse(args)?;
            let p = proposals::find(app, &a.proposal)?;
            if p.author != user.name && !user.is_admin() {
                return Err(ToolError::new("forbidden", "only the author or an admin closes a proposal"));
            }
            app.recipes.close(&app.store, &user.name, p.id, a.reason.as_deref()).map_err(Rejection::from)?;
            audit(app, user, "proposal_close", Some(p.id), None, a.reason.clone());
            let mut v = json!({ "proposal": proposal_label(p.id), "summary": format!("{} 를 닫았다 — 커밋은 refs/cimon/closed/{} 에 30일 남는다", proposal_label(p.id), proposal_label(p.id)) });
            with_backup_warnings(app, &mut v);
            ok_structured(v)
        })(),
        "recipe_sync" => sync(app, user, args),
        "recipe_promote" => promote(app, user, args),
        "recipe_revert" => (|| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                commit: String,
                reason: String,
            }
            let a: A = parse(args)?;
            admin(user, "recipe_revert")?;
            let new = app.recipes.revert(&user.name, &a.commit, &a.reason).map_err(Rejection::from)?;
            audit(app, user, "recipe_revert", None, Some(&new), Some(format!("reverted {} — {}", a.commit, a.reason)));
            let backup = cimon_engine::backup::backup_recipes(app, "revert");
            let mut v = json!({ "commit": new, "summary": format!("main 에 되돌리는 커밋 {} 를 만들었다 — 다음 작업부터 쓰인다", proposals::short(&new)) });
            if let Err(e) = backup {
                v["backup_error"] = json!(e);
            }
            with_backup_warnings(app, &mut v);
            ok_structured(v)
        })(),
        _ => return None,
    })
}

fn edit(app: &Arc<App>, user: &AuthUser, args: Value) -> ToolResult {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        branch: String,
        base: String,
        message: String,
        changes: Vec<cimon_recipestore::Change>,
    }
    let a: A = parse(args)?;
    if a.changes.len() > 500 {
        return Err(ToolError::new("invalid_param", "at most 500 changes per commit"));
    }
    let r = app.recipes.edit(&app.store, &user.name, &a.branch, &a.base, &a.message, &a.changes).map_err(Rejection::from)?;
    audit(app, user, "recipe_edit", Some(r.proposal_id), Some(&r.commit), Some(format!("{} file(s) on {}", a.changes.len(), a.branch)));
    let mut v = json!({
        "branch": r.branch,
        "commit": r.commit,
        "proposal": proposal_label(r.proposal_id),
        "proposal_created": r.proposal_created,
        "warnings": r.warnings.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
        "summary": format!(
            "{} @ {} 저장{} — 제안 {}. 시도 실행: run(project=…, recipe_ref=\"{}\")",
            a.branch,
            proposals::short(&r.commit),
            if r.warnings.is_empty() { String::new() } else { format!(" (경고 {})", r.warnings.len()) },
            proposal_label(r.proposal_id),
            a.branch
        ),
    });
    if app.config.web.enabled {
        v["url"] = json!(format!("{}/ui/proposals/{}", app.config.server.public_url.trim_end_matches('/'), proposal_label(r.proposal_id)));
    }
    with_backup_warnings(app, &mut v);
    ok_structured(v)
}

fn list(app: &Arc<App>, args: Value) -> ToolResult {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default)]
        status: Option<String>,
    }
    let a: A = parse(args)?;
    let status = match a.status.as_deref() {
        None => Some("open"),
        Some("all") => None,
        Some(s) => Some(s),
    };
    let rows = app.store.proposals(status, 200)?;
    let list: Vec<Value> = rows
        .iter()
        .map(|p| {
            let head = app.recipes.branch_head(&p.branch);
            let cond = head.as_ref().filter(|_| p.status == "open").and_then(|h| proposals::conditions(app, p, h).ok());
            json!({
                "proposal": proposal_label(p.id),
                "branch": p.branch,
                "title": p.title,
                "author": p.author,
                "status": p.status,
                "head": head,
                "updated_at": p.updated_at,
                "conditions": cond.map(|c| c.list.iter().map(|x| format!("{}={}", x.name, x.state)).collect::<Vec<_>>().join(", ")),
            })
        })
        .collect();
    let n = list.len();
    let mut v = json!({ "proposals": list, "summary": format!("제안 {n}개({})", a.status.as_deref().unwrap_or("open")) });
    with_backup_warnings(app, &mut v);
    ok_structured(v)
}

fn sync(app: &Arc<App>, user: &AuthUser, args: Value) -> ToolResult {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        branch: String,
        head: String,
        #[serde(default)]
        resolutions: HashMap<String, String>,
    }
    let a: A = parse(args)?;
    let out = app.recipes.sync(&app.store, &user.name, &a.branch, &a.head, &a.resolutions).map_err(Rejection::from)?;
    let mut v = match out {
        cimon_recipestore::SyncOutcome::UpToDate { head } => json!({ "result": "up_to_date", "head": head, "summary": format!("{} 은 이미 main 을 포함한다", a.branch) }),
        cimon_recipestore::SyncOutcome::Merged { commit } => {
            audit(app, user, "recipe_sync", None, Some(&commit), Some(a.branch.clone()));
            json!({ "result": "merged", "head": commit, "summary": format!("main 을 {} 에 합쳤다 → {} — 새 head 이므로 시도 실행을 다시 돌려라", a.branch, proposals::short(&commit)) })
        }
        cimon_recipestore::SyncOutcome::Conflicts { files } => {
            let n = files.len();
            json!({ "result": "conflicts", "files": files, "summary": format!("충돌 {n}개 — 파일마다 base·main·branch 내용을 보고 해결한 내용을 resolutions 로 다시 보내라") })
        }
    };
    with_backup_warnings(app, &mut v);
    ok_structured(v)
}

fn promote(app: &Arc<App>, user: &AuthUser, args: Value) -> ToolResult {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        proposal: String,
        head: String,
        #[serde(default)]
        override_reason: Option<String>,
    }
    let a: A = parse(args)?;
    admin(user, "recipe_promote")?;
    let p = proposals::find(app, &a.proposal)?;
    let reason = a.override_reason.clone().filter(|r| !r.trim().is_empty());
    let mut used_override = false;
    let promoted = app
        .recipes
        .promote(&app.store, &user.name, p.id, &a.head, reason.as_deref(), |_main, head| {
            let c = proposals::conditions(app, &p, head).map_err(|e| cimon_recipestore::RsError::new("promote_conditions_unmet", format!("could not compute conditions: {e}")))?;
            let hard = c.hard_failures();
            if !hard.is_empty() {
                let code = if hard.iter().any(|x| x.name == "fast_forward") { "not_fast_forward" } else { "promote_conditions_unmet" };
                return Err(cimon_recipestore::RsError::new(code, hard.iter().map(|x| format!("{}: {}", x.name, x.detail)).collect::<Vec<_>>().join("; ")));
            }
            let soft = c.soft_failures();
            if !soft.is_empty() {
                match &reason {
                    Some(r) if r.trim().chars().count() >= 10 => used_override = true,
                    _ => {
                        return Err(cimon_recipestore::RsError::new(
                            "promote_conditions_unmet",
                            format!(
                                "{} — run the trials, or give override_reason (10+ characters, recorded permanently)",
                                soft.iter().map(|x| format!("{}: {}", x.name, x.detail)).collect::<Vec<_>>().join("; ")
                            ),
                        ))
                    }
                }
            }
            Ok(())
        })
        .map_err(Rejection::from)?;
    if !used_override {
        // promoted without needing it: do not record a reason that overrode nothing
        if reason.is_some() {
            let _ = app.store.tx(|tx| {
                tx.execute("UPDATE proposals SET override_reason = NULL WHERE id = ?1", [p.id])?;
                Ok(())
            });
        }
    }
    audit(app, user, "recipe_promote", Some(p.id), Some(&promoted), if used_override { reason.clone() } else { None });
    let backup = cimon_engine::backup::backup_recipes(app, "promote");
    let mut v = json!({
        "proposal": proposal_label(p.id),
        "main": promoted,
        "override_reason": if used_override { reason.clone() } else { None },
        "summary": format!(
            "{} 를 main 에 반영했다({}){} — 다음 작업부터 쓰인다",
            proposal_label(p.id),
            proposals::short(&promoted),
            if used_override { " — 조건을 사유와 함께 덮어씀" } else { "" }
        ),
    });
    if let Err(e) = backup {
        v["backup_error"] = json!(e);
    }
    with_backup_warnings(app, &mut v);
    ok_structured(v)
}
