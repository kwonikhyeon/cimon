//! M2: jobs end to end — tasks over MCP, intake (fetch, pin, refs), the runner (Job
//! Object, judges, first error line), merging identical requests, task-level and
//! job-level cancellation, callers without the Tasks extension.

mod common;

use cimon_recipe::tree::MemTree;
use common::*;
use rmcp::model::TaskStatus;
use serde_json::json;

const BUILD_CMD: &str = "@echo off\r\nif not exist out mkdir out\r\necho built > out\\app.txt\r\necho building ok\r\n";
const TEST_OK: &str = "@echo off\r\necho running tests\r\necho test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured\r\n";
const TEST_BROKEN: &str = "@echo off\r\necho error: assertion failed: left == right\r\necho test result: FAILED. 1 passed; 1 failed; 0 ignored\r\nexit /b 101\r\n";

pub fn recipes(project: &str) -> MemTree {
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert(
        format!("projects/{project}/project.toml"),
        format!("schema = 1\nid = \"{project}\"\nrepo = \"https://example.invalid/{project}.git\"\nallowed_refs = [\"main\", \"feature/**\"]\ndefault_ref = \"main\"\nmax_job_minutes = 10\n"),
    );
    t.insert(
        format!("projects/{project}/recipe.toml"),
        r#"schema = 1
[[step]]
id = "prepare"
kind = "setup"
run = ["cmd", "/c", "echo", "setup"]

[[step]]
id = "build"
kind = "build"
run = ["./build.cmd"]
timeout_min = 5
  [[step.artifact]]
  name = "app.txt"
  glob = "out/app.txt"
  expect = 1

[[step]]
id = "test"
kind = "test"
run = ["./test.cmd"]
after = ["build"]
judge = { type = "cargo-test", min_tests = 1 }

[[step]]
id = "slow"
kind = "test"
run = ["cmd", "/c", "ping -n 30 127.0.0.1 >NUL"]
default = false
uncounted = true
"#,
    );
    t
}

async fn setup(project: &str) -> Harness {
    repos_dir();
    let repo = make_repo(project, &[("build.cmd", BUILD_CMD), ("test.cmd", TEST_OK)]);
    branch_with(&repo, "feature/broken", &[("test.cmd", TEST_BROKEN)]);
    let h = Harness::start("").await;
    h.app.recipes.import_init(&recipes(project), "admin-a", "init").unwrap();
    h
}

