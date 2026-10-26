//! Semantic diff between two recipe trees (review-ui §4.4): what will run differently,
//! per project, with risk flags. Promote conditions (design §6.5) are computed from it.

use crate::catalog::Catalog;
use crate::model::Kind;
use crate::refs::{project_refs, project_texts, ref_files, RecipeRef, Scope};
use crate::resolve::{parse_project, resolve_project, Resolved};
use crate::tree::Tree;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// repo / allowed_refs / sibling / new project — who can be built
    Permission,
    /// commands, env, scripts that run on the build machine
    Exec,
    /// where binaries come from
    Supply,
    /// what runs and what counts as passing
    Behavior,
    /// a judgement got looser
    Weaken,
}

impl Risk {
    pub fn label(&self) -> &'static str {
        match self {
            Risk::Permission => "권한",
            Risk::Exec => "실행",
            Risk::Supply => "공급망",
            Risk::Behavior => "동작",
            Risk::Weaken => "동작·완화",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldChange {
    pub field: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub from: Value,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub to: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<Risk>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepChange {
    pub id: String,
    /// added | removed | changed
    pub change: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub fields: Vec<FieldChange>,
    pub risks: BTreeSet<Risk>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct ProjectDiff {
    pub id: String,
    /// added | removed | changed
    pub change: &'static str,
    /// `project.toml` of a project that exists on main changed — cannot be tried.
    pub project_toml_changed: bool,
    pub project_fields: Vec<FieldChange>,
    pub steps: Vec<StepChange>,
    /// Project-wide settings changed (toolchains, env, vars, workspace…).
    pub settings: Vec<FieldChange>,
    /// Catalog kinds this project uses whose entry changed.
    pub catalog_kinds: Vec<String>,
    /// Steps a trial must include: added or changed (directly, via script or fixture).
    pub changed_steps: Vec<String>,
    /// A settings change touches every step — a trial must run the default build+test.
    pub requires_default_run: bool,
    pub risks: BTreeSet<Risk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KindChange {
    pub kind: String,
    pub change: &'static str,
    pub risk: Risk,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct SemanticDiff {
    pub projects: Vec<ProjectDiff>,
    pub catalog: Vec<KindChange>,
    /// Files that changed, for the file-diff view.
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SemanticDiff {
    pub fn affected(&self) -> Vec<&ProjectDiff> {
        self.projects.iter().collect()
    }
    pub fn risks(&self) -> BTreeSet<Risk> {
        let mut r: BTreeSet<Risk> = self.projects.iter().flat_map(|p| p.risks.iter().copied()).collect();
        r.extend(self.catalog.iter().map(|k| k.risk));
        r
    }
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

pub fn semantic_diff(old: &dyn Tree, new: &dyn Tree) -> SemanticDiff {
    let mut out = SemanticDiff::default();
    let all_files: BTreeSet<&str> = old.files().into_iter().chain(new.files()).collect();
    out.files = all_files.into_iter().filter(|f| old.read(f) != new.read(f)).map(String::from).collect();
    if out.files.is_empty() {
        return out;
    }
    let old_cat = old.read_str("toolchains.toml").and_then(|t| Catalog::parse(t).ok());
    let new_cat = new.read_str("toolchains.toml").and_then(|t| Catalog::parse(t).ok());
    let mut changed_kinds: BTreeSet<String> = BTreeSet::new();
    match (&old_cat, &new_cat) {
        (Some(o), Some(n)) => {
            let names: BTreeSet<&String> = o.hashes.keys().chain(n.hashes.keys()).collect();
            for k in names {
                let change = match (o.hash(k), n.hash(k)) {
                    (None, Some(_)) => "added",
                    (Some(_), None) => "removed",
                    (Some(a), Some(b)) if a != b => "changed",
                    _ => continue,
                };
                changed_kinds.insert(k.clone());
                out.catalog.push(KindChange { kind: k.clone(), change, risk: Risk::Supply });
            }
        }
        (_, None) => out.error = Some("toolchains.toml does not parse on the new side".into()),
        (None, Some(_)) => {}
    }

    let ids: BTreeSet<String> = old.project_ids().into_iter().chain(new.project_ids()).collect();
    let changed_files = out.files.clone();
    for id in ids {
        let o = resolve_project(old, &id).ok();
        let n = resolve_project(new, &id);
        let in_old = old.exists(&format!("projects/{id}/project.toml"));
        let in_new = new.exists(&format!("projects/{id}/project.toml"));
        let mut pd = ProjectDiff { id: id.clone(), change: "changed", ..Default::default() };
        match (in_old, in_new) {
            (false, true) => {
                pd.change = "added";
                pd.risks.insert(Risk::Permission);
            }
            (true, false) => {
                pd.change = "removed";
                pd.risks.insert(Risk::Behavior);
                out.projects.push(pd);
                continue;
            }
            _ => {}
        }
        let n = match n {
            Ok(n) => n,
            Err(errs) => {
                pd.error = Some(errs.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("; "));
                if touches(&out.files, &id, None) {
                    out.projects.push(pd);
                }
                continue;
            }
        };
        diff_project(old, new, o.as_ref(), &n, &changed_kinds, &changed_files, &mut pd);
        let pt = format!("projects/{id}/project.toml");
        if in_old && old.read(&pt) != new.read(&pt) {
            match (parse_project(old, &id), parse_project(new, &id)) {
                (Ok(op), Ok(np)) => {
                    if op != np {
                        pd.project_toml_changed = true;
                        let ov = serde_json::to_value(&op).unwrap();
                        let nv = serde_json::to_value(&np).unwrap();
                        for f in field_changes(&ov, &nv, "") {
                            let risk = if f.field.starts_with("repo") || f.field.starts_with("allowed_refs") || f.field.starts_with("sibling") {
                                Risk::Permission
                            } else {
                                Risk::Behavior
                            };
                            pd.risks.insert(risk);
                            pd.project_fields.push(FieldChange { risk: Some(risk), ..f });
                        }
                    }
                }
                // one side does not parse: nothing can be compared, so assume the worst
                _ => {
                    pd.project_toml_changed = true;
                    pd.risks.insert(Risk::Permission);
                    pd.project_fields.push(FieldChange { field: "project.toml".into(), from: Value::Null, to: Value::Null, risk: Some(Risk::Permission) });
                }
            }
        }
        let has_change = pd.change == "added"
            || pd.project_toml_changed
            || !pd.steps.is_empty()
            || !pd.settings.is_empty()
            || !pd.catalog_kinds.is_empty();
        if has_change {
            out.projects.push(pd);
        }
    }
    out
}

fn touches(files: &[String], id: &str, _r: Option<&Resolved>) -> bool {
    files.iter().any(|f| f.starts_with(&format!("projects/{id}/")))
}

/// Did any file a reference reaches change between the trees?
fn ref_changed(old: &dyn Tree, new: &dyn Tree, r: &RecipeRef) -> bool {
    let files: BTreeSet<String> = ref_files(old, r).into_iter().chain(ref_files(new, r)).collect();
    files.iter().any(|f| old.read(f) != new.read(f))
}

fn ref_label(r: &RecipeRef) -> String {
    if r.path.is_empty() {
        "script:(레시피 저장소 전체)".into()
    } else {
        format!("script:{}{}", r.path, if r.dynamic { "/…" } else { "" })
    }
}

#[allow(clippy::too_many_arguments)]
fn diff_project(old: &dyn Tree, new: &dyn Tree, o: Option<&Resolved>, n: &Resolved, changed_kinds: &BTreeSet<String>, changed: &[String], pd: &mut ProjectDiff) {
    let empty_steps = vec![];
    let old_steps = o.map(|r| &r.steps).unwrap_or(&empty_steps);
    let mut changed_steps: BTreeSet<String> = BTreeSet::new();
    let new_refs = project_refs(n);
    let old_refs = o.map(project_refs).unwrap_or_default();
    // Changed files this project does not read directly — not its recipe files, not a
    // file its own references reach — but that a script it runs can still load (a
    // helper module). Decided per project: another project naming the same file does
    // not make it "seen" here. Other projects' directories, presets, the catalog and
    // Markdown are left out.
    let own_dir = format!("projects/{}/", n.id);
    let mut direct: BTreeSet<String> = n.files.iter().cloned().collect();
    direct.extend(o.map(|r| r.files.clone()).unwrap_or_default());
    for (_, rr) in new_refs.iter().chain(old_refs.iter()) {
        direct.extend(ref_files(old, rr));
        direct.extend(ref_files(new, rr));
    }
    let reaching: Vec<&String> = if new_refs.is_empty() {
        vec![]
    } else {
        changed
            .iter()
            .filter(|f| !direct.contains(*f))
            .filter(|f| f.starts_with(&own_dir) || !f.starts_with("projects/"))
            .filter(|f| !f.starts_with("presets/") && f.as_str() != "toolchains.toml" && !f.to_ascii_lowercase().ends_with(".md"))
            .collect()
    };

    // settings
    if let Some(o) = o {
        let pairs: Vec<(&str, Value, Value)> = vec![
            ("toolchains", serde_json::to_value(&o.toolchains).unwrap(), serde_json::to_value(&n.toolchains).unwrap()),
            ("env", serde_json::to_value(&o.env).unwrap(), serde_json::to_value(&n.env).unwrap()),
            ("isolate", serde_json::to_value(&o.isolate).unwrap(), serde_json::to_value(&n.isolate).unwrap()),
            ("account_env", serde_json::to_value(&o.account_env).unwrap(), serde_json::to_value(&n.account_env).unwrap()),
            ("vars", serde_json::to_value(&o.vars).unwrap(), serde_json::to_value(&n.vars).unwrap()),
            ("workspace.keep", serde_json::to_value(&o.keep).unwrap(), serde_json::to_value(&n.keep).unwrap()),
            ("workspace.clean_env", serde_json::to_value(&o.clean_env).unwrap(), serde_json::to_value(&n.clean_env).unwrap()),
        ];
        for (name, a, b) in pairs {
            for f in field_changes(&a, &b, name) {
                let risk = match name {
                    "env" | "account_env" | "workspace.clean_env" => Risk::Exec,
                    "toolchains" => Risk::Supply,
                    _ => Risk::Behavior,
                };
                pd.risks.insert(risk);
                pd.settings.push(FieldChange { risk: Some(risk), ..f });
            }
        }
        // params: a changed parameter changes the steps it applies to
        let names: BTreeSet<&String> = o.params.keys().chain(n.params.keys()).collect();
        for p in names {
            let a = o.params.get(p).map(|x| serde_json::to_value(x).unwrap()).unwrap_or(Value::Null);
            let b = n.params.get(p).map(|x| serde_json::to_value(x).unwrap()).unwrap_or(Value::Null);
            if a != b {
                pd.risks.insert(Risk::Behavior);
                pd.settings.push(FieldChange { field: format!("params.{p}"), from: a, to: b, risk: Some(Risk::Behavior) });
                for s in n.params.get(p).and_then(|x| x.applies_to.clone()).unwrap_or_default() {
                    if n.step(&s).is_some() {
                        changed_steps.insert(s);
                    }
                }
            }
        }
        // uploads: who may hand a job what, and which steps get it — a changed upload
        // changes the steps it applies to, before and after
        let unames: BTreeSet<&String> = o.uploads.keys().chain(n.uploads.keys()).collect();
        for u in unames {
            let a = o.uploads.get(u).map(|x| serde_json::to_value(x).unwrap()).unwrap_or(Value::Null);
            let b = n.uploads.get(u).map(|x| serde_json::to_value(x).unwrap()).unwrap_or(Value::Null);
            if a != b {
                pd.risks.insert(Risk::Permission);
                pd.settings.push(FieldChange { field: format!("upload.{u}"), from: a, to: b, risk: Some(Risk::Permission) });
                for s in o.uploads.get(u).into_iter().chain(n.uploads.get(u)).flat_map(|x| x.applies_to.clone()) {
                    if n.step(&s).is_some() {
                        changed_steps.insert(s);
                    }
                }
            }
        }
        // fixtures: a changed fixture changes the steps that use it
        let fnames: BTreeSet<&String> = o.fixtures.keys().chain(n.fixtures.keys()).collect();
        for f in fnames {
            let a = o.fixtures.get(f).map(|x| serde_json::to_value(x).unwrap()).unwrap_or(Value::Null);
            let b = n.fixtures.get(f).map(|x| serde_json::to_value(x).unwrap()).unwrap_or(Value::Null);
            if a != b {
                let weaken = a.get("on_up_failure").and_then(|v| v.as_str()) == Some("code")
                    && b.get("on_up_failure").and_then(|v| v.as_str()) != Some("code");
                let risk = if weaken { Risk::Weaken } else { Risk::Exec };
                pd.risks.insert(risk);
                pd.settings.push(FieldChange { field: format!("fixture.{f}"), from: a, to: b, risk: Some(risk) });
                for s in &n.steps {
                    if s.def.fixture.as_deref() == Some(f.as_str()) {
                        changed_steps.insert(s.id().to_string());
                    }
                }
            }
        }
        // vars: a changed var changes every step whose strings use it
        let changed_vars: Vec<&String> = o.vars.keys().chain(n.vars.keys()).filter(|k| o.vars.get(*k) != n.vars.get(*k)).collect();
        if !changed_vars.is_empty() {
            for (scope, t) in project_texts(n) {
                if let Scope::Step(id) = scope {
                    if changed_vars.iter().any(|k| t.contains(&format!("${{vars.{k}}}"))) {
                        changed_steps.insert(id);
                    }
                }
            }
        }
    }
    // scripts reached from project-wide strings ([env], [workspace])
    let project_scope: BTreeSet<&RecipeRef> = new_refs.iter().chain(old_refs.iter()).filter(|(s, _)| *s == Scope::Project).map(|(_, r)| r).collect();
    for r in project_scope {
        if ref_changed(old, new, r) {
            pd.risks.insert(Risk::Exec);
            pd.settings.push(FieldChange { field: ref_label(r), from: Value::Null, to: Value::Null, risk: Some(Risk::Exec) });
        }
    }
    if !reaching.is_empty() && new_refs.iter().any(|(s, _)| *s == Scope::Project) {
        for f in &reaching {
            pd.risks.insert(Risk::Exec);
            pd.settings.push(FieldChange { field: format!("script(간접):{f}"), from: Value::Null, to: Value::Null, risk: Some(Risk::Exec) });
        }
    }
    let project_wide = pd.settings.iter().any(|f| !f.field.starts_with("params.") && !f.field.starts_with("fixture.") && !f.field.starts_with("upload."));

    // steps
    for s in &n.steps {
        let old_step = old_steps.iter().find(|x| x.id() == s.id());
        let mut sc = StepChange { id: s.id().to_string(), change: "changed", kind: Some(s.kind.as_str().into()), fields: vec![], risks: BTreeSet::new() };
        match old_step {
            None => {
                sc.change = "added";
                sc.risks.insert(Risk::Exec);
            }
            Some(os) => {
                let a = serde_json::to_value(&os.def).unwrap();
                let b = serde_json::to_value(&s.def).unwrap();
                // `default` is compared by its effect below (unset = the kind's default)
                for f in field_changes(&a, &b, "").into_iter().filter(|f| f.field != "default") {
                    let risk = step_field_risk(&f);
                    sc.risks.insert(risk);
                    sc.fields.push(FieldChange { risk: Some(risk), ..f });
                }
                if os.is_default() != s.is_default() {
                    // a test leaving the default run means it stops guarding every build
                    let risk = if !s.is_default() && matches!(s.kind, Kind::Test | Kind::Scenario) { Risk::Weaken } else { Risk::Behavior };
                    sc.risks.insert(risk);
                    sc.fields.push(FieldChange { field: "default".into(), from: Value::Bool(os.is_default()), to: Value::Bool(s.is_default()), risk: Some(risk) });
                }
            }
        }
        // referenced scripts: every reference of this step (and of its fixture), in
        // either version of the step
        if old_step.is_some() {
            let scope = Scope::Step(s.id().to_string());
            let refs: BTreeSet<&RecipeRef> = new_refs.iter().chain(old_refs.iter()).filter(|(sc_, _)| *sc_ == scope).map(|(_, r)| r).collect();
            let has_refs = !refs.is_empty();
            for r in refs {
                if ref_changed(old, new, r) {
                    sc.risks.insert(Risk::Exec);
                    sc.fields.push(FieldChange { field: ref_label(r), from: Value::Null, to: Value::Null, risk: Some(Risk::Exec) });
                }
            }
            if has_refs {
                for f in &reaching {
                    sc.risks.insert(Risk::Exec);
                    sc.fields.push(FieldChange { field: format!("script(간접):{f}"), from: Value::Null, to: Value::Null, risk: Some(Risk::Exec) });
                }
            }
        }
        if sc.change == "added" || !sc.fields.is_empty() {
            changed_steps.insert(s.id().to_string());
            pd.risks.extend(sc.risks.iter().copied());
            pd.steps.push(sc);
        } else if changed_steps.contains(s.id()) {
            // changed through a parameter or fixture — listed without own fields
        }
    }
    for os in old_steps {
        if n.step(os.id()).is_none() {
            pd.risks.insert(Risk::Behavior);
            pd.steps.push(StepChange {
                id: os.id().to_string(),
                change: "removed",
                kind: Some(os.kind.as_str().into()),
                fields: vec![],
                risks: [Risk::Behavior].into_iter().collect(),
            });
        }
    }
    for k in n.toolchains.keys() {
        if changed_kinds.contains(k) {
            pd.catalog_kinds.push(k.clone());
            pd.risks.insert(Risk::Supply);
        }
    }
    pd.requires_default_run = project_wide || !pd.catalog_kinds.is_empty() || pd.change == "added";
    pd.changed_steps = changed_steps.into_iter().collect();
}

fn step_field_risk(f: &FieldChange) -> Risk {
    let top = f.field.split(['.', '[']).next().unwrap_or("");
    match top {
        "run" | "shell" | "cwd" | "env" => Risk::Exec,
        // a judge script is a command like any other
        "judge" if f.field == "judge.run" => Risk::Exec,
        "judge" if f.field == "judge" && f.to.get("run").is_some_and(|r| !r.is_null()) => Risk::Exec,
        "judge" => {
            let weaker = match f.field.as_str() {
                "judge.min_tests" => f.to.as_i64().unwrap_or(1) < f.from.as_i64().unwrap_or(1),
                "judge.type" => f.to.as_str() == Some("exit") && f.from.as_str() != Some("exit"),
                "judge" => f.to.is_null(),
                _ => false,
            };
            if weaker {
                Risk::Weaken
            } else {
                Risk::Behavior
            }
        }
        // inspection scripts run on the build machine too
        "artifact" if inspects(&f.from) != inspects(&f.to) => Risk::Exec,
        "fail_on_output" => {
            // looser when any pattern that used to fail the step is gone
            let set = |v: &Value| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<BTreeSet<String>>()).unwrap_or_default();
            if set(&f.from).difference(&set(&f.to)).next().is_some() {
                Risk::Weaken
            } else {
                Risk::Behavior
            }
        }
        "uncounted" if f.to.as_bool() == Some(true) => Risk::Weaken,
        _ => Risk::Behavior,
    }
}

/// The inspect lists of an `artifact` array, by artifact name.
fn inspects(v: &Value) -> BTreeMap<String, Value> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|x| (x.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string(), x.get("inspect").cloned().unwrap_or(Value::Null)))
                .filter(|(_, i)| !i.is_null() && i.as_array().is_none_or(|a| !a.is_empty()))
                .collect()
        })
        .unwrap_or_default()
}

/// Leaf-level differences between two JSON values; `prefix` names the root.
pub fn field_changes(a: &Value, b: &Value, prefix: &str) -> Vec<FieldChange> {
    let mut out = vec![];
    fn walk(a: &Value, b: &Value, path: String, out: &mut Vec<FieldChange>) {
        if a == b {
            return;
        }
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
                for k in keys {
                    let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                    walk(x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), p, out);
                }
            }
            _ => {
                // treat empty arrays/false/null alike as "absent"
                let absent = |v: &Value| v.is_null() || v.as_array().is_some_and(|a| a.is_empty()) || v.as_bool() == Some(false);
                if absent(a) && absent(b) {
                    return;
                }
                out.push(FieldChange { field: path, from: a.clone(), to: b.clone(), risk: None })
            }
        }
    }
    walk(a, b, prefix.to_string(), &mut out);
    out
}

