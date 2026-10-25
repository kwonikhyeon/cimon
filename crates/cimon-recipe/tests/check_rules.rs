//! Static-check rules against the format examples and deliberately broken copies.

use cimon_recipe::{check_tree, MemTree, Tree};
use std::path::PathBuf;

fn examples() -> MemTree {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/recipes");
    let mut t = MemTree::from_dir(&dir).unwrap();
    for v in t.files.values_mut() {
        if let Ok(s) = String::from_utf8(v.clone()) {
            *v = s.replace("\r\n", "\n").into_bytes();
        }
    }
    t
}

fn replace(t: &mut MemTree, path: &str, from: &str, to: &str) {
    let s = t.read_str(path).unwrap().to_string();
    assert!(s.contains(from), "{path} does not contain {from:?}");
    t.insert(path, s.replacen(from, to, 1));
}

#[test]
fn examples_pass() {
    let r = check_tree(&examples());
    assert!(r.ok(), "errors: {:#?}", r.errors);
    assert!(r.warnings.is_empty(), "warnings: {:#?}", r.warnings);
    assert!(r.resolved.contains_key("example-app"));
}

#[test]
fn version_any_expect_must_be_literal() {
    let mut t = examples();
    replace(&mut t, "toolchains.toml", "expect = \"GNU bash, version\"", "expect = \"GNU bash, version {version_probe}\"");
    let r = check_tree(&t);
    assert!(r.errors.iter().any(|d| d.message.contains("fixed string")), "{:#?}", r.errors);
}

#[test]
fn managed_probe_needs_version() {
    let mut t = examples();
    replace(&mut t, "toolchains.toml", "expect = \"rustc {version}\"", "expect = \"rustc\"");
    let r = check_tree(&t);
    assert!(r.errors.iter().any(|d| d.message.contains("must contain {version}")), "{:#?}", r.errors);
}

#[test]
fn script_ref_must_be_sha() {
    let mut t = examples();
    let cat = t.read_str("toolchains.toml").unwrap().to_string();
    t.insert(
        "toolchains.toml",
        format!("{cat}\n[kind.tool]\ntier = \"managed\"\ninstaller = \"script\"\nscript = {{ from_project = \"example-app\", ref = \"main\", path = \"install.ps1\" }}\nprobe = {{ run = [\"tool\"], expect = \"{{version}}\" }}\n"),
    );
    let r = check_tree(&t);
    assert!(r.errors.iter().any(|d| d.message.contains("40-character commit SHA")), "{:#?}", r.errors);
}

#[test]
fn param_rules() {
    let mut t = examples();
    replace(&mut t, "projects/example-app/recipe.toml", "applies_to = [\"build-apk\"]\n", "");
    let r = check_tree(&t);
    assert!(r.errors.iter().any(|d| d.message.contains("applies_to is required")), "{:#?}", r.errors);
}

#[test]
fn clean_env_replacement_warns() {
    let mut t = examples();
    replace(&mut t, "projects/example-app/recipe.toml", "\"-Dorg.gradle.daemon=false -Dorg.gradle.caching=false\"", "\"-Dorg.gradle.caching=false\"");
    let r = check_tree(&t);
    assert!(r.ok());
    assert!(r.warnings.iter().any(|d| d.message.contains("clean_env GRADLE_OPTS")), "{:#?}", r.warnings);
}

#[test]
fn exit_judge_on_test_warns() {
    let mut t = examples();
    replace(
        &mut t,
        "projects/example-app/recipe.toml",
        "judge = { type = \"junit\", reports = \"app/app/build/outputs/androidTest-results/**/*.xml\", min_tests = 1 }",
        "",
    );
    let r = check_tree(&t);
    assert!(r.warnings.iter().any(|d| d.message.contains("0 tests ran")), "{:#?}", r.warnings);
}

#[test]
fn unknown_key_and_unknown_after() {
    let mut t = examples();
    replace(&mut t, "projects/example-app/recipe.toml", "timeout_min = 30\ntags = [\"rust\"]", "timeot_min = 30\ntags = [\"rust\"]");
    let r = check_tree(&t);
    let e = r.errors.iter().find(|d| d.message.contains("timeot_min")).expect("unknown key");
    assert!(e.line.is_some());

    let mut t = examples();
    replace(&mut t, "projects/example-app/recipe.toml", "id = \"build-apk\"\n", "id = \"build-apk\"\nafter = [\"nope\"]\n");
    let r = check_tree(&t);
    assert!(r.errors.iter().any(|d| d.message.contains("unknown step \"nope\"")), "{:#?}", r.errors);
}

#[test]
fn account_env_needs_isolate() {
    let mut t = examples();
    let s = t.read_str("projects/example-app/recipe.toml").unwrap().to_string();
    t.insert("projects/example-app/recipe.toml", s.replacen("\n[env]\n", "\n[account_env]\nAPP_HOME = \"job\"\n\n[env]\n", 1));
    let r = check_tree(&t);
    assert!(r.errors.iter().any(|d| d.message.contains("[account_env] APP_HOME")), "{:#?}", r.errors);
}

#[test]
fn new_errors_only() {
    let mut base = examples();
    replace(&mut base, "projects/example-app/recipe.toml", "id = \"build-apk\"\n", "id = \"build-apk\"\nafter = [\"nope\"]\n");
    let base_report = check_tree(&base);
    assert!(!base_report.ok());
    // an unrelated edit keeps the old error but introduces none
    let mut edited = base.clone();
    edited.insert("projects/example-app/notes.md", "notes");
    let r = check_tree(&edited);
    assert!(r.new_errors_since(&base_report).is_empty());
    assert_eq!(r.preexisting_errors(&base_report).len(), base_report.errors.len());
}

#[test]
fn ranges_rejected() {
    let mut t = examples();
    replace(&mut t, "projects/example-app/recipe.toml", "jdk         = { version = \"17.0.19+10\" }", "jdk         = { version = \"17\" }");
    let r = check_tree(&t);
    assert!(r.errors.iter().any(|d| d.message.contains("exact version required")), "{:#?}", r.errors);
}
