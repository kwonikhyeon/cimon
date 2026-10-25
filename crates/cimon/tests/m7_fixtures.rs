//! M7: fixtures (up → ready → scenario → collect → down → gone), on_up_failure,
//! ready.on_timeout exited_path, script judge verdicts, cancellation tears the fixture
//! down, account_env in single-account mode warns.

mod common;

use cimon_recipe::tree::MemTree;
use common::*;
use rmcp::model::TaskStatus;
use serde_json::json;

const CLI_OK: &str = "@echo off\r\nif \"%1\"==\"up\" ( if not exist stackstate mkdir stackstate & echo up> stackstate\\up & echo service log>stackstate\\svc.log & exit /b 0 )\r\nif \"%1\"==\"status\" ( if exist stackstate\\up ( exit /b 0 ) else ( exit /b 3 ) )\r\nif \"%1\"==\"down\" ( del /q stackstate\\up 2>NUL & exit /b 0 )\r\nif \"%1\"==\"status-json\" ( echo {\"services\":[{\"name\":\"svc\",\"state\":\"running\"}]} & exit /b 0 )\r\nexit /b 9\r\n";
const CLI_UP_BROKEN: &str = "@echo off\r\nif \"%1\"==\"up\" ( echo error: cannot start & exit /b 1 )\r\nif \"%1\"==\"status\" exit /b 3\r\nexit /b 0\r\n";
const CLI_EXITS: &str = "@echo off\r\nif \"%1\"==\"up\" exit /b 0\r\nif \"%1\"==\"status\" exit /b 3\r\nif \"%1\"==\"status-json\" ( echo {\"services\":[{\"name\":\"planner\",\"state\":\"exited\"},{\"name\":\"svc\",\"state\":\"running\"}]} & exit /b 0 )\r\nexit /b 0\r\n";

const JUDGE: &str = "param([string]$Out, [double]$Min)\r\n$f = Join-Path $Out 'travel.txt'\r\nif (-not (Test-Path $f)) { Write-Output '{\"verdict\":\"inconclusive\",\"failure_class\":\"unobserved\",\"reason\":\"no travel recorded\"}'; exit 0 }\r\n$t = [double]((Get-Content $f -Raw).Trim())\r\nif ($t -lt $Min) { Write-Output ('{\"verdict\":\"fail\",\"failure_class\":\"code\",\"reason\":\"moved ' + $t + ' m\",\"metrics\":{\"travel\":' + $t + '}}'); exit 0 }\r\nWrite-Output ('{\"verdict\":\"pass\",\"reason\":\"moved ' + $t + ' m\",\"metrics\":{\"travel\":' + $t + '}}')\r\n";

fn recipes(project: &str) -> MemTree {
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert("scripts/judge.ps1", JUDGE);
    t.insert(
        format!("projects/{project}/project.toml"),
        format!("schema = 1\nid = \"{project}\"\nrepo = \"https://example.invalid/{project}.git\"\nallowed_refs = [\"main\", \"feature/**\"]\ndefault_ref = \"main\"\n"),
    );
    t.insert(
        format!("projects/{project}/recipe.toml"),
        r#"schema = 1
[isolate]
APP_HOME = "job"
[account_env]
APP_HOME = "job"

[fixture.stack]
after = ["build-cli"]
requires_artifacts = ["cli.cmd"]
on_up_failure = "code"
env = { STACK_NAME = "cimon-${job.id}" }
collect = ["stackstate/*.log"]

  [[fixture.stack.up]]
  run = ["${artifact.cli.cmd}", "up"]
  timeout_min = 1

  [fixture.stack.ready]
  run = ["${artifact.cli.cmd}", "status"]
  until = { exit = 0 }
  every_s = 1
  timeout_s = 4
  on_timeout = { run = ["${artifact.cli.cmd}", "status-json"], exited_path = "$.services[?(@.state=='exited')].name" }

  [[fixture.stack.down]]
  run = ["${artifact.cli.cmd}", "down"]

  [fixture.stack.gone]
  run = ["${artifact.cli.cmd}", "status"]
  until = { exit_not = 0 }
  every_s = 1
  timeout_s = 4

[[step]]
id = "build-cli"
kind = "build"
run = ["cmd", "/c", "if not exist out mkdir out & copy /y src\\cli.cmd out\\cli.cmd >NUL"]
  [[step.artifact]]
  name = "cli.cmd"
  glob = "out/cli.cmd"
  expect = 1
  default = false

[[step]]
id = "drive"
kind = "scenario"
fixture = "stack"
run = ["./drive.cmd", "${step.out}"]
judge = { type = "script", run = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "${recipes}/scripts/judge.ps1", "-Out", "${step.out}", "-Min", "2.5"] }

[[step]]
id = "long"
kind = "scenario"
fixture = "stack"
run = ["cmd", "/c", "ping -n 30 127.0.0.1 >NUL"]
uncounted = true
"#,
    );
    t
}

