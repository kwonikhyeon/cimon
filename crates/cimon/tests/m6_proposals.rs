//! M6: recipe editing, proposals and promotion over MCP (design §6.4–6.5).

mod common;

use cimon_recipe::tree::MemTree;
use common::*;
use serde_json::{json, Value};

const RECIPE: &str = "schema = 1\n[[step]]\nid = \"build\"\nkind = \"build\"\nrun = [\"cmd\", \"/c\", \"echo\", \"build\"]\n\n[[step]]\nid = \"test\"\nkind = \"test\"\nrun = [\"cmd\", \"/c\", \"echo\", \"test\"]\nuncounted = true\n";

fn tree(project: &str) -> MemTree {
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert(
        format!("projects/{project}/project.toml"),
        format!("schema = 1\nid = \"{project}\"\nrepo = \"https://example.invalid/{project}.git\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\n"),
    );
    t.insert(format!("projects/{project}/recipe.toml"), RECIPE);
    t
}

async fn tool<S: rmcp::Service<rmcp::RoleClient>>(c: &rmcp::service::RunningService<rmcp::RoleClient, S>, name: &str, args: Value) -> (Value, bool) {
    let mut p = rmcp::model::CallToolRequestParams::new(name.to_string());
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    match c.call_tool_once(p).await.unwrap() {
        rmcp::model::CallToolResponse::Complete(r) => (r.structured_content.unwrap_or_default(), r.is_error.unwrap_or(false)),
        other => panic!("{other:?}"),
    }
}

async fn trial<S: rmcp::Service<rmcp::RoleClient>>(c: &rmcp::service::RunningService<rmcp::RoleClient, S>, args: Value) -> Value {
    let t = start_task(c, "run", args).await;
    let (_, v, _) = wait_task(c, &t, 120).await;
    v
}

fn cond<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["conditions"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap_or_else(|| panic!("no condition {name}: {v:#}"))
}

