//! Semantic diff between two recipe trees (review-ui §4.4): what will run differently,
//! per project, with risk flags. Promote conditions (design §6.5) are computed from it.

use crate::catalog::Catalog;
use crate::check::{find_recipes_path, step_strings};
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
        diff_project(old, new, o.as_ref(), &n, &changed_kinds, &mut pd);
        if in_old {
            if let (Ok(op), Ok(np)) = (parse_project(old, &id), parse_project(new, &id)) {
                if op != np {
                    pd.project_toml_changed = true;
                    let ov = serde_json::to_value(&op).unwrap();
                    let nv = serde_json::to_value(&np).unwrap();
                    for f in field_changes(&ov, &nv, "") {
                        let risk = if f.field.starts_with("repo") || f.field.starts_with("allowed_refs") || f.field.starts_with("sibling") {
                            Some(Risk::Permission)
                        } else {
                            Some(Risk::Behavior)
                        };
                        if let Some(r) = risk {
                            pd.risks.insert(r);
                        }
                        pd.project_fields.push(FieldChange { risk, ..f });
                    }
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

fn diff_project(old: &dyn Tree, new: &dyn Tree, o: Option<&Resolved>, n: &Resolved, changed_kinds: &BTreeSet<String>, pd: &mut ProjectDiff) {
    let empty_steps = vec![];
    let old_steps = o.map(|r| &r.steps).unwrap_or(&empty_steps);
    let mut changed_steps: BTreeSet<String> = BTreeSet::new();

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
    }
    let project_wide = pd.settings.iter().any(|f| !f.field.starts_with("params.") && !f.field.starts_with("fixture."));

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
                for f in field_changes(&a, &b, "") {
                    let risk = step_field_risk(&f);
                    sc.risks.insert(risk);
                    sc.fields.push(FieldChange { risk: Some(risk), ..f });
                }
            }
        }
        // referenced scripts
        for text in step_strings(&s.def) {
            if let Some(p) = find_recipes_path(&text) {
                if old.read(&p) != new.read(&p) && old_step.is_some() {
                    sc.risks.insert(Risk::Exec);
                    sc.fields.push(FieldChange { field: format!("script:{p}"), from: Value::Null, to: Value::Null, risk: Some(Risk::Exec) });
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
        "fail_on_output" => {
            let a = f.from.as_array().map(|v| v.len()).unwrap_or(0);
            let b = f.to.as_array().map(|v| v.len()).unwrap_or(0);
            if b < a || f.to.is_null() {
                Risk::Weaken
            } else {
                Risk::Behavior
            }
        }
        "uncounted" if f.to.as_bool() == Some(true) => Risk::Weaken,
        _ => Risk::Behavior,
    }
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
}
