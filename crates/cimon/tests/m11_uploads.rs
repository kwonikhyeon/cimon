//! Uploads (design §11.7): a file or value the caller sends from their own machine for
//! one job — taken over HTTP, handed to the steps that declare it, kept out of logs,
//! and deleted when the job ends.

mod common;

use cimon_recipe::tree::MemTree;
use common::*;
use serde_json::{json, Value};

const P: &str = "m11up";
const KEYSTORE: &str = "KEYSTORE-BYTES\n";
const PASSWORD: &str = "pw-s3cr3t-123";

const RELEASE_CMD: &str = "@echo off\r\n\
if not \"%STORE_PASSWORD%\"==\"pw-s3cr3t-123\" (echo password wrong & exit /b 1)\r\n\
findstr /c:\"KEYSTORE-BYTES\" \"%~1\" >nul || (echo keystore wrong & exit /b 1)\r\n\
if not exist \"%~2\\release.jks\" (echo dir wrong & exit /b 1)\r\n\
echo password is %STORE_PASSWORD%\r\n\
echo caches %GRADLE_USER_HOME%\r\n\
if exist \"%GRADLE_USER_HOME%\\gradle\\init.d\\planted.gradle\" (echo planted init script & exit /b 1)\r\n\
echo release ok\r\n";