async fn scenario<S: rmcp::Service<rmcp::RoleClient>>(c: &rmcp::service::RunningService<rmcp::RoleClient, S>, args: serde_json::Value) -> serde_json::Value {
    let t = start_task(c, "test", args).await;
    let (st, v, _) = wait_task(c, &t, 120).await;
    assert_eq!(st, TaskStatus::Completed);
    v
}

fn step<'a>(v: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    v["steps"].as_array().unwrap().iter().find(|s| s["id"] == id).unwrap_or_else(|| panic!("no step {id} in {v:#}"))
}

async fn setup(project: &str) -> Harness {
    repos_dir();
    let repo = make_repo(project, &[("src/cli.cmd", CLI_OK), ("drive.cmd", "@echo off\r\necho 3.2> %1\\travel.txt\r\n")]);
    branch_with(&repo, "feature/up-broken", &[("src/cli.cmd", CLI_UP_BROKEN)]);
    branch_with(&repo, "feature/exits", &[("src/cli.cmd", CLI_EXITS)]);
    branch_with(&repo, "feature/short", &[("drive.cmd", "@echo off\r\necho 0.004> %1\\travel.txt\r\n")]);
    branch_with(&repo, "feature/silent", &[("drive.cmd", "@echo off\r\necho driving\r\n")]);
    let h = Harness::start("").await;
    h.app.recipes.import_init(&recipes(project), "admin-a", "init").unwrap();
    h
}

#[tokio::test]
async fn fixture_lifecycle_and_script_judge() {
    let h = setup("m7ok").await;
    let c = h.task_client(&h.member_token).await;
    let v = scenario(&c, json!({"project": "m7ok", "steps": ["drive"]})).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
    assert_eq!(step(&v, "drive")["metrics"]["travel"], 3.2);
    // single-account mode: account_env is skipped with a warning
    assert!(v["warnings"].as_array().unwrap().iter().any(|w| w["source"] == "account_env"), "{v:#}");
    // collect ran before down; down removed the state; gone confirmed it
    let job = cimon_core::parse_job_label(v["job_id"].as_str().unwrap()).unwrap();
    let collected = h.app.layout.job_logs(job).join("fixture-stack").join("stackstate").join("svc.log");
    assert!(collected.exists(), "collect did not copy evidence: {}", collected.display());
    assert!(!v["warnings"].as_array().unwrap().iter().any(|w| w["message"].as_str().unwrap_or("").contains("잔류")), "{v:#}");
    let slot = h.app.layout.warm_slot("m7ok", 1);
    assert!(!slot.join("stackstate").join("up").exists(), "down did not run");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn broken_cli_and_exited_component_are_code_failures() {
    let h = setup("m7bad").await;
    let c = h.task_client(&h.member_token).await;
    let v = scenario(&c, json!({"project": "m7bad", "ref": "feature/up-broken", "steps": ["drive"]})).await;
    assert_eq!(v["verdict"], "fail", "{v:#}");
    assert_eq!(v["failure_class"], "code");
    assert!(step(&v, "drive")["reason"].as_str().unwrap().contains("up failed"), "{v:#}");
    let v = scenario(&c, json!({"project": "m7bad", "ref": "feature/exits", "steps": ["drive"]})).await;
    assert_eq!(v["verdict"], "fail", "{v:#}");
    assert_eq!(v["failure_class"], "code");
    assert!(step(&v, "drive")["reason"].as_str().unwrap().contains("planner exited"), "{v:#}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn judge_script_fail_and_unobserved() {
    let h = setup("m7judge").await;
    let c = h.task_client(&h.member_token).await;
    let v = scenario(&c, json!({"project": "m7judge", "ref": "feature/short", "steps": ["drive"]})).await;
    assert_eq!(v["verdict"], "fail", "{v:#}");
    assert_eq!(v["failure_class"], "code");
    assert!(v["summary"].as_str().unwrap().contains("moved 0.004"), "{}", v["summary"]);
    let v = scenario(&c, json!({"project": "m7judge", "ref": "feature/silent", "steps": ["drive"]})).await;
    assert_eq!(v["verdict"], "inconclusive", "{v:#}");
    assert_eq!(v["failure_class"], "unobserved");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn cancel_tears_the_fixture_down() {
    let h = setup("m7cancel").await;
    let c = h.task_client(&h.member_token).await;
    let task = start_task(&c, "test", json!({"project": "m7cancel", "steps": ["long"]})).await;
    let mut job = String::new();
    for _ in 0..300 {
        let (s, _) = call(&h.client(&h.member_token).await, "job_status", json!({"job": task})).await;
        job = s["job_id"].as_str().unwrap_or_default().to_string();
        if s["summary"].as_str().unwrap_or("").starts_with("long") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let slot = h.app.layout.warm_slot("m7cancel", 1);
    assert!(slot.join("stackstate").join("up").exists(), "fixture should be up while the scenario runs");
    let (_, err) = call(&h.client(&h.admin_token).await, "cancel_job", json!({"job": job})).await;
    assert!(!err);
    let (st, _, _) = wait_task(&c, &task, 30).await;
    assert_eq!(st, TaskStatus::Cancelled);
    assert!(!slot.join("stackstate").join("up").exists(), "down must run after a cancel");
    c.cancel().await.unwrap();
}
