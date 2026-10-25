//! M4: artifacts — pre-clean keeps only the new file among old versions, zip packing,
//! required/optional inspections, tracked-file globs refused at intake, Range, 410.

mod common;

use cimon_recipe::tree::MemTree;
use common::*;
use serde_json::json;

const BUILD: &str = "@echo off\r\nif not exist out mkdir out\r\necho build %RANDOM%%RANDOM% > out\\app-%RANDOM%%RANDOM%.txt\r\nif not exist dist\\sub mkdir dist\\sub\r\necho a > dist\\a.txt\r\necho b > dist\\sub\\b.txt\r\n";

fn recipes(project: &str, extra_artifact: &str) -> MemTree {
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert(
        format!("projects/{project}/project.toml"),
        format!("schema = 1\nid = \"{project}\"\nrepo = \"https://example.invalid/{project}.git\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\nretention_days = 7\n"),
    );
    t.insert(
        format!("projects/{project}/recipe.toml"),
        format!(
            r#"schema = 1
[[step]]
id = "build"
kind = "build"
run = ["./build.cmd"]
  [[step.artifact]]
  name = "app.txt"
  glob = "out/app-*.txt"
  expect = 1
  inspect = [{{ builtin = "pe-version" }}]

  [[step.artifact]]
  name = "dist.zip"
  glob = "dist/**"
  expect_min = 2
  pack = "zip"
  pack_name = "dist-${{job.commit_short}}.zip"
  default = false
{extra_artifact}
"#
        ),
    );
    t
}

async fn build<S: rmcp::Service<rmcp::RoleClient>>(c: &rmcp::service::RunningService<rmcp::RoleClient, S>, args: serde_json::Value) -> serde_json::Value {
    let t = start_task(c, "build", args).await;
    let (_, v, _) = wait_task(c, &t, 120).await;
    v
}

#[tokio::test]
async fn new_file_only_zip_inspections_range_410() {
    repos_dir();
    make_repo("m4art", &[("build.cmd", BUILD)]);
    let h = Harness::start("").await;
    h.app.recipes.import_init(&recipes("m4art", ""), "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let first = build(&c, json!({"project": "m4art"})).await;
    assert_eq!(first["verdict"], "pass", "{first:#}");
    // the warm slot now holds the old app-*.txt; pre-clean removes it before the build
    let v = build(&c, json!({"project": "m4art", "artifacts": ["app.txt", "dist.zip"]})).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
    let arts = v["artifacts"].as_array().unwrap();
    assert_eq!(arts.len(), 2);
    let app = arts.iter().find(|a| a["name"] == "app.txt").unwrap();
    // a non-PE file: optional inspection failure is recorded, not fatal
    assert!(app["inspect"]["pe-version_error"].is_string(), "{app}");
    assert!(v["warnings"].as_array().unwrap().iter().any(|w| w["source"] == "inspect"), "{v:#}");
    let zip = arts.iter().find(|a| a["name"] == "dist.zip").unwrap();
    assert!(zip["file_name"].as_str().unwrap().starts_with("dist-"), "{zip}");
    let url = zip["url"].as_str().unwrap().to_string();
    let bytes = reqwest::get(&url).await.unwrap().bytes().await.unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
    let mut names: Vec<String> = (0..z.len()).map(|i| z.by_index(i).unwrap().name().to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["a.txt", "sub/b.txt"]);
    // Range
    let part = reqwest::Client::new().get(&url).header("Range", "bytes=0-3").send().await.unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(part.bytes().await.unwrap().len(), 4);
    // provenance written next to the artifacts
    let job = cimon_core::parse_job_label(v["job_id"].as_str().unwrap()).unwrap();
    let prov: serde_json::Value = serde_json::from_slice(&std::fs::read(h.app.layout.job_artifacts(job).join("provenance.json")).unwrap()).unwrap();
    assert_eq!(prov["signed"], false);
    assert_eq!(prov["artifacts"].as_array().unwrap().len(), 2);
    // retention: expire the artifacts, then the link answers 410
    h.app.store.tx(|tx| {
        tx.execute("UPDATE artifacts SET expires_at = '2000-01-01T00:00:00Z'", [])?;
        Ok(())
    })
    .unwrap();
    cimon_engine::janitor::retention_pass(&h.app);
    let gone = reqwest::get(&url).await.unwrap();
    assert_eq!(gone.status(), 410);
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn required_inspection_failure_is_recipe_inconclusive() {
    repos_dir();
    make_repo("m4req", &[("build.cmd", BUILD)]);
    let h = Harness::start("").await;
    let mut t = recipes("m4req", "");
    let r = t.files.get("projects/m4req/recipe.toml").cloned().unwrap();
    t.insert("projects/m4req/recipe.toml", String::from_utf8(r).unwrap().replace("inspect = [{ builtin = \"pe-version\" }]", "inspect = [{ builtin = \"apk-signer\", required = true }]"));
    h.app.recipes.import_init(&t, "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let v = build(&c, json!({"project": "m4req"})).await;
    assert_eq!(v["verdict"], "inconclusive", "{v:#}");
    assert_eq!(v["failure_class"], "recipe");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn glob_matching_tracked_file_is_refused_at_intake() {
    repos_dir();
    make_repo("m4tracked", &[("build.cmd", BUILD), ("out/app-committed.txt", "tracked")]);
    let h = Harness::start("").await;
    h.app.recipes.import_init(&recipes("m4tracked", ""), "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let v = build(&c, json!({"project": "m4tracked"})).await;
    assert_eq!(v["verdict"], "fail", "{v:#}");
    assert_eq!(v["failure_class"], "recipe");
    assert_eq!(v["reason_code"], "glob_matches_tracked");
    // the tracked file is untouched in the mirror's checkout path (never deleted)
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn missing_artifact_fails_the_step() {
    repos_dir();
    make_repo("m4miss", &[("build.cmd", "@echo nothing\r\n")]);
    let h = Harness::start("").await;
    h.app.recipes.import_init(&recipes("m4miss", ""), "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let v = build(&c, json!({"project": "m4miss"})).await;
    assert_eq!(v["verdict"], "fail", "{v:#}");
    assert_eq!(v["failure_class"], "code");
    assert!(v["summary"].as_str().unwrap().contains("found 0"), "{}", v["summary"]);
    c.cancel().await.unwrap();
}