#[tokio::test]
async fn run_passes_with_artifact_and_logs() {
    let h = setup("m2pass").await;
    let c = h.task_client(&h.member_token).await;
    let task = start_task(&c, "run", json!({"project": "m2pass", "artifacts": ["app.txt"]})).await;
    let (status, v, is_error) = wait_task(&c, &task, 120).await;
    assert_eq!(status, TaskStatus::Completed);
    assert!(!is_error, "{v:#}");
    assert_eq!(v["verdict"], "pass", "{v:#}");
    assert_eq!(v["task_id"], task.as_str());
    assert_eq!(v["deduplicated"], false);
    let steps: Vec<(&str, &str)> = v["steps"].as_array().unwrap().iter().map(|s| (s["id"].as_str().unwrap(), s["result"].as_str().unwrap())).collect();
    assert_eq!(steps, vec![("prepare", "pass"), ("build", "pass"), ("test", "pass")]);
    assert_eq!(v["steps"][2]["tests"]["passed"], 2);
    let art = &v["artifacts"][0];
    assert_eq!(art["name"], "app.txt");
    let url = art["url"].as_str().unwrap();
    // download through the link and check the hash
    let body = reqwest::get(url).await.unwrap();
    assert_eq!(body.status(), 200);
    let bytes = body.bytes().await.unwrap();
    assert_eq!(cimon_core::sha256_hex(&bytes), art["sha256"].as_str().unwrap());
    // a tampered link is refused
    let bad = reqwest::get(url.replace("app.txt", "other.txt")).await.unwrap();
    assert_eq!(bad.status(), 403);
    // job_log and job_status by task id
    let (log, err) = call_any(&c, "job_log", json!({"job": task, "step": "test"})).await;
    assert!(!err, "{log}");
    assert!(log["log"].as_str().unwrap().contains("test result: ok"));
    let (st, _) = call_any(&c, "job_status", json!({"job": v["job_id"]})).await;
    assert_eq!(st["verdict"], "pass");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn broken_branch_fails_with_first_error_line() {
    let h = setup("m2broken").await;
    let c = h.task_client(&h.member_token).await;
    let task = start_task(&c, "test", json!({"project": "m2broken", "ref": "feature/broken"})).await;
    let (_, v, is_error) = wait_task(&c, &task, 120).await;
    assert!(is_error);
    assert_eq!(v["verdict"], "fail", "{v:#}");
    assert_eq!(v["failure_class"], "code");
    let summary = v["summary"].as_str().unwrap();
    assert!(summary.contains("error: assertion failed"), "{summary}");
    assert!(summary.starts_with("[code]"), "{summary}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn missing_branch_is_a_request_failure() {
    let h = setup("m2missing").await;
    let c = h.task_client(&h.member_token).await;
    let task = start_task(&c, "build", json!({"project": "m2missing", "ref": "feature/nope"})).await;
    let (_, v, is_error) = wait_task(&c, &task, 60).await;
    assert!(is_error);
    assert_eq!(v["verdict"], "fail");
    assert_eq!(v["failure_class"], "request");
    assert_eq!(v["reason_code"], "ref_not_found");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn identical_requests_merge_and_task_cancel_detaches() {
    let h = setup("m2merge").await;
    let a = h.task_client(&h.member_token).await;
    let b = h.task_client(&h.admin_token).await;
    let t1 = start_task(&a, "test", json!({"project": "m2merge", "steps": ["slow"]})).await;
    // let the first one reach the runner
    for _ in 0..100 {
        let (s, _) = call_any(&a, "job_status", json!({"job": t1})).await;
        if s["state"] == "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let t2 = start_task(&b, "test", json!({"project": "m2merge", "steps": ["slow"]})).await;
    // the second follows the first
    let mut merged = false;
    for _ in 0..100 {
        let (s2, _) = call_any(&b, "job_status", json!({"job": t2})).await;
        let (s1, _) = call_any(&a, "job_status", json!({"job": t1})).await;
        if s1["job_id"] == s2["job_id"] {
            merged = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(merged, "second request did not merge into the first");
    // detaching one task leaves the job running for the other
    a.peer().cancel_task(rmcp::model::CancelTaskParams::new(t1.clone())).await.unwrap();
    let (st1, _, _) = wait_task(&a, &t1, 10).await;
    assert_eq!(st1, TaskStatus::Cancelled);
    let (s2, _) = call_any(&b, "job_status", json!({"job": t2})).await;
    assert_eq!(s2["state"], "running", "{s2}");
    // cancel_job ends it
    let (r, err) = call_any(&b, "cancel_job", json!({"job": t2})).await;
    assert!(!err, "{r}");
    let (st2, _, _) = wait_task(&b, &t2, 30).await;
    assert_eq!(st2, TaskStatus::Cancelled);
    a.cancel().await.unwrap();
    b.cancel().await.unwrap();
}

#[tokio::test]
async fn callers_without_tasks_get_accepted() {
    let h = setup("m2plain").await;
    let c = h.client(&h.member_token).await;
    let (v, err) = call(&c, "build", json!({"project": "m2plain"})).await;
    assert!(!err, "{v}");
    assert_eq!(v["accepted"], true);
    assert_eq!(v["done"], false);
    let job = v["job_id"].as_str().unwrap().to_string();
    let mut done = false;
    for _ in 0..600 {
        let (s, _) = call(&c, "job_status", json!({"job": job})).await;
        if s["done"] == true {
            assert_eq!(s["verdict"], "pass", "{s}");
            done = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert!(done);
    // validation happens before a task exists
    let (v, err) = call(&c, "build", json!({"project": "m2plain", "steps": ["test"]})).await;
    assert!(err);
    assert_eq!(v["error"], "wrong_kind");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn queue_limit_per_user() {
    let h = Harness::start("[queue]\nmax_queued_per_user = 1\n").await;
    repos_dir();
    make_repo("m2queue", &[("build.cmd", BUILD_CMD), ("test.cmd", TEST_OK)]);
    h.app.recipes.import_init(&recipes("m2queue"), "admin-a", "init").unwrap();
    // stop the workers so jobs stay queued
    h.app.engine.shutdown.cancel();
    let c = h.client(&h.member_token).await;
    let (_, err) = call(&c, "build", json!({"project": "m2queue"})).await;
    assert!(!err);
    let (v, err) = call(&c, "build", json!({"project": "m2queue"})).await;
    assert!(err);
    assert_eq!(v["error"], "queue_full");
    c.cancel().await.unwrap();
}

async fn call_any<S: rmcp::Service<rmcp::RoleClient>>(c: &rmcp::service::RunningService<rmcp::RoleClient, S>, name: &str, args: serde_json::Value) -> (serde_json::Value, bool) {
    let mut p = rmcp::model::CallToolRequestParams::new(name.to_string());
    if let serde_json::Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    match c.call_tool_once(p).await.unwrap() {
        rmcp::model::CallToolResponse::Complete(r) => (r.structured_content.unwrap_or_default(), r.is_error.unwrap_or(false)),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_push_is_built_not_an_old_result() {
    let h = setup("m2push").await;
    let c = h.task_client(&h.member_token).await;
    let t1 = start_task(&c, "build", json!({"project": "m2push"})).await;
    let (_, v1, _) = wait_task(&c, &t1, 120).await;
    let repo = repos_dir().join("m2push.git");
    write_files(&repo, &[("README.md", "new commit")]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "push"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let t2 = start_task(&c, "build", json!({"project": "m2push"})).await;
    let (_, v2, _) = wait_task(&c, &t2, 120).await;
    assert_ne!(v1["commit"], v2["commit"]);
    assert_eq!(v2["commit"], head.as_str());
    assert_eq!(v2["deduplicated"], false);
    assert_eq!(v2["verdict"], "pass");
    c.cancel().await.unwrap();
}

/// A build secret reaches the step through the env value the recipe wrote — and a
/// missing one is the admin's to fix, not a code failure (design §11.6).
#[tokio::test]
async fn build_secret_reaches_the_step_env() {
    const P: &str = "m2secret";
    repos_dir();
    make_repo(P, &[("show.cmd", "@echo off\r\nif \"%REG_TOKEN%\"==\"Bearer s3cr3t-value\" (echo token ok) else (echo token wrong & exit /b 1)\r\n")]);
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert(
        format!("projects/{P}/project.toml"),
        format!("schema = 1\nid = \"{P}\"\nrepo = \"https://example.invalid/{P}.git\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\nmax_job_minutes = 10\n"),
    );
    t.insert(
        format!("projects/{P}/recipe.toml"),
        "schema = 1\n[[step]]\nid = \"build\"\nkind = \"build\"\nrun = [\"./show.cmd\"]\nenv = { REG_TOKEN = \"Bearer ${secret.registry}\" }\nuncounted = true\n",
    );
    let h = Harness::start("").await;
    h.app.recipes.import_init(&t, "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;

    let (plan, _) = call_any(&c, "plan", json!({"project": P, "tool": "build"})).await;
    assert!(plan["secrets"][0]["state"].as_str().unwrap().contains("cimon build-secret m2secret registry"), "{plan:#}");
    let task = start_task(&c, "build", json!({"project": P})).await;
    let (_, v, _) = wait_task(&c, &task, 120).await;
    assert_eq!(v["verdict"], "inconclusive", "{v:#}");
    assert_eq!(v["failure_class"], "environment", "{v:#}");
    assert!(v["summary"].as_str().unwrap().contains("cimon build-secret m2secret registry"), "{v:#}");

    let mut secrets = cimon_engine::build_secrets::Secrets::new();
    secrets.entry(P.to_string()).or_default().insert("registry".into(), "s3cr3t-value".into());
    cimon_engine::build_secrets::save(&h.app.layout, &secrets).unwrap();
    let (plan, _) = call_any(&c, "plan", json!({"project": P, "tool": "build"})).await;
    assert_eq!(plan["secrets"][0]["state"], "있음", "{plan:#}");
    let task = start_task(&c, "build", json!({"project": P})).await;
    let (_, v, _) = wait_task(&c, &task, 120).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
    let (log, _) = call_any(&c, "job_log", json!({"job": v["job_id"], "step": "build"})).await;
    assert!(log["log"].as_str().unwrap().contains("token ok"), "{log:#}");
    c.cancel().await.unwrap();
}
