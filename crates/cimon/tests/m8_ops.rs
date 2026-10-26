//! M8: operations — setup dry run, doctor, drain, disk floor, stale offsite backup
//! warnings, `recipe check --main`, restore from a bundle.

mod common;

use cimon_recipe::tree::MemTree;
use common::*;
use serde_json::json;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cimon"))
}

fn write_config(dir: &std::path::Path, extra: &str) -> std::path::PathBuf {
    let root = dir.join("cimon");
    std::fs::create_dir_all(&root).unwrap();
    let cfg = dir.join("cimon.toml");
    std::fs::write(&cfg, format!("[tailscale]\nrequire_funnel_off = false\n[paths]\nroot = '{}'\n[accounts]\nbuild = \"\"\n{extra}", root.display())).unwrap();
    cfg
}

fn small_tree() -> MemTree {
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert("projects/m8/project.toml", "schema = 1\nid = \"m8\"\nrepo = \"https://example.invalid/m8.git\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\n");
    t.insert("projects/m8/recipe.toml", "schema = 1\n[[step]]\nid = \"b\"\nkind = \"build\"\nrun = [\"cmd\", \"/c\", \"echo\"]\n");
    t
}

#[test]
fn setup_dry_run_lists_steps_and_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let cfg = write_config(d.path(), "[backup]\noffsite = \"none\"\n");
    let out = bin().args(["--config", &cfg.to_string_lossy(), "setup", "--dry-run"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("dry run"), "{text}");
    assert!(text.contains("Windows service cimon"), "{text}");
    assert!(text.contains("ACLs under"), "{text}");
    assert!(!d.path().join("cimon").join("state").exists(), "dry run must not create directories");
}

#[test]
fn doctor_reports_failures_with_exit_code() {
    let d = tempfile::tempdir().unwrap();
    let cfg = write_config(d.path(), "[backup]\noffsite = \"none\"\n");
    let out = bin().args(["--config", &cfg.to_string_lossy(), "doctor"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("OK   job store and recipe store open"), "{text}");
    assert!(text.contains("FAIL recipe repository"), "{text}");
    assert!(text.contains("single-account mode"), "{text}");
}

#[test]
fn recipe_check_main_and_restore_from_bundle() {
    let d = tempfile::tempdir().unwrap();
    let offsite = d.path().join("offsite");
    let cfg = write_config(d.path(), &format!("[backup]\noffsite = '{}'\n", offsite.display()));
    let src = d.path().join("src");
    small_tree().write_to(&src).unwrap();
    let out = bin().args(["--config", &cfg.to_string_lossy(), "recipe", "import", &src.to_string_lossy(), "--init", "--author", "admin-a"]).output().unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let out = bin().args(["--config", &cfg.to_string_lossy(), "recipe", "check", "--main"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let bundle = std::fs::read_dir(&offsite).unwrap().flatten().next().unwrap().path();
    // restore into a fresh root
    let d2 = tempfile::tempdir().unwrap();
    let cfg2 = write_config(d2.path(), "[backup]\noffsite = \"none\"\n");
    let out = bin().args(["--config", &cfg2.to_string_lossy(), "recipe", "restore", &bundle.to_string_lossy()]).output().unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let a = cimon_engine::App::open(cimon_core::Config::load(&cfg).unwrap()).unwrap();
    let b = cimon_engine::App::open(cimon_core::Config::load(&cfg2).unwrap()).unwrap();
    assert_eq!(a.recipes.main_commit(), b.recipes.main_commit());
}

#[tokio::test]
async fn draining_refuses_new_jobs() {
    repos_dir();
    make_repo("m8", &[("x", "1")]);
    let h = Harness::start("").await;
    h.app.recipes.import_init(&small_tree(), "admin-a", "init").unwrap();
    std::fs::write(h.app.layout.draining_flag(), "now").unwrap();
    let c = h.client(&h.member_token).await;
    let (v, err) = call(&c, "build", json!({"project": "m8"})).await;
    assert!(err);
    assert_eq!(v["error"], "draining");
    std::fs::remove_file(h.app.layout.draining_flag()).unwrap();
    let (v, err) = call(&c, "build", json!({"project": "m8"})).await;
    assert!(!err, "{v}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn disk_floor_refuses_and_starts_cleanup() {
    repos_dir();
    let h = Harness::start("[disk]\nmin_free_gb = 100000000\n").await;
    h.app.recipes.import_init(&small_tree(), "admin-a", "init").unwrap();
    let c = h.client(&h.member_token).await;
    let (v, err) = call(&c, "build", json!({"project": "m8"})).await;
    assert!(err);
    assert_eq!(v["error"], "disk_low");
    assert!(v["message"].as_str().unwrap().contains("cleanup has started"), "{v}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn stale_offsite_copy_warns_on_recipe_tools() {
    let offsite = tempfile::tempdir().unwrap();
    let h = Harness::start(&format!("[backup]\noffsite = '{}'\n", offsite.path().display())).await;
    h.app.recipes.import_init(&small_tree(), "admin-a", "init").unwrap();
    cimon_engine::backup::backup_recipes(&h.app, "test").unwrap();
    let c = h.client(&h.member_token).await;
    let (v, _) = call(&c, "recipe_show", json!({})).await;
    assert!(v.get("backup_warnings").is_none(), "fresh backup must not warn: {v}");
    // pretend the last offsite copy is 30 hours old
    h.app.store.tx(|tx| {
        tx.execute("UPDATE backups SET offsite_ok_at = ?1 WHERE kind = 'recipes'", [cimon_core::after_secs(-30 * 3600)])?;
        Ok(())
    })
    .unwrap();
    let (v, _) = call(&c, "recipe_show", json!({})).await;
    let w = v["backup_warnings"][0].as_str().unwrap();
    assert!(w.contains("30시간 전"), "{w}");
    let health: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/healthz", h.base))
        .bearer_auth(&h.member_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(health["warnings"][0].as_str().unwrap().contains("30시간 전"));
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn janitor_does_not_follow_junctions_out_of_build_trees() {
    repos_dir();
    // cache limit 0: every cache entry is over the limit and would be evicted
    let h = Harness::start("[disk]\ncache_max_gb_per_project = 0\n").await;
    let l = &h.app.layout;
    // a finished job's artifacts, which only retention may delete
    let art = l.job_artifacts(999_999);
    std::fs::create_dir_all(&art).unwrap();
    std::fs::write(art.join("app.zip"), "x").unwrap();
    let junction = |link: &std::path::Path, target: &std::path::Path| {
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        assert!(Command::new("cmd").args(["/c", "mklink", "/J"]).arg(link).arg(target).output().unwrap().status.success(), "mklink /J needs no privilege");
    };
    // build steps swap a project folder under ws\ and a tool folder under caches\
    // for junctions to artifacts\ (where j<id> folders also live)
    junction(&l.ws().join("evil"), &l.artifacts());
    junction(&l.root.join("caches").join("p").join("npm"), &l.artifacts());
    cimon_engine::janitor::startup(&h.app);
    cimon_engine::janitor::retention_pass(&h.app);
    assert!(art.join("app.zip").exists(), "the janitor followed a junction out of a build-writable tree");
}
