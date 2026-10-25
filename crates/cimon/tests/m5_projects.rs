//! M5: project builds — setup `when` and its invalidation, siblings (beside/isolated),
//! fail_on_output as the negative control, warm slots surviving branch deletion + GC.

mod common;

use cimon_recipe::tree::MemTree;
use common::*;
use rmcp::model::TaskStatus;
use serde_json::json;

fn project_toml(id: &str, siblings: &str) -> String {
    format!("schema = 1\nid = \"{id}\"\nrepo = \"https://example.invalid/{id}.git\"\nallowed_refs = [\"main\", \"feature/**\"]\ndefault_ref = \"main\"\n{siblings}")
}

async fn run<S: rmcp::Service<rmcp::RoleClient>>(c: &rmcp::service::RunningService<rmcp::RoleClient, S>, tool: &str, args: serde_json::Value) -> serde_json::Value {
    let t = start_task(c, tool, args).await;
    let (_, v, _) = wait_task(c, &t, 120).await;
    v
}

fn step<'a>(v: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    v["steps"].as_array().unwrap().iter().find(|s| s["id"] == id).unwrap_or_else(|| panic!("no step {id} in {v:#}"))
}

#[tokio::test]
async fn setup_when_changed_and_invalidation() {
    repos_dir();
    let repo = make_repo("m5setup", &[("deps.lock", "v1"), ("build.cmd", "@echo build\r\n")]);
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert("projects/m5setup/project.toml", project_toml("m5setup", ""));
    t.insert(
        "projects/m5setup/recipe.toml",
        r#"schema = 1
[[step]]
id = "deps"
kind = "setup"
run = ["cmd", "/c", "echo installing deps"]
when = "changed"
inputs = ["deps.lock"]

[[step]]
id = "build"
kind = "build"
run = ["./build.cmd"]

[[step]]
id = "slow-setup-test"
kind = "test"
run = ["cmd", "/c", "echo t"]
uncounted = true
default = false
"#,
    );
    let h = Harness::start("").await;
    h.app.recipes.import_init(&t, "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let v = run(&c, "build", json!({"project": "m5setup"})).await;
    assert_eq!(step(&v, "deps")["result"], "pass", "{v:#}");
    let v = run(&c, "build", json!({"project": "m5setup"})).await;
    assert_eq!(step(&v, "deps")["result"], "skipped", "{v:#}");
    assert_eq!(step(&v, "deps")["reason"], "setup 생략: 변경 없음");
    // inputs change → runs again
    write_files(&repo, &[("deps.lock", "v2")]);
    git(&repo, &["commit", "-qam", "deps"]);
    let v = run(&c, "build", json!({"project": "m5setup"})).await;
    assert_eq!(step(&v, "deps")["result"], "pass", "{v:#}");
    // a cancelled job clears the slot's setup state → next job starts cold
    let job = cimon_core::parse_job_label(v["job_id"].as_str().unwrap()).unwrap();
    let slot = h.app.store.require_job(job).unwrap().workspace_slot.unwrap();
    assert!(h.app.store.warm_state("m5setup", slot, "deps").unwrap().is_some());
    h.app.store.clear_warm_state("m5setup", slot).unwrap(); // what cancel/interrupt/failure do
    let v = run(&c, "build", json!({"project": "m5setup"})).await;
    assert_eq!(step(&v, "deps")["result"], "pass", "cold after invalidation: {v:#}");
    // clean jobs always run setup and never touch warm state
    let v = run(&c, "build", json!({"project": "m5setup", "clean": true})).await;
    assert_eq!(step(&v, "deps")["result"], "pass");
    assert_eq!(v["workspace"]["mode"], "clean");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn siblings_beside_and_isolated_and_negative_control() {
    repos_dir();
    make_repo("m5lib", &[("VERSION", "lib-1\r\n")]);
    make_repo("m5other", &[("VERSION", "other-1\r\n")]);
    // reads the sibling by relative path (beside) and through the env var (isolated)
    let check = "@echo off\r\nif not exist ..\\m5lib\\VERSION ( echo SKIP (no sibling m5lib & exit /b 0 )\r\ntype ..\\m5lib\\VERSION\r\ntype \"%OTHER_ROOT%\\VERSION\"\r\necho test result: ok. 1 passed; 0 failed; 0 ignored\r\n";
    let repo = make_repo("m5app", &[("check.cmd", check)]);
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert("projects/m5lib/project.toml", project_toml("m5lib", ""));
    t.insert("projects/m5lib/recipe.toml", "schema = 1\n[[step]]\nid = \"b\"\nkind = \"build\"\nrun = [\"cmd\", \"/c\", \"echo\"]\n");
    t.insert("projects/m5other/project.toml", project_toml("m5other", ""));
    t.insert("projects/m5other/recipe.toml", "schema = 1\n[[step]]\nid = \"b\"\nkind = \"build\"\nrun = [\"cmd\", \"/c\", \"echo\"]\n");
    t.insert(
        "projects/m5app/project.toml",
        project_toml(
            "m5app",
            "[[sibling]]\nproject = \"m5lib\"\nref = \"main\"\nplace = \"beside\"\nenv = \"LIB_ROOT\"\n\n[[sibling]]\nproject = \"m5other\"\nref = \"main\"\nenv = [\"OTHER_ROOT\", \"OTHER_DIR\"]\n",
        ),
    );
    t.insert(
        "projects/m5app/recipe.toml",
        "schema = 1\n[env]\nLIB_COMMIT = \"${sibling.m5lib.commit}\"\n[[step]]\nid = \"check\"\nkind = \"test\"\nrun = [\"./check.cmd\"]\nfail_on_output = [\"SKIP (no sibling\"]\njudge = { type = \"cargo-test\" }\n",
    );
    let h = Harness::start("").await;
    h.app.recipes.import_init(&t, "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let v = run(&c, "test", json!({"project": "m5app"})).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
    let sibs: Vec<&str> = v["siblings"].as_array().unwrap().iter().map(|s| s["project"].as_str().unwrap()).collect();
    assert_eq!(sibs, vec!["m5lib", "m5other"]);
    let (log, _) = call(&h.client(&h.member_token).await, "job_log", json!({"job": v["job_id"], "step": "check"})).await;
    let text = log["log"].as_str().unwrap();
    assert!(text.contains("lib-1") && text.contains("other-1"), "{text}");
    // negative control: a run where the script reports a skipped sibling must fail
    branch_with(&repo, "feature/skip", &[("check.cmd", "@echo off\r\necho SKIP (no sibling m5lib\r\necho test result: ok. 1 passed; 0 failed; 0 ignored\r\n")]);
    let v = run(&c, "test", json!({"project": "m5app", "ref": "feature/skip"})).await;
    assert_eq!(v["verdict"], "fail", "{v:#}");
    assert!(v["summary"].as_str().unwrap().contains("fail_on_output"), "{}", v["summary"]);
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn warm_slot_survives_deleted_branch_and_gc() {
    repos_dir();
    let repo = make_repo("m5gc", &[("build.cmd", "@echo build\r\n")]);
    branch_with(&repo, "feature/gone", &[("only-here.txt", "x"), ("build.cmd", "@echo build gone\r\n")]);
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert("projects/m5gc/project.toml", project_toml("m5gc", ""));
    t.insert("projects/m5gc/recipe.toml", "schema = 1\n[[step]]\nid = \"build\"\nkind = \"build\"\nrun = [\"./build.cmd\"]\n");
    let h = Harness::start("").await;
    h.app.recipes.import_init(&t, "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let v = run(&c, "build", json!({"project": "m5gc", "ref": "feature/gone"})).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
    // the branch disappears upstream; fetch prunes it; GC runs
    git(&repo, &["branch", "-D", "feature/gone"]);
    let mirror = cimon_workspace::Mirror::fetch(&h.app.layout.mirror("m5gc"), &format!("https://example.invalid/m5gc.git")).unwrap();
    assert!(mirror.branch("feature/gone").is_none());
    mirror.gc().unwrap();
    // the slot still works for the next job, without being recreated
    let v = run(&c, "build", json!({"project": "m5gc"})).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
    let warnings = v["warnings"].as_array().unwrap();
    assert!(!warnings.iter().any(|w| w["source"] == "workspace"), "slot was recreated: {v:#}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn cancel_running_job_clears_setup_state() {
    repos_dir();
    make_repo("m5cancel", &[("build.cmd", "@echo build\r\n")]);
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert("projects/m5cancel/project.toml", project_toml("m5cancel", ""));
    t.insert(
        "projects/m5cancel/recipe.toml",
        "schema = 1\n[[step]]\nid = \"deps\"\nkind = \"setup\"\nrun = [\"cmd\", \"/c\", \"echo deps\"]\nwhen = \"cold\"\n[[step]]\nid = \"build\"\nkind = \"build\"\nrun = [\"cmd\", \"/c\", \"ping -n 30 127.0.0.1 >NUL\"]\n",
    );
    let h = Harness::start("").await;
    h.app.recipes.import_init(&t, "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let task = start_task(&c, "build", json!({"project": "m5cancel"})).await;
    // wait until the long step runs, then cancel
    let mut job = String::new();
    for _ in 0..200 {
        let (s, _) = call(&h.client(&h.member_token).await, "job_status", json!({"job": task})).await;
        job = s["job_id"].as_str().unwrap_or_default().to_string();
        if s["summary"].as_str().unwrap_or("").starts_with("build") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (r, err) = call(&h.client(&h.admin_token).await, "cancel_job", json!({"job": job})).await;
    assert!(!err, "{r}");
    let (st, _, _) = wait_task(&c, &task, 30).await;
    assert_eq!(st, TaskStatus::Cancelled);
    let id = cimon_core::parse_job_label(&job).unwrap();
    let row = h.app.store.require_job(id).unwrap();
    assert_eq!(row.state, cimon_core::JobState::Cancelled);
    assert!(h.app.store.warm_state("m5cancel", row.workspace_slot.unwrap(), "deps").unwrap().is_none(), "setup state must be cleared after cancel");
    c.cancel().await.unwrap();
}