/// Helper for promote conditions: `(project, changed steps, needs default run)` for
/// every affected project.
pub fn trial_requirements(d: &SemanticDiff) -> BTreeMap<String, (Vec<String>, bool)> {
    d.projects
        .iter()
        .filter(|p| p.change != "removed")
        .map(|p| (p.id.clone(), (p.changed_steps.clone(), p.requires_default_run)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::MemTree;

    fn base() -> MemTree {
        let mut t = MemTree::new();
        t.insert("toolchains.toml", "schema = 1\n[kind.node]\ntier = \"managed\"\ninstaller = \"archive\"\nurl = \"u\"\nchecksum = { source = \"sha256\" }\n");
        t.insert("projects/a/project.toml", "schema = 1\nid = \"a\"\nrepo = \"https://x\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\n");
        t.insert(
            "projects/a/recipe.toml",
            "schema = 1\n[toolchains]\nnode = { version = \"1.2.3\" }\n[[step]]\nid = \"t\"\nkind = \"test\"\nrun = [\"x\"]\njudge = { type = \"junit\", reports = \"r.xml\", min_tests = 1 }\n",
        );
        t.insert("projects/b/project.toml", "schema = 1\nid = \"b\"\nrepo = \"https://y\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\n");
        t.insert("projects/b/recipe.toml", "schema = 1\n[[step]]\nid = \"b1\"\nkind = \"build\"\nrun = [\"python\", \"${recipes}/scripts/x.py\"]\n");
        t.insert("scripts/x.py", "print(1)\n");
        t
    }

    #[test]
    fn weakening_and_added_step() {
        let old = base();
        let mut new = base();
        new.insert(
            "projects/a/recipe.toml",
            "schema = 1\n[toolchains]\nnode = { version = \"1.2.3\" }\n[[step]]\nid = \"t\"\nkind = \"test\"\nrun = [\"x\"]\njudge = { type = \"junit\", reports = \"r.xml\", min_tests = 0 }\n[[step]]\nid = \"t2\"\nkind = \"test\"\nrun = [\"y\"]\nuncounted = true\n",
        );
        let d = semantic_diff(&old, &new);
        assert_eq!(d.projects.len(), 1);
        let p = &d.projects[0];
        assert_eq!(p.changed_steps, vec!["t", "t2"]);
        assert!(p.risks.contains(&Risk::Weaken));
        assert!(p.risks.contains(&Risk::Exec));
        assert!(!p.requires_default_run);
    }

    #[test]
    fn script_and_catalog_changes_reach_users() {
        let old = base();
        let mut new = base();
        new.insert("scripts/x.py", "print(2)\n");
        new.insert("toolchains.toml", "schema = 1\n[kind.node]\ntier = \"managed\"\ninstaller = \"archive\"\nurl = \"v\"\nchecksum = { source = \"sha256\" }\n");
        let d = semantic_diff(&old, &new);
        let a = d.projects.iter().find(|p| p.id == "a").unwrap();
        assert_eq!(a.catalog_kinds, vec!["node"]);
        assert!(a.requires_default_run);
        let b = d.projects.iter().find(|p| p.id == "b").unwrap();
        assert_eq!(b.changed_steps, vec!["b1"]);
        assert_eq!(d.catalog.len(), 1);
    }

    #[test]
    fn project_toml_change_flagged() {
        let old = base();
        let mut new = base();
        new.insert("projects/a/project.toml", "schema = 1\nid = \"a\"\nrepo = \"https://evil\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\n");
        let d = semantic_diff(&old, &new);
        let a = d.projects.iter().find(|p| p.id == "a").unwrap();
        assert!(a.project_toml_changed);
        assert!(a.risks.contains(&Risk::Permission));
    }

    #[test]
    fn no_change() {
        assert!(semantic_diff(&base(), &base()).is_empty());
    }

    fn step_of<'a>(d: &'a SemanticDiff, p: &str, s: &str) -> Option<&'a StepChange> {
        d.projects.iter().find(|x| x.id == p)?.steps.iter().find(|x| x.id == s)
    }

    #[test]
    fn default_judge_and_fail_on_output() {
        let old = base();
        let mut new = base();
        new.insert(
            "projects/a/recipe.toml",
            "schema = 1\n[toolchains]\nnode = { version = \"1.2.3\" }\n[[step]]\nid = \"t\"\nkind = \"test\"\ndefault = false\nrun = [\"x\"]\njudge = { type = \"junit\", reports = \"r.xml\", min_tests = 1 }\n",
        );
        let d = semantic_diff(&old, &new);
        let t = step_of(&d, "a", "t").expect("default change listed");
        assert!(t.risks.contains(&Risk::Weaken), "{t:?}");

        let mut old2 = base();
        old2.insert("projects/b/recipe.toml", "schema = 1\n[[step]]\nid = \"b1\"\nkind = \"build\"\nrun = [\"python\", \"${recipes}/scripts/x.py\"]\nfail_on_output = [\"skipped\"]\njudge = { type = \"script\", run = [\"j\"] }\n");
        let mut new2 = old2.clone();
        new2.insert("projects/b/recipe.toml", "schema = 1\n[[step]]\nid = \"b1\"\nkind = \"build\"\nrun = [\"python\", \"${recipes}/scripts/x.py\"]\nfail_on_output = [\"zzz\"]\njudge = { type = \"script\", run = [\"j2\"] }\n");
        let d = semantic_diff(&old2, &new2);
        let b = step_of(&d, "b", "b1").unwrap();
        assert!(b.fields.iter().any(|f| f.field == "fail_on_output" && f.risk == Some(Risk::Weaken)), "{b:?}");
        assert!(b.fields.iter().any(|f| f.field == "judge.run" && f.risk == Some(Risk::Exec)), "{b:?}");
    }

    #[test]
    fn every_script_reference_counts() {
        let mut old = base();
        old.insert(
            "projects/b/recipe.toml",
            "schema = 1\n[[step]]\nid = \"b1\"\nkind = \"build\"\nrun = [\"python\", \"${recipes}/scripts/x.py\", '${recipes}\\scripts\\y.py']\n[[step]]\nid = \"b2\"\nkind = \"test\"\nrun = [\"t\"]\nuncounted = true\nfixture = \"svc\"\n[fixture.svc]\nup = [{ run = [\"pwsh\", \"${recipes}/scripts/up.ps1\"] }]\n",
        );
        old.insert("scripts/y.py", "1");
        old.insert("scripts/up.ps1", "1");
        old.insert("scripts/lib.py", "1");
        // second reference (backslashes)
        let mut new = old.clone();
        new.insert("scripts/y.py", "2");
        assert!(step_of(&semantic_diff(&old, &new), "b", "b1").is_some_and(|s| s.risks.contains(&Risk::Exec)));
        // fixture script
        let mut new = old.clone();
        new.insert("scripts/up.ps1", "2");
        assert!(step_of(&semantic_diff(&old, &new), "b", "b2").is_some_and(|s| s.risks.contains(&Risk::Exec)));
        // a helper no recipe names: every step that references scripts is marked
        let mut new = old.clone();
        new.insert("scripts/lib.py", "2");
        let d = semantic_diff(&old, &new);
        let b1 = step_of(&d, "b", "b1").expect("indirect change reaches b1");
        assert!(b1.fields.iter().any(|f| f.field == "script(간접):scripts/lib.py"), "{b1:?}");
        // documentation alone touches nobody
        let mut new = old.clone();
        new.insert("scripts/README.md", "docs");
        assert!(semantic_diff(&old, &new).projects.is_empty());
        // another project naming the helper directly does not hide it from b
        let mut old2 = old.clone();
        old2.insert("projects/a/recipe.toml", "schema = 1\n[toolchains]\nnode = { version = \"1.2.3\" }\n[[step]]\nid = \"t\"\nkind = \"test\"\nrun = [\"python\", \"${recipes}/scripts/lib.py\"]\nuncounted = true\n");
        let mut new2 = old2.clone();
        new2.insert("scripts/lib.py", "2");
        let d = semantic_diff(&old2, &new2);
        assert!(step_of(&d, "b", "b1").is_some(), "b still affected: {d:?}");
        assert!(step_of(&d, "a", "t").is_some(), "a affected directly");
    }

    #[test]
    fn vars_reach_non_default_steps_and_broken_project_toml_is_flagged() {
        let mut old = base();
        old.insert("projects/b/recipe.toml", "schema = 1\n[vars]\nkey = \"1\"\n[[step]]\nid = \"b1\"\nkind = \"build\"\nrun = [\"b\"]\n[[step]]\nid = \"pkg\"\nkind = \"package\"\nrun = [\"p\", \"${vars.key}\"]\n");
        let mut new = old.clone();
        new.insert("projects/b/recipe.toml", "schema = 1\n[vars]\nkey = \"2\"\n[[step]]\nid = \"b1\"\nkind = \"build\"\nrun = [\"b\"]\n[[step]]\nid = \"pkg\"\nkind = \"package\"\nrun = [\"p\", \"${vars.key}\"]\n");
        let d = semantic_diff(&old, &new);
        let b = d.projects.iter().find(|p| p.id == "b").unwrap();
        assert!(b.changed_steps.contains(&"pkg".to_string()), "{b:?}");

        let mut old = base();
        old.insert("projects/a/project.toml", "schema = 1\nid = \"a\"\nrepo = ");
        let mut new = old.clone();
        new.insert("projects/a/project.toml", "schema = 1\nid = \"a\"\nrepo = \"https://evil\"\nallowed_refs = [\"*\"]\ndefault_ref = \"main\"\n");
        let d = semantic_diff(&old, &new);
        let a = d.projects.iter().find(|p| p.id == "a").unwrap();
        assert!(a.project_toml_changed && a.risks.contains(&Risk::Permission), "{a:?}");
    }
}
