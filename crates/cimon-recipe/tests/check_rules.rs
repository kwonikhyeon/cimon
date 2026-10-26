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
fn uv_tool_python_pin() {
    let with = |extra: &str| {
        let mut t = examples();
        let cat = t.read_str("toolchains.toml").unwrap().to_string();
        t.insert("toolchains.toml", format!("{cat}\n[kind.pytool]\ntier = \"managed\"\ninstaller = \"uv-tool\"\npackage = \"pytool\"\n{extra}probe = {{ run = [\"pytool\", \"--version\"], expect = \"pytool {{version}}\" }}\n"));
        check_tree(&t)
    };
    let r = with("");
    assert!(r.ok(), "a missing pin is not an error: {:#?}", r.errors);
    assert!(r.warnings.iter().any(|d| d.message.contains("uv-tool without python")), "{:#?}", r.warnings);
    let r = with("python = \"3.13\"\n");
    assert!(r.errors.iter().any(|d| d.message.contains("exact x.y.z version")), "{:#?}", r.errors);
    let r = with("python = \"3.13.14\"\n");
    assert!(r.ok() && !r.warnings.iter().any(|d| d.message.contains("uv-tool")), "{:#?} {:#?}", r.errors, r.warnings);
}

#[test]
fn ignore_version_files_rules() {
    const R: &str = "projects/example-app/recipe.toml";
    const BASE: &str = "rust        = { version_file = \"backend/rust-toolchain.toml\" }";
    let with = |line: &str| {
        let mut t = examples();
        replace(&mut t, R, BASE, line);
        check_tree(&t)
    };
    let r = with("rust = { version_file = \"backend/rust-toolchain.toml\", ignore_version_files = [\"tools/pack/rust-toolchain.toml\"] }");
    assert!(r.ok(), "{:#?}", r.errors);
    let r = with("rust = { version_file = \"backend/rust-toolchain.toml\", ignore_version_files = [\"backend/rust-toolchain.toml\"] }");
    assert!(r.errors.iter().any(|d| d.message.contains("cannot name version_file")), "{:#?}", r.errors);
    let r = with("rust = { version_file = \"backend/rust-toolchain.toml\", ignore_version_files = [\"../other/rust-toolchain.toml\"] }");
    assert!(r.errors.iter().any(|d| d.message.contains("must be a relative path")), "{:#?}", r.errors);
    let r = with("rust = { version = \"1.94.0\", ignore_version_files = [\"tools/pack/rust-toolchain.toml\"] }");
    assert!(r.errors.iter().any(|d| d.message.contains("only goes with version_file")), "{:#?}", r.errors);
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

#[test]
fn param_without_default_and_path_rules() {
    let mut t = examples();
    replace(&mut t, "projects/example-app/recipe.toml", "default = \"http://10.0.2.2:8800\"\n", "");
    replace(&mut t, "projects/example-app/recipe.toml", "run = [\"./gradlew.bat\", \":app:assembleDebug\"", "run = [\"./gradlew.bat\", \"-Purl=${param.backend_url}\", \":app:assembleDebug\"");
    replace(&mut t, "projects/example-app/recipe.toml", "cwd = \"backend\"\ntimeout_min = 30\ntags = [\"rust\"]", "cwd = \"${ws}/../x\"\ntimeout_min = 30\ntags = [\"rust\"]");
    replace(&mut t, "projects/example-app/recipe.toml", "cwd = \"backend\"\ntimeout_min = 30\ntags = [\"rust\", \"unit\"]", "cwd = \"./backend\"\ntimeout_min = 30\ntags = [\"rust\", \"unit\"]");
    replace(&mut t, "projects/example-app/recipe.toml", "\":shared:allTests\", \"--console=plain\"]", "\":shared:allTests\", \"--console=plain\", \"${vars.gradle_dir\"]");
    let r = check_tree(&t);
    let has = |s: &str| r.errors.iter().any(|d| d.message.contains(s));
    assert!(has("needs a default"), "{:#?}", r.errors);
    assert!(has("cwd \"${ws}/../x\""), "{:#?}", r.errors);
    assert!(!has("cwd \"./backend\""), "{:#?}", r.errors);
    assert!(has("unterminated"), "{:#?}", r.errors);
}

#[test]
fn secrets_only_in_env_values() {
    const R: &str = "projects/example-app/recipe.toml";
    let mut t = examples();
    replace(&mut t, R, "GRADLE_OPTS = \"-Dorg.gradle.daemon=false\"\n", "GRADLE_OPTS = \"-Dorg.gradle.daemon=false\"\nREGISTRY_TOKEN = \"${secret.registry}\"\n");
    replace(&mut t, R, "cwd = \"backend\"\ntimeout_min = 30\ntags = [\"rust\"]", "cwd = \"backend\"\nenv = { CARGO_REGISTRIES_CO_TOKEN = \"Bearer ${secret.cargo-co}\" }\ntimeout_min = 30\ntags = [\"rust\"]");
    let r = check_tree(&t);
    assert!(r.ok(), "{:#?}", r.errors);
    replace(&mut t, R, "\":shared:allTests\", \"--console=plain\"]", "\":shared:allTests\", \"--console=plain\", \"-Ptoken=${secret.registry}\"]");
    replace(&mut t, R, "REGISTRY_TOKEN = \"${secret.registry}\"", "REGISTRY_TOKEN = \"${secret.Registry}\"");
    let r = check_tree(&t);
    let has = |s: &str| r.errors.iter().any(|d| d.message.contains(s));
    assert!(has("${secret.registry}: a secret may only be an env value"), "{:#?}", r.errors);
    assert!(has("${secret.Registry}: a secret name is"), "{:#?}", r.errors);
    assert_eq!(r.errors.len(), 2, "{:#?}", r.errors);
}

/// `[upload.<name>]` (recipe-spec §8a): declared once, reaching only its steps — a file
/// as a path anywhere in them, a text value only as an env value.
#[test]
fn uploads_reach_only_their_steps() {
    const R: &str = "projects/example-app/recipe.toml";
    const UPLOADS: &str = r#"
[upload.keystore]
file = "release.jks"
applies_to = ["release-apk"]
refs = ["main", "release/**"]

[upload.keystore-password]
kind = "text"
applies_to = ["release-apk"]

[[step]]
id = "release-apk"
kind = "package"
run = ["./gradlew.bat", ":app:assembleRelease", "-Pstore=${upload.keystore}", "-Pdir=${upload.keystore.dir}"]
cwd = "${vars.gradle_dir}"
env = { STORE = "${upload.keystore}", STORE_PASSWORD = "${upload.keystore-password}" }
"#;
    let with = |edit: &dyn Fn(String) -> String| {
        let mut t = examples();
        let s = t.read_str(R).unwrap().to_string();
        t.insert(R, edit(format!("{s}{UPLOADS}")));
        check_tree(&t)
    };
    let r = with(&|s| s);
    assert!(r.ok(), "{:#?}", r.errors);
    assert!(r.warnings.is_empty(), "{:#?}", r.warnings);
    let u = &r.resolved["example-app"].uploads;
    assert_eq!(u.keys().collect::<Vec<_>>(), ["keystore", "keystore-password"]);

    let cases: &[(&str, &str, &str)] = &[
        ("\"-Pdir=${upload.keystore.dir}\"]", "\"-Ppw=${upload.keystore-password}\"]", "a text upload may only be an env value"),
        ("STORE_PASSWORD = \"${upload.keystore-password}\"", "STORE_PASSWORD = \"${upload.keystore-password.dir}\"", "a text upload is a value, not a file"),
        ("STORE = \"${upload.keystore}\"", "STORE = \"${upload.nope}\"", "unknown upload ${upload.nope}"),
        ("STORE = \"${upload.keystore}\"", "STORE = \"${upload.keystore.path}\"", "an upload has no field \"path\""),
        ("GRADLE_OPTS = \"-Dorg.gradle.daemon=false\"\n", "GRADLE_OPTS = \"-Dorg.gradle.daemon=false\"\nSTORE = \"${upload.keystore}\"\n", "an upload reaches only the steps in its applies_to"),
        ("run = [\"./gradlew.bat\", \":app:assembleDebug\", \"--console=plain\"]", "run = [\"./gradlew.bat\", \":app:assembleDebug\", \"-Ps=${upload.keystore}\"]", "step \"build-apk\" is not in [upload.keystore].applies_to"),
        ("applies_to = [\"release-apk\"]\nrefs", "applies_to = [\"release-apk\", \"test-backend\"]\nrefs", "\"test-backend\" is a default step"),
        ("applies_to = [\"release-apk\"]\nrefs", "applies_to = [\"release-apkk\"]\nrefs", "applies_to names unknown step \"release-apkk\""),
        ("kind = \"text\"", "kind = \"blob\"", "kind must be file or text"),
        ("file = \"release.jks\"", "file = \"../release.jks\"", "must be a plain file name"),
        ("kind = \"text\"", "file = \"release.jks\"", "is also upload keystore's"),
        ("kind = \"text\"", "kind = \"text\"\nfile = \"pw.txt\"", "file is for kind = \"file\" only"),
        ("refs = [\"main\", \"release/**\"]", "refs = []", "refs is empty"),
        ("[upload.keystore-password]", "[upload.Keystore-Password]", "a name is [a-z0-9][a-z0-9_-]*"),
    ];
    for (from, to, want) in cases {
        let r = with(&|s: String| {
            assert!(s.contains(from), "{from:?}");
            s.replacen(from, to, 1)
        });
        assert!(r.errors.iter().any(|d| d.message.contains(want)), "{to:?} should fail with {want:?}: {:#?}", r.errors);
    }

    // a preset or an include cannot ask for uploads
    let mut t = examples();
    let s = t.read_str(R).unwrap().to_string();
    t.insert(R, format!("{s}{UPLOADS}").replacen("schema = 1\n", "schema = 1\nextends = [\"p\"]\n", 1));
    t.insert("presets/p.toml", "schema = 1\n[upload.cert]\napplies_to = [\"release-apk\"]\n");
    let r = check_tree(&t);
    assert!(r.errors.iter().any(|d| d.message.contains("a preset may not use upload")), "{:#?}", r.errors);
}

#[test]
fn layering_holes_are_errors() {
    let mut t = examples();
    let recipe = t.read_str("projects/example-app/recipe.toml").unwrap().to_string();
    t.insert(
        "projects/example-app/recipe.toml",
        recipe.replacen("[vars]", "include = [\"extra.toml\"]\n\n[vars]", 1) + "\n[[step]]\nid = \"tset-backend\"\ndisabled = true\n",
    );
    t.insert("projects/example-app/extra.toml", "schema = 2\n[[step]]\nid = \"extra\"\nkind = \"build\"\nrun = [\"y\"]\n");
    let r = check_tree(&t);
    let has = |s: &str| r.errors.iter().any(|d| d.message.contains(s));
    assert!(has("no earlier layer defines"), "{:#?}", r.errors);
    assert!(has("unknown schema 2"), "{:#?}", r.errors);
}

#[test]
fn windows_aliases_and_new_project_on_broken_preset() {
    let mut t = examples();
    t.insert("scripts/judge.py.", "x");
    let r = check_tree(&t);
    assert!(r.errors.iter().any(|d| d.message.contains("not valid on Windows")), "{:#?}", r.errors);

    // a preset error reached by a new project is a new error for that project
    let mut base = examples();
    base.insert("presets/broken.toml", "schema = 1\n[[step]]\nid = \"p\"\nkind = \"build\"\nrun = [\"x\", \"${vars.nope}\"]\n");
    let pt = base.read_str("projects/example-app/project.toml").unwrap().to_string();
    base.insert("projects/a/project.toml", pt.replace("example-app", "a"));
    base.insert("projects/a/recipe.toml", "schema = 1\nextends = [\"broken\"]\n");
    let mut new = base.clone();
    new.insert("projects/c/project.toml", pt.replace("example-app", "c"));
    new.insert("projects/c/recipe.toml", "schema = 1\nextends = [\"broken\"]\n");
    let (before, after) = (check_tree(&base), check_tree(&new));
    let fresh = after.new_errors_since(&before);
    assert!(fresh.iter().any(|d| d.project.as_deref() == Some("c")), "{fresh:#?}");
}
