//! M3: toolchains — managed installs from main's catalog (archive with SHASUMS,
//! github-release with asset digests), checksum mismatches, the trial-branch catalog
//! rule with `provision --recipe-ref`, auto-approve, base checks.

mod common;

use cimon_recipe::tree::MemTree;
use common::*;
use rmcp::model::TaskStatus;
use serde_json::json;
use std::io::Write;

/// A zip with `hello-<v>/bin/hello.cmd` printing `hello <v>`.
fn hello_zip(version: &str) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file(format!("hello-{version}/bin/hello.cmd"), o).unwrap();
        z.write_all(format!("@echo hello {version}\r\n").as_bytes()).unwrap();
        z.finish().unwrap();
    }
    buf.into_inner()
}

/// Serves `/hello-<v>.zip`, `/SHASUMS256.txt` (optionally wrong) and a fake GitHub API.
async fn file_server(versions: &[&str], wrong_sum: bool) -> String {
    use axum::{routing::get, Router};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut zips = std::collections::HashMap::new();
    let mut sums = String::new();
    for v in versions {
        let z = hello_zip(v);
        let sum = if wrong_sum { "0".repeat(64) } else { cimon_core::sha256_hex(&z) };
        sums.push_str(&format!("{sum}  hello-{v}.zip\n"));
        zips.insert(format!("hello-{v}.zip"), z);
    }
    let zips = std::sync::Arc::new(zips);
    let z2 = zips.clone();
    let b2 = base.clone();
    let app = Router::new()
        .route("/SHASUMS256.txt", get(move || async move { sums.clone() }))
        .route(
            "/dl/{name}",
            get(move |axum::extract::Path(name): axum::extract::Path<String>| {
                let zips = zips.clone();
                async move { zips.get(&name).cloned().map(|b| (axum::http::StatusCode::OK, b)).unwrap_or((axum::http::StatusCode::NOT_FOUND, vec![])) }
            }),
        )
        .route(
            "/repos/{o}/{r}/releases/tags/{tag}",
            get(move |axum::extract::Path((_o, _r, tag)): axum::extract::Path<(String, String, String)>| {
                let zips = z2.clone();
                let base = b2.clone();
                async move {
                    let v = tag.trim_start_matches('v').to_string();
                    let name = format!("hello-{v}.zip");
                    let digest = zips.get(&name).map(|z| cimon_core::sha256_hex(z)).unwrap_or_default();
                    axum::Json(json!({"assets": [{"name": name, "digest": format!("sha256:{digest}"), "browser_download_url": format!("{base}/dl/{name}")}]}))
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    base
}

fn catalog(base: &str, installer: &str) -> String {
    match installer {
        "archive" => format!(
            "schema = 1\n[kind.hello]\ntier = \"managed\"\ninstaller = \"archive\"\nurl = \"{base}/dl/hello-{{version}}.zip\"\nchecksum = {{ source = \"shasums\", url = \"{base}/SHASUMS256.txt\" }}\nbin = [\"hello-{{version}}/bin\"]\nprobe = {{ run = [\"hello\"], expect = \"hello {{version}}\" }}\n\n[kind.mybase]\ntier = \"base\"\ninstaller = \"base\"\ncheck = {{ path_from = \"mybase\" }}\nbin = [\"bin\"]\nversion_any = true\nprobe = {{ run = [\"mybase\"], expect = \"mybase here\" }}\nhint = \"install mybase\"\n"
        ),
        _ => "schema = 1\n[kind.hello]\ntier = \"managed\"\ninstaller = \"github-release\"\nrepo = \"o/hello\"\ntag = \"v{version}\"\nasset = \"hello-{version}.zip\"\nbin = [\"hello-{version}/bin\"]\nprobe = { run = [\"hello\"], expect = \"hello {version}\" }\n".to_string(),
    }
}

fn recipes(project: &str, cat: &str, extra_tc: &str) -> MemTree {
    let mut t = MemTree::new();
    t.insert("toolchains.toml", cat.to_string());
    t.insert(
        format!("projects/{project}/project.toml"),
        format!("schema = 1\nid = \"{project}\"\nrepo = \"https://example.invalid/{project}.git\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\n"),
    );
    t.insert(
        format!("projects/{project}/recipe.toml"),
        format!("schema = 1\n[toolchains]\nhello = {{ version = \"1.2.3\" }}\n{extra_tc}\n[[step]]\nid = \"build\"\nkind = \"build\"\nrun = [\"hello\"]\nfail_on_output = [\"is not recognized\"]\n"),
    );
    t
}

async fn run_build<S: rmcp::Service<rmcp::RoleClient>>(c: &rmcp::service::RunningService<rmcp::RoleClient, S>, args: serde_json::Value) -> serde_json::Value {
    let t = start_task(c, "build", args).await;
    let (st, v, _) = wait_task(c, &t, 180).await;
    assert_eq!(st, TaskStatus::Completed);
    v
}

#[tokio::test]
async fn archive_install_with_shasums_then_runs() {
    repos_dir();
    make_repo("m3arch", &[("README.md", "x")]);
    let base = file_server(&["1.2.3"], false).await;
    let h = Harness::start("[toolchains]\nauto_approve = [\"hello\"]\n").await;
    h.app.recipes.import_init(&recipes("m3arch", &catalog(&base, "archive"), ""), "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let v = run_build(&c, json!({"project": "m3arch"})).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
    assert_eq!(v["toolchains"]["hello"], "1.2.3");
    let installs = h.app.store.installs().unwrap();
    assert_eq!(installs.len(), 1);
    assert!(installs[0].root.contains("hello"), "{:?}", installs[0]);
    // the step really ran the installed tool
    let (log, _) = call(&h.client(&h.member_token).await, "job_log", json!({"job": v["job_id"], "step": "build"})).await;
    assert!(log["log"].as_str().unwrap().contains("hello 1.2.3"), "{log}");
}

#[tokio::test]
async fn checksum_mismatch_is_not_installed() {
    repos_dir();
    make_repo("m3bad", &[("README.md", "x")]);
    let base = file_server(&["1.2.3"], true).await;
    let h = Harness::start("[toolchains]\nauto_approve = [\"hello\"]\n").await;
    h.app.recipes.import_init(&recipes("m3bad", &catalog(&base, "archive"), ""), "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let v = run_build(&c, json!({"project": "m3bad"})).await;
    assert_eq!(v["verdict"], "inconclusive", "{v:#}");
    assert_eq!(v["failure_class"], "environment");
    assert!(v["summary"].as_str().unwrap().contains("checksum mismatch"), "{}", v["summary"]);
    assert!(h.app.store.installs().unwrap().is_empty());
}

#[tokio::test]
async fn github_release_digest_install() {
    repos_dir();
    make_repo("m3gh", &[("README.md", "x")]);
    let base = file_server(&["1.2.3"], false).await;
    let h = Harness::start(&format!("[toolchains]\nauto_approve = [\"hello\"]\n[github]\napi_url = \"{base}\"\n")).await;
    h.app.recipes.import_init(&recipes("m3gh", &catalog(&base, "github"), ""), "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let v = run_build(&c, json!({"project": "m3gh"})).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
}

#[tokio::test]
async fn not_auto_approved_needs_admin_then_trial_catalog_rule() {
    repos_dir();
    make_repo("m3prov", &[("README.md", "x")]);
    let base = file_server(&["1.2.3"], false).await;
    let h = Harness::start("").await;
    let main = h.app.recipes.import_init(&recipes("m3prov", &catalog(&base, "archive"), ""), "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let v = run_build(&c, json!({"project": "m3prov"})).await;
    assert_eq!(v["failure_class"], "environment", "{v:#}");
    assert_eq!(v["reason_code"], "toolchain_not_provisioned");
    assert!(v["summary"].as_str().unwrap().contains("cimon provision"), "{}", v["summary"]);
    // admin provision from main
    let app = h.app.clone();
    let lines = tokio::task::spawn_blocking(move || {
        cimon_engine::toolchains::provision(&app, &cimon_engine::toolchains::ProvisionArgs { project: Some("m3prov".into()), admin: "admin-a".into(), ..Default::default() })
    })
    .await
    .unwrap()
    .unwrap();
    assert!(lines.iter().all(|l| l.ok), "{lines:?}");
    let v = run_build(&c, json!({"project": "m3prov"})).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");

    // a trial branch that changes the catalog entry is not installed by intake
    let new_cat = catalog(&base, "archive").replace("SHASUMS256.txt", "SHASUMS256.txt?v=2");
    let edit = h
        .app
        .recipes
        .edit(&h.app.store, "member-b", "recipe/cat", &main, "catalog url", &[cimon_recipestore::Change { path: "toolchains.toml".into(), content: Some(new_cat), delete: false, bytes: None }])
        .unwrap();
    let v = run_build(&c, json!({"project": "m3prov", "recipe_ref": "recipe/cat"})).await;
    assert_eq!(v["reason_code"], "toolchain_not_provisioned", "{v:#}");
    assert!(v["summary"].as_str().unwrap().contains("--recipe-ref recipe/cat"), "{}", v["summary"]);
    assert_eq!(v["trial"], true);
    // after review the admin installs the trial entry; the trial then runs
    let app = h.app.clone();
    let lines = tokio::task::spawn_blocking(move || {
        cimon_engine::toolchains::provision(
            &app,
            &cimon_engine::toolchains::ProvisionArgs { kind: Some("hello".into()), version: Some("1.2.3".into()), recipe_ref: Some("recipe/cat".into()), admin: "admin-a".into(), ..Default::default() },
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(lines[0].ok, "{lines:?}");
    let v = run_build(&c, json!({"project": "m3prov", "recipe_ref": "recipe/cat"})).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
    assert_eq!(v["recipe"]["commit"], edit.commit.as_str());
}

#[tokio::test]
async fn base_kind_checks() {
    repos_dir();
    make_repo("m3base", &[("README.md", "x")]);
    let base = file_server(&["1.2.3"], false).await;
    let tooldir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tooldir.path().join("bin")).unwrap();
    std::fs::write(tooldir.path().join("bin").join("mybase.cmd"), "@echo mybase here\r\n").unwrap();
    let extra = format!("[toolchains]\nauto_approve = [\"hello\"]\n[base]\nmybase = '{}'\n", tooldir.path().display());
    let h = Harness::start(&extra).await;
    h.app.recipes.import_init(&recipes("m3base", &catalog(&base, "archive"), "mybase = {}"), "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let v = run_build(&c, json!({"project": "m3base"})).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
    // remove the base tool: the job says what is missing and how to get it
    std::fs::remove_file(tooldir.path().join("bin").join("mybase.cmd")).unwrap();
    let v = run_build(&c, json!({"project": "m3base"})).await;
    assert_eq!(v["reason_code"], "base_missing", "{v:#}");
    assert!(v["summary"].as_str().unwrap().contains("install mybase"), "{}", v["summary"]);
}