#[tokio::test]
async fn edit_trial_promote_flow() {
    repos_dir();
    make_repo("m6flow", &[("x", "1")]);
    let h = Harness::start("").await;
    let main = h.app.recipes.import_init(&tree("m6flow"), "admin-a", "init").unwrap();
    let m = h.task_client(&h.member_token).await;
    let a = h.task_client(&h.admin_token).await;

    // main is never edited directly
    let (v, err) = tool(&m, "recipe_edit", json!({"branch": "main", "base": main, "message": "x", "changes": [{"path": "a", "content": "b"}]})).await;
    assert!(err);
    assert_eq!(v["error"], "main_not_editable");
    // a typo is refused with file:line
    let (v, err) = tool(&m, "recipe_edit", json!({"branch": "recipe/typo", "base": main, "message": "typo", "changes": [{"path": "projects/m6flow/recipe.toml", "content": RECIPE.replace("uncounted", "uncountd")}]})).await;
    assert!(err);
    assert_eq!(v["error"], "recipe_invalid");
    assert!(v["details"][0].as_str().unwrap().contains("recipe.toml:"), "{v}");

    // a multi-file edit: new step + notes, one commit
    let new_recipe = format!("{RECIPE}\n[[step]]\nid = \"test2\"\nkind = \"test\"\nrun = [\"cmd\", \"/c\", \"echo\", \"t2\"]\nuncounted = true\n");
    let (v, err) = tool(
        &m,
        "recipe_edit",
        json!({"branch": "recipe/add-test2", "base": main, "message": "test2 추가", "changes": [
            {"path": "projects/m6flow/recipe.toml", "content": new_recipe},
            {"path": "docs/projects/m6flow.md", "content": "test2 를 더했다"}
        ]}),
    )
    .await;
    assert!(!err, "{v}");
    let head = v["commit"].as_str().unwrap().to_string();
    let proposal = v["proposal"].as_str().unwrap().to_string();
    assert_eq!(v["proposal_created"], true);
    assert_eq!(h.app.recipes.history(&head, None, 1).unwrap()[0].author, "member-b");
    // stale base
    let (v, err) = tool(&m, "recipe_edit", json!({"branch": "recipe/add-test2", "base": main, "message": "again", "changes": [{"path": "a.txt", "content": "x"}]})).await;
    assert!(err);
    assert_eq!(v["error"], "recipe_conflict");

    // conditions: no trial yet
    let (v, _) = tool(&a, "recipe_proposal", json!({"proposal": proposal})).await;
    assert_eq!(cond(&v, "static_check")["state"], "pass");
    assert_eq!(cond(&v, "fast_forward")["state"], "pass");
    assert_eq!(cond(&v, "trial")["state"], "fail", "{v:#}");
    assert!(v["semantic_diff"]["projects"][0]["changed_steps"].as_array().unwrap().contains(&json!("test2")));
    assert!(v["risks"].as_array().unwrap().contains(&json!("실행")));

    // a trial that does not include the changed step does not count
    let t = trial(&m, json!({"project": "m6flow", "recipe_ref": "recipe/add-test2", "steps": ["build"]})).await;
    assert_eq!(t["trial"], true);
    let (v, _) = tool(&a, "recipe_proposal", json!({"proposal": proposal})).await;
    assert_eq!(cond(&v, "trial")["state"], "fail");
    // members cannot promote
    let (v, err) = tool(&m, "recipe_promote", json!({"proposal": proposal, "head": head})).await;
    assert!(err);
    assert_eq!(v["error"], "forbidden");
    // admin without trial and without reason → refused
    let (v, err) = tool(&a, "recipe_promote", json!({"proposal": proposal, "head": head})).await;
    assert!(err);
    assert_eq!(v["error"], "promote_conditions_unmet", "{v}");

    // the right trial
    let t = trial(&m, json!({"project": "m6flow", "recipe_ref": "recipe/add-test2", "steps": ["test2"]})).await;
    assert_eq!(t["verdict"], "pass", "{t:#}");
    let (v, _) = tool(&a, "recipe_proposal", json!({"proposal": proposal})).await;
    assert_eq!(cond(&v, "trial")["state"], "pass", "{v:#}");
    // promoting a head the admin did not look at is refused
    let (v, err) = tool(&a, "recipe_promote", json!({"proposal": proposal, "head": main})).await;
    assert!(err);
    assert_eq!(v["error"], "recipe_conflict");
    let (v, err) = tool(&a, "recipe_promote", json!({"proposal": proposal, "head": head})).await;
    assert!(!err, "{v}");
    assert_eq!(v["main"], head.as_str());
    assert_eq!(h.app.recipes.main_commit().unwrap(), head);
    assert!(v["override_reason"].is_null());
    assert!(h.app.recipes.branch_head("recipe/add-test2").is_none());
    // promoted: closed for further changes, the name is reusable
    let (v, err) = tool(&a, "recipe_promote", json!({"proposal": proposal, "head": head})).await;
    assert!(err);
    assert_eq!(v["error"], "proposal_closed");
    // a backup was written on promote
    assert!(!h.app.store.backups("recipes").unwrap().is_empty());
    m.cancel().await.unwrap();
    a.cancel().await.unwrap();
}