fn recipes() -> MemTree {
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert(
        format!("projects/{P}/project.toml"),
        format!("schema = 1\nid = \"{P}\"\nrepo = \"https://example.invalid/{P}.git\"\nallowed_refs = [\"main\", \"feature/**\"]\ndefault_ref = \"main\"\nmax_job_minutes = 10\n"),
    );
    t.insert(
        format!("projects/{P}/recipe.toml"),
        r#"schema = 1
[upload.keystore]
file = "release.jks"
applies_to = ["release"]

[upload.store-password]
kind = "text"
applies_to = ["release"]

[[step]]
id = "build"
kind = "build"
run = ["cmd", "/c", "echo", "built"]
uncounted = true

[[step]]
id = "release"
kind = "package"
run = ["./release.cmd", "${upload.keystore}", "${upload.keystore.dir}"]
env = { STORE_PASSWORD = "${upload.store-password}" }
uncounted = true
"#,
    );
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

async fn upload(h: &Harness, token: &str, label: &str, kind: &str, body: &[u8]) -> (u16, Value) {
    let r = reqwest::Client::new()
        .post(format!("{}/uploads?kind={kind}&label={label}", h.base))
        .bearer_auth(token)
        .header("Content-Type", "application/octet-stream")
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn an_upload_reaches_its_step_once_and_is_gone_after() {
    repos_dir();
    let repo = make_repo(P, &[("release.cmd", RELEASE_CMD)]);
    branch_with(&repo, "feature/x", &[("x.txt", "x")]);
    let h = Harness::start("[uploads]\nmax_bytes = 1024\n").await;
    h.app.recipes.import_init(&recipes(), "admin-a", "init").unwrap();
    let c = h.task_client(&h.member_token).await;
    let admin = h.task_client(&h.admin_token).await;

    // the scripts that send them are served with this server's address
    for path in ["upload.ps1", "upload.sh"] {
        let text = reqwest::get(format!("{}/{path}", h.base)).await.unwrap().text().await.unwrap();
        assert!(text.contains(&format!("'{}'", h.base)), "{path}");
        assert!(text.is_ascii(), "{path}: Windows PowerShell reads a BOM-less script as ANSI");
    }

    // taking an upload: a token, a size limit, one line for a text
    let r = reqwest::Client::new().post(format!("{}/uploads?kind=file&label=keystore", h.base)).body("x").send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(upload(&h, &h.member_token, "keystore", "file", &[b'x'; 2000]).await.0, 413);
    assert_eq!(upload(&h, &h.member_token, "store-password", "text", b"two\nlines").await.0, 400);
    assert_eq!(upload(&h, &h.member_token, "Store", "text", b"x").await.0, 400, "an upload name");
    let (s, k) = upload(&h, &h.member_token, "keystore", "file", KEYSTORE.as_bytes()).await;
    assert_eq!(s, 200, "{k}");
    assert_eq!(k["sha256"], cimon_core::sha256_hex(KEYSTORE.as_bytes()));
    let keystore = k["handle"].as_str().unwrap().to_string();
    let (s, p) = upload(&h, &h.member_token, "store-password", "text", format!("{PASSWORD}\r\n").as_bytes()).await;
    assert_eq!(s, 200, "{p}");
    assert!(p.get("sha256").is_none() && p.get("bytes").is_none(), "a password's hash or length is never shown: {p}");
    let password = p["handle"].as_str().unwrap().to_string();
    assert_eq!(h.app.store.upload(&password).unwrap().unwrap().bytes, PASSWORD.len() as i64, "the line ending is not part of the value");
    let blob = h.app.layout.uploads().join(&keystore);
    assert!(blob.is_file());
    assert!(!std::fs::read(&blob).unwrap().windows(14).any(|w| w == b"KEYSTORE-BYTES"), "kept protected, not as sent");
    let both = json!({"keystore": keystore, "store-password": password});

    // plan says what is needed and what this call hands over
    let (plan, err) = call_any(&c, "plan", json!({"project": P, "tool": "build", "steps": ["release"]})).await;
    assert!(!err, "{plan}");
    assert!(plan["uploads"][0]["state"].as_str().unwrap().starts_with("필요"), "{plan:#}");
    let (plan, _) = call_any(&c, "plan", json!({"project": P, "tool": "build", "steps": ["release"], "uploads": both})).await;
    assert!(plan["uploads"].as_array().unwrap().iter().all(|u| u["state"].as_str().unwrap().starts_with("받음")), "{plan:#}");

    // refused before a job exists
    let refused = |v: &Value| v["error"].as_str().unwrap_or_default().to_string();
    let (v, err) = call_any(&c, "build", json!({"project": P, "steps": ["release"]})).await;
    assert!(err && refused(&v) == "upload_missing", "{v}");
    assert!(v["message"].as_str().unwrap().contains("upload.ps1 keystore=<파일> store-password=env:<변수>|prompt"), "{v}");
    let (v, _) = call_any(&c, "build", json!({"project": P, "steps": ["release"], "uploads": {"keystore": keystore}})).await;
    assert_eq!(refused(&v), "upload_missing", "{v}");
    let (v, _) = call_any(&c, "build", json!({"project": P, "steps": ["build"], "uploads": {"keystore": keystore}})).await;
    assert_eq!(refused(&v), "upload_unused", "{v}");
    let (v, _) = call_any(&c, "build", json!({"project": P, "steps": ["release"], "uploads": {"keystore": keystore, "store-password": password, "cert": password}})).await;
    assert_eq!(refused(&v), "unknown_upload", "{v}");
    let (v, _) = call_any(&c, "build", json!({"project": P, "ref": "feature/x", "steps": ["release"], "uploads": both})).await;
    assert_eq!(refused(&v), "upload_ref_not_allowed", "a branch the upload does not allow: {v}");
    let main = h.app.recipes.main_commit().unwrap();
    let (e, err) = call_any(&c, "recipe_edit", json!({"branch": "recipe/notes", "base": main, "message": "notes", "changes": [{"path": "docs/notes.md", "content": "x"}]})).await;
    assert!(!err, "{e}");
    let (v, _) = call_any(&c, "build", json!({"project": P, "recipe_ref": "recipe/notes", "steps": ["release"], "uploads": both})).await;
    assert_eq!(refused(&v), "upload_trial_recipe", "{v}");
    let (v, _) = call_any(&c, "build", json!({"project": P, "steps": ["release"], "uploads": {"keystore": password, "store-password": keystore}})).await;
    assert_eq!(refused(&v), "upload_invalid", "the wrong kind: {v}");
    let (v, _) = call_any(&admin, "build", json!({"project": P, "steps": ["release"], "uploads": both})).await;
    assert_eq!(refused(&v), "upload_invalid", "someone else's upload: {v}");

    // an earlier job planted a Gradle init script for the next job id — it is gone
    let next: i64 = h.app.store.tx(|tx| Ok(tx.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM jobs", [], |r| r.get(0))?)).unwrap();
    let planted = h.app.layout.home_job(next, "caches").join("gradle").join("init.d");
    std::fs::create_dir_all(&planted).unwrap();
    std::fs::write(planted.join("planted.gradle"), "x").unwrap();
    std::fs::create_dir_all(h.app.layout.tmp_job(next)).unwrap();

    // the job: the step gets the file and the value, the log does not show the value
    let task = start_task(&c, "build", json!({"project": P, "steps": ["release"], "uploads": both})).await;
    let (_, v, _) = wait_task(&c, &task, 120).await;
    assert_eq!(v["verdict"], "pass", "{v:#}");
    assert_eq!(v["clean"], true, "{v:#}");
    assert_eq!(v["workspace"]["mode"], "clean", "{v:#}");
    assert_eq!(v["uploads"]["keystore"]["sha256"], cimon_core::sha256_hex(KEYSTORE.as_bytes()), "{v:#}");
    assert_eq!(v["uploads"]["store-password"]["by"], "member-b", "{v:#}");
    assert!(v["warnings"].as_array().unwrap().iter().any(|w| w["source"] == "upload"), "{v:#}");
    assert_eq!(cimon_core::parse_job_label(v["job_id"].as_str().unwrap()), Some(next));
    let planted_warnings = v["warnings"].as_array().unwrap().iter().filter(|w| w["message"].as_str().unwrap_or_default().contains("작업 시작 전부터 있었다")).count();
    assert_eq!(planted_warnings, 2, "homes and tmp were there before the job: {v:#}");
    assert!(v["uploads"]["store-password"].get("sha256").is_none() && v["uploads"]["store-password"].get("bytes").is_none(), "{v:#}");
    let (log, _) = call_any(&c, "job_log", json!({"job": v["job_id"], "step": "release"})).await;
    let log = log["log"].as_str().unwrap().to_string();
    assert!(log.contains("release ok"), "{log}");
    assert!(log.contains("password is ***"), "{log}");
    assert!(!log.contains(PASSWORD), "{log}");
    let id = cimon_core::parse_job_label(v["job_id"].as_str().unwrap()).unwrap();
    assert!(log.contains(&format!("homes\\j{id}\\caches")), "the job's own caches: {log}");
    let s = serde_json::to_string(&v).unwrap();
    assert!(!s.contains(PASSWORD) && !s.contains(&password) && !s.contains(&keystore), "the result holds fingerprints only");

    // once: the bytes are gone, the handle is spent
    assert!(!blob.exists(), "deleted with the job");
    assert!(!h.app.layout.homes_job_dir(id).exists());
    assert!(h.app.store.upload(&keystore).unwrap().unwrap().deleted_at.is_some());
    let (v, _) = call_any(&c, "build", json!({"project": P, "steps": ["release"], "uploads": both})).await;
    assert_eq!(refused(&v), "upload_invalid", "a spent handle: {v}");

    // nobody took it in time
    let (_, k2) = upload(&h, &h.member_token, "keystore", "file", KEYSTORE.as_bytes()).await;
    let late = k2["handle"].as_str().unwrap().to_string();
    h.app.store.tx(|tx| Ok(tx.execute("UPDATE uploads SET expires_at = '2000-01-01T00:00:00Z' WHERE handle = ?1", [&late])?)).unwrap();
    let (plan, _) = call_any(&c, "plan", json!({"project": P, "tool": "build", "steps": ["release"], "uploads": {"keystore": late, "store-password": password}})).await;
    assert!(plan["uploads"][0]["state"].as_str().unwrap().contains("기다리는 업로드가 아니다"), "{plan:#}");
    cimon_engine::uploads::sweep(&h.app);
    assert!(!h.app.layout.uploads().join(&late).exists());
    c.cancel().await.unwrap();
    admin.cancel().await.unwrap();
}
