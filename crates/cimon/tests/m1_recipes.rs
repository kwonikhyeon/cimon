//! M1: recipe store import, static checks, describe_project, static plan, read-only
//! recipe tools, bundle backup with offsite copy and staleness warnings.

mod common;

use cimon_recipe::tree::MemTree;
use common::*;
use serde_json::json;
use std::path::PathBuf;

fn examples() -> MemTree {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/recipes");
    MemTree::from_dir(&dir).unwrap()
}

#[tokio::test]
async fn import_describe_plan_show() {
    let h = Harness::start("").await;
    let main = h.app.recipes.import_init(&examples(), "admin-a", "처음 내용").unwrap();
    // a second --init is refused
    assert_eq!(h.app.recipes.import_init(&examples(), "admin-a", "again").unwrap_err().code, "main_exists");

    let client = h.client(&h.member_token).await;
    let (v, err) = call(&client, "list_projects", json!({})).await;
    assert!(!err, "{v}");
    assert_eq!(v["projects"][0]["id"], "example-app");

    let (v, err) = call(&client, "describe_project", json!({"project": "example-app"})).await;
    assert!(!err, "{v}");
    assert_eq!(v["recipe"]["commit"], main.as_str());
    let steps: Vec<&str> = v["steps"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert!(steps.contains(&"build-apk") && steps.contains(&"ui-test"));

    let (v, err) = call(&client, "plan", json!({"project": "example-app", "tool": "run"})).await;
    assert!(!err, "{v}");
    let ran: Vec<&str> = v["steps"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ran, vec!["build-backend", "test-backend", "build-apk", "test-shared"]);
    assert_eq!(v["excluded"][0]["step"], "ui-test");
    assert!(v["unknown"][0].as_str().unwrap().contains("rust-toolchain.toml"), "{v}");

    // rejections come back as isError results with codes
    let (v, err) = call(&client, "plan", json!({"project": "example-app", "tool": "build", "steps": ["test-shared"]})).await;
    assert!(err);
    assert_eq!(v["error"], "wrong_kind");
    let (v, err) = call(&client, "plan", json!({"project": "example-app", "tool": "build", "ref": "release/1"})).await;
    assert!(err);
    assert_eq!(v["error"], "ref_not_allowed");
    let (v, err) = call(&client, "plan", json!({"project": "nope", "tool": "build"})).await;
    assert!(err);
    assert_eq!(v["error"], "unknown_project");
    let (v, err) = call(&client, "plan", json!({"project": "example-app", "tool": "build", "params": {"backend_url": "x"}})).await;
    assert!(err);
    assert_eq!(v["error"], "invalid_param");
    let (v, err) = call(&client, "plan", json!({"project": "example-app", "tool": "test", "params": {"backend_url": "http://a.b:1"}})).await;
    assert!(err);
    assert_eq!(v["error"], "param_not_applicable");

    let (v, err) = call(&client, "recipe_show", json!({"path": "projects/example-app/recipe.toml"})).await;
    assert!(!err, "{v}");
    assert_eq!(v["commit"], main.as_str());
    assert!(v["result"]["content"].as_str().unwrap().contains("build-apk"));
    // offsite = "none" → every recipe_* result warns
    assert!(v["backup_warnings"][0].as_str().unwrap().contains("외부 백업 없음"), "{v}");

    let (v, err) = call(&client, "recipe_export", json!({})).await;
    assert!(!err);
    assert!(v["files"]["toolchains.toml"].is_string());
    assert_eq!(v["cimon_version"], env!("CARGO_PKG_VERSION"));

    let (v, err) = call(&client, "recipe_history", json!({})).await;
    assert!(!err);
    assert_eq!(v["commits"][0]["author"], "admin-a");

    let (v, err) = call(&client, "recipe_show", json!({"ref": "feature/x"})).await;
    assert!(err);
    assert_eq!(v["error"], "trial_ref_not_allowed");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn bundle_goes_offsite_and_staleness_warns() {
    let offsite = tempfile::tempdir().unwrap();
    let extra = format!("[backup]\noffsite = '{}'\nkeep = 2\n", offsite.path().display());
    // base_config already sets offsite = "none"; build the config by hand instead
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cimon");
    let text = format!(
        "[server]\nbind = \"127.0.0.1:0\"\n[tailscale]\nrequire_funnel_off = false\n[paths]\nroot = '{}'\n[accounts]\nbuild = \"\"\n{extra}",
        root.display()
    );
    let cfg = cimon_core::Config::parse(&text, "t").unwrap();
    let app = cimon_engine::App::open(cfg).unwrap();
    // nothing imported yet: warning about missing offsite copy does not apply
    assert!(cimon_engine::backup::warnings(&app).is_empty());
    app.recipes.import_init(&examples(), "admin-a", "init").unwrap();
    let w = cimon_engine::backup::warnings(&app);
    assert!(w.iter().any(|x| x.contains("외부 사본이 아직 없다")), "{w:?}");
    for _ in 0..3 {
        cimon_engine::backup::backup_recipes(&app, "test").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    assert!(cimon_engine::backup::warnings(&app).is_empty());
    let copies: Vec<_> = std::fs::read_dir(offsite.path()).unwrap().collect();
    assert_eq!(copies.len(), 3, "offsite keeps every copy; local pruning keeps 2");
    let local: Vec<_> = std::fs::read_dir(app.layout.backup()).unwrap().collect();
    assert_eq!(local.len(), 2);
    cimon_engine::backup::backup_db(&app).unwrap();
}
