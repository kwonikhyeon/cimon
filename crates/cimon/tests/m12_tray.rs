//! What the tray app asks of the service (docs/tray.md §4.2): stop once nothing runs —
//! the running job finishes, new jobs are refused, ready jobs wait for the next start,
//! and the server stops by itself — or take jobs again.

mod common;

use cimon_recipe::tree::MemTree;
use common::*;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const SLOW_CMD: &str = "@echo off\r\nping -n 5 127.0.0.1 >nul\r\necho done\r\n";

fn recipes(p: &str) -> MemTree {
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert(
        format!("projects/{p}/project.toml"),
        format!("schema = 1\nid = \"{p}\"\nrepo = \"https://example.invalid/{p}.git\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\nmax_job_minutes = 10\n"),
    );
    t.insert(format!("projects/{p}/recipe.toml"), "schema = 1\n[[step]]\nid = \"build\"\nkind = \"build\"\nrun = [\"./slow.cmd\"]\nuncounted = true\n");
    t
}

async fn call_any<S: rmcp::Service<rmcp::RoleClient>>(c: &rmcp::service::RunningService<rmcp::RoleClient, S>, name: &str, args: Value) -> (Value, bool) {
    let mut p = rmcp::model::CallToolRequestParams::new(name.to_string());
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    match c.call_tool_once(p).await.unwrap() {
        rmcp::model::CallToolResponse::Complete(r) => (r.structured_content.unwrap_or_default(), r.is_error.unwrap_or(false)),
        other => panic!("{other:?}"),
    }
}

async fn health(h: &Harness) -> Value {
    reqwest::Client::new().get(format!("{}/healthz", h.base)).bearer_auth(&h.member_token).send().await.unwrap().json().await.unwrap()
}

async fn wait_state(h: &Harness, id: i64, states: &[&str], secs: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let s = h.app.store.job(id).unwrap().unwrap().state.as_str().to_string();
        if states.contains(&s.as_str()) {
            return s;
        }
        assert!(Instant::now() < deadline, "job {id} still {s}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn job_id(task_or_job: &str, h: &Harness) -> i64 {
    h.app.store.task(task_or_job).unwrap().unwrap().job_id
}

#[tokio::test]
async fn stop_when_idle_lets_the_job_finish_then_stops() {
    const P: &str = "m12idle";
    repos_dir();
    make_repo(P, &[("slow.cmd", SLOW_CMD)]);
    let h = Harness::start("").await;
    h.app.recipes.import_init(&recipes(P), "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;

    let t1 = start_task(&c, "build", json!({"project": P})).await;
    let j1 = job_id(&t1, &h);
    wait_state(&h, j1, &["running"], 60).await;
    // a second job waits behind it (clean, so it is not merged into the first)
    let t2 = start_task(&c, "build", json!({"project": P, "clean": true})).await;
    let j2 = job_id(&t2, &h);
    wait_state(&h, j2, &["ready"], 60).await;

    cimon_engine::stop_when_idle(&h.app, true);
    let hz = health(&h).await;
    assert_eq!(hz["stopping_when_idle"], true, "{hz}");
    assert_eq!(hz["draining"], true, "{hz}");
    let (v, err) = call_any(&c, "build", json!({"project": P})).await;
    assert!(err && v["error"] == "draining", "new jobs are refused: {v}");
    assert!(!h.shutdown.is_cancelled(), "a job is still running");

    assert_eq!(wait_state(&h, j1, &["done"], 60).await, "done");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !h.shutdown.is_cancelled() {
        assert!(Instant::now() < deadline, "the server did not stop once idle");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let v1 = h.app.store.job(j1).unwrap().unwrap();
    assert_eq!(v1.verdict.as_deref(), Some("pass"), "the running job finished, not interrupted");
    assert_eq!(h.app.store.job(j2).unwrap().unwrap().state.as_str(), "ready", "the waiting job stays for the next start");
}

#[tokio::test]
async fn cancelling_takes_jobs_again() {
    const P: &str = "m12cancel";
    repos_dir();
    make_repo(P, &[("slow.cmd", "@echo off\r\necho quick\r\n")]);
    let h = Harness::start("").await;
    h.app.recipes.import_init(&recipes(P), "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;

    cimon_engine::stop_when_idle(&h.app, true);
    let (v, err) = call_any(&c, "build", json!({"project": P})).await;
    assert!(err && v["error"] == "draining", "{v}");
    cimon_engine::stop_when_idle(&h.app, false);
    let hz = health(&h).await;
    assert_eq!(hz["stopping_when_idle"], false, "{hz}");
    assert_eq!(hz["draining"], false, "{hz}");
    let t = start_task(&c, "build", json!({"project": P})).await;
    let (_, v, _) = wait_task(&c, &t, 120).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
    assert!(!h.shutdown.is_cancelled());
}