#[tokio::test]
async fn not_fast_forward_sync_override_untestable_close_revert() {
    repos_dir();
    make_repo("m6ff", &[("x", "1")]);
    let h = Harness::start("").await;
    let main = h.app.recipes.import_init(&tree("m6ff"), "admin-a", "init").unwrap();
    let m = h.task_client(&h.member_token).await;
    let a = h.task_client(&h.admin_token).await;
    let (p1, _) = tool(&m, "recipe_edit", json!({"branch": "recipe/one", "base": main, "message": "one", "changes": [{"path": "docs/one.md", "content": "1"}]})).await;
    let (p2, _) = tool(&m, "recipe_edit", json!({"branch": "recipe/two", "base": main, "message": "two", "changes": [{"path": "projects/m6ff/recipe.toml", "content": RECIPE.replace("echo\", \"test", "echo\", \"test-two")}]})).await;
    // docs-only change: no affected project, trial not applicable
    let (v, _) = tool(&a, "recipe_proposal", json!({"proposal": p1["proposal"]})).await;
    assert_eq!(cond(&v, "trial")["state"], "na", "{v:#}");
    let (v, err) = tool(&a, "recipe_promote", json!({"proposal": p1["proposal"], "head": p1["commit"]})).await;
    assert!(!err, "{v}");
    // two is behind main now
    let (v, err) = tool(&a, "recipe_promote", json!({"proposal": p2["proposal"], "head": p2["commit"], "override_reason": "급하게 반영해야 한다 — 시도는 뒤에"})).await;
    assert!(err);
    assert_eq!(v["error"], "not_fast_forward", "{v}");
    let (v, err) = tool(&m, "recipe_sync", json!({"branch": "recipe/two", "head": p2["commit"]})).await;
    assert!(!err, "{v}");
    assert_eq!(v["result"], "merged");
    let synced = v["head"].as_str().unwrap().to_string();
    // the new head has no trial; an override reason is recorded
    let (v, err) = tool(&a, "recipe_promote", json!({"proposal": p2["proposal"], "head": synced, "override_reason": "short"})).await;
    assert!(err, "a short reason is not enough: {v}");
    let (v, err) = tool(&a, "recipe_promote", json!({"proposal": p2["proposal"], "head": synced, "override_reason": "출력 문구만 바꿨다 — 동작 변화 없음"})).await;
    assert!(!err, "{v}");
    assert_eq!(v["override_reason"], "출력 문구만 바꿨다 — 동작 변화 없음");
    let id = cimon_core::parse_proposal_label(p2["proposal"].as_str().unwrap()).unwrap();
    assert_eq!(h.app.store.proposal(id).unwrap().unwrap().override_reason.as_deref(), Some("출력 문구만 바꿨다 — 동작 변화 없음"));

    // project.toml changes cannot be tried: a reason is required even with a trial
    let main = h.app.recipes.main_commit().unwrap();
    let (p3, err) = tool(&m, "recipe_edit", json!({"branch": "recipe/refs", "base": main, "message": "refs", "changes": [{"path": "projects/m6ff/project.toml", "content": "schema = 1\nid = \"m6ff\"\nrepo = \"https://example.invalid/m6ff.git\"\nallowed_refs = [\"main\", \"release/*\"]\ndefault_ref = \"main\"\n"}]})).await;
    assert!(!err, "{p3}");
    let (v, _) = tool(&a, "recipe_proposal", json!({"proposal": p3["proposal"]})).await;
    assert_eq!(cond(&v, "untestable_changes")["state"], "fail", "{v:#}");
    assert!(v["risks"].as_array().unwrap().contains(&json!("권한")), "{v:#}");
    // close by the author; the branch name is free again
    let (v, err) = tool(&m, "recipe_close", json!({"proposal": p3["proposal"], "reason": "not now"})).await;
    assert!(!err, "{v}");
    let (v, err) = tool(&m, "recipe_comment", json!({"proposal": p3["proposal"], "body": "닫힌 뒤 댓글"})).await;
    assert!(!err, "{v}");
    let (v, err) = tool(&m, "recipe_edit", json!({"branch": "recipe/refs", "base": main, "message": "again", "changes": [{"path": "docs/x.md", "content": "x"}]})).await;
    assert!(!err, "{v}");
    assert_ne!(v["proposal"], p3["proposal"]);
    let (list, _) = tool(&m, "recipe_proposals", json!({"status": "all"})).await;
    assert!(list["proposals"].as_array().unwrap().len() >= 4);

    // revert needs admin and a reason
    let target = h.app.recipes.main_commit().unwrap();
    let (v, err) = tool(&m, "recipe_revert", json!({"commit": target, "reason": "member cannot"})).await;
    assert!(err);
    assert_eq!(v["error"], "forbidden");
    let (v, err) = tool(&a, "recipe_revert", json!({"commit": target, "reason": "test-two 출력이 로그 파서를 깨뜨렸다"})).await;
    assert!(!err, "{v}");
    let h1 = h.app.recipes.history("main", None, 1).unwrap();
    assert!(h1[0].subject.starts_with("되돌리기"));
    m.cancel().await.unwrap();
    a.cancel().await.unwrap();
}
