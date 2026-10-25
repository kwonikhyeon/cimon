//! What `build`/`test`/`run` will run (recipe-spec §14).

use crate::model::{Kind, ParamDef};
use crate::resolve::Resolved;
use cimon_core::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default)]
pub struct SelectRequest {
    pub steps: Vec<String>,
    pub tags: Vec<String>,
    /// `None` = the default artifacts of the selected steps; `Some(vec![])` = none.
    pub artifacts: Option<Vec<String>>,
    pub params: BTreeMap<String, Value>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedStep {
    pub id: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Plan {
    /// Setup steps, in recipe order. Each decides at run time whether it runs.
    pub setup: Vec<String>,
    /// Non-setup steps in execution order.
    pub steps: Vec<PlannedStep>,
    /// (step, reason) — not run because the runner lacks a capability.
    pub excluded: Vec<(String, String)>,
    /// Artifacts to collect.
    pub artifacts: Vec<String>,
    /// Toolchain kinds to provide.
    pub toolchains: Vec<String>,
    /// Every parameter with its effective value (explicit or default). Absent = no value.
    pub params: BTreeMap<String, Value>,
    /// Parameters the caller set explicitly.
    pub explicit_params: Vec<String>,
    pub fixtures: Vec<String>,
}

impl Plan {
    pub fn step_ids(&self) -> Vec<String> {
        self.steps.iter().map(|s| s.id.clone()).collect()
    }
    pub fn runs(&self, id: &str) -> bool {
        self.steps.iter().any(|s| s.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub code: &'static str,
    pub message: String,
}

impl Rejection {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Rejection { code, message: message.into() }
    }
}

pub fn candidate_kinds(tool: Tool) -> &'static [Kind] {
    match tool {
        Tool::Build => &[Kind::Build, Kind::Package],
        Tool::Test => &[Kind::Test, Kind::Scenario],
        Tool::Run => &[Kind::Build, Kind::Package, Kind::Test, Kind::Scenario],
    }
}

pub fn select(r: &Resolved, tool: Tool, req: &SelectRequest) -> Result<Plan, Rejection> {
    let kinds = candidate_kinds(tool);
    let producers = r.artifact_producers();

    // 1–2. pick
    let mut picked_by_id: BTreeSet<String> = BTreeSet::new();
    let mut picked: BTreeSet<String> = BTreeSet::new();
    for id in &req.steps {
        let Some(s) = r.step(id) else {
            let known: Vec<&str> = r.steps.iter().map(|s| s.id()).collect();
            return Err(Rejection::new("unknown_step", format!("unknown step {id:?} — known: {}", known.join(", "))));
        };
        if !kinds.contains(&s.kind) {
            let hint = match s.kind {
                Kind::Setup => "setup steps run automatically".to_string(),
                Kind::Test | Kind::Scenario => "call test or run".to_string(),
                Kind::Build | Kind::Package => "call build or run".to_string(),
            };
            return Err(Rejection::new("wrong_kind", format!("{id:?} is a {} step — {hint}", s.kind.as_str())));
        }
        picked_by_id.insert(id.clone());
        picked.insert(id.clone());
    }
    for tag in &req.tags {
        let matched: Vec<&str> = r.steps.iter().filter(|s| kinds.contains(&s.kind) && s.def.tags.contains(tag)).map(|s| s.id()).collect();
        if matched.is_empty() {
            let all: BTreeSet<&str> = r.steps.iter().filter(|s| kinds.contains(&s.kind)).flat_map(|s| s.def.tags.iter().map(|t| t.as_str())).collect();
            return Err(Rejection::new(
                "unknown_tag",
                format!("no {} step has tag {tag:?} — tags: {}", tool.as_str(), all.into_iter().collect::<Vec<_>>().join(", ")),
            ));
        }
        picked.extend(matched.into_iter().map(String::from));
    }
    let mut requested_artifacts: Vec<String> = vec![];
    if let Some(names) = &req.artifacts {
        for a in names {
            let Some(p) = producers.get(a) else {
                return Err(Rejection::new(
                    "unknown_artifact",
                    format!("unknown artifact {a:?} — known: {}", producers.keys().cloned().collect::<Vec<_>>().join(", ")),
                ));
            };
            if !requested_artifacts.contains(a) {
                requested_artifacts.push(a.clone());
            }
            picked_by_id.insert(p.clone());
            picked.insert(p.clone());
        }
    }
    if req.steps.is_empty() && req.tags.is_empty() && req.artifacts.as_ref().is_none_or(|a| a.is_empty()) {
        for s in &r.steps {
            if kinds.contains(&s.kind) && s.is_default() {
                picked.insert(s.id().to_string());
            }
        }
        if picked.is_empty() {
            return Err(Rejection::new("nothing_to_run", format!("no {} step is default = true — name steps or tags", tool.as_str())));
        }
    }

    // 3. close over after and fixture dependencies
    let mut selected = picked.clone();
    let mut fixture_artifacts: Vec<String> = vec![];
    let mut stack: Vec<String> = selected.iter().cloned().collect();
    while let Some(id) = stack.pop() {
        let Some(s) = r.step(&id) else { continue };
        let mut deps = s.def.after.clone();
        if let Some(f) = s.def.fixture.as_ref().and_then(|f| r.fixtures.get(f)) {
            deps.extend(f.after.clone());
            for a in &f.requires_artifacts {
                if let Some(p) = producers.get(a) {
                    deps.push(p.clone());
                }
                if !fixture_artifacts.contains(a) {
                    fixture_artifacts.push(a.clone());
                }
            }
        }
        for d in deps {
            if r.step(&d).is_some_and(|x| x.kind == Kind::Setup) {
                continue;
            }
            if selected.insert(d.clone()) {
                stack.push(d);
            }
        }
    }

    // 4. exclusion by capability, propagated to dependents
    let caps: BTreeSet<&str> = req.capabilities.iter().map(|s| s.as_str()).collect();
    let mut excluded: BTreeMap<String, String> = BTreeMap::new();
    for id in &selected {
        let s = r.step(id).unwrap();
        let missing: Vec<&str> = s.def.needs.iter().map(|n| n.as_str()).filter(|n| !caps.contains(n)).collect();
        if !missing.is_empty() {
            excluded.insert(id.clone(), format!("{} 필요 — 이 러너에는 없다", missing.join(", ")));
        }
    }
    loop {
        let mut changed = false;
        for id in &selected {
            if excluded.contains_key(id) {
                continue;
            }
            let s = r.step(id).unwrap();
            let mut deps = s.def.after.clone();
            if let Some(f) = s.def.fixture.as_ref().and_then(|f| r.fixtures.get(f)) {
                deps.extend(f.after.clone());
                deps.extend(f.requires_artifacts.iter().filter_map(|a| producers.get(a).cloned()));
            }
            if let Some(d) = deps.iter().find(|d| excluded.contains_key(*d)) {
                let reason = format!("기대는 단계 {d} 가 제외됐다({})", excluded[d]);
                excluded.insert(id.clone(), reason);
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }
    for id in &picked_by_id {
        if let Some(reason) = excluded.get(id) {
            return Err(Rejection::new("needs_unavailable", format!("{id:?} cannot run here: {reason}")));
        }
    }
    let remaining: Vec<String> = selected.iter().filter(|id| !excluded.contains_key(*id)).cloned().collect();
    if remaining.is_empty() {
        let list: Vec<String> = excluded.iter().map(|(k, v)| format!("{k} — {v}")).collect();
        return Err(Rejection::new("nothing_to_run", format!("every selected step was excluded: {}", list.join("; "))));
    }

    // 5. parameters
    let mut params = BTreeMap::new();
    let mut explicit = vec![];
    for (name, value) in &req.params {
        let Some(def) = r.params.get(name) else {
            let known: Vec<&str> = r.params.keys().map(|s| s.as_str()).collect();
            return Err(Rejection::new(
                "unknown_param",
                format!("unknown parameter {name:?} — declared: {}", if known.is_empty() { "(none)".into() } else { known.join(", ") }),
            ));
        };
        validate_param_value(def, value).map_err(|e| Rejection::new("invalid_param", format!("parameter {name}: {e}")))?;
        let applies = def.applies_to.clone().unwrap_or_default();
        if !applies.iter().any(|a| remaining.contains(a)) {
            return Err(Rejection::new(
                "param_not_applicable",
                format!(
                    "{name} does not apply to the selected steps ({}) — it is used by: {}",
                    remaining.join(", "),
                    applies.join(", ")
                ),
            ));
        }
        params.insert(name.clone(), value.clone());
        explicit.push(name.clone());
    }
    for (name, def) in &r.params {
        if !params.contains_key(name) {
            if let Some(d) = &def.default {
                params.insert(name.clone(), crate::check::toml_to_json(d));
            }
        }
    }

    // 6–7. setup, order
    let setup: Vec<String> = r.steps.iter().filter(|s| s.kind == Kind::Setup).map(|s| s.id().to_string()).collect();
    let order = topo_order(r, &remaining);
    let steps: Vec<PlannedStep> = order.iter().map(|id| PlannedStep { id: id.clone(), kind: r.step(id).unwrap().kind }).collect();

    // artifacts to collect
    let mut artifacts: Vec<String> = match &req.artifacts {
        Some(names) => names.clone(),
        None => steps
            .iter()
            .flat_map(|ps| r.step(&ps.id).unwrap().def.artifact.iter().filter(|a| a.default.unwrap_or(true)).map(|a| a.name.clone()))
            .collect(),
    };
    for a in fixture_artifacts {
        if !artifacts.contains(&a) {
            artifacts.push(a);
        }
    }
    artifacts.retain(|a| producers.get(a).is_some_and(|p| remaining.contains(p)));

    // toolchains
    let used_optional: BTreeSet<&str> = steps.iter().flat_map(|ps| r.step(&ps.id).unwrap().def.uses.iter().map(|s| s.as_str())).collect();
    let toolchains: Vec<String> =
        r.toolchains.iter().filter(|(k, t)| !t.optional || used_optional.contains(k.as_str())).map(|(k, _)| k.clone()).collect();
    let fixtures: Vec<String> = {
        let mut f: Vec<String> = steps.iter().filter_map(|ps| r.step(&ps.id).unwrap().def.fixture.clone()).collect();
        f.dedup();
        let mut seen = BTreeSet::new();
        f.retain(|x| seen.insert(x.clone()));
        f
    };

    Ok(Plan {
        setup,
        steps,
        excluded: excluded.into_iter().collect(),
        artifacts,
        toolchains,
        params,
        explicit_params: explicit,
        fixtures,
    })
}

/// Topological order; ties broken by declaration order.
fn topo_order(r: &Resolved, ids: &[String]) -> Vec<String> {
    let set: BTreeSet<&String> = ids.iter().collect();
    let producers = r.artifact_producers();
    let deps_of = |id: &str| -> Vec<String> {
        let s = r.step(id).unwrap();
        let mut d = s.def.after.clone();
        if let Some(f) = s.def.fixture.as_ref().and_then(|f| r.fixtures.get(f)) {
            d.extend(f.after.clone());
            d.extend(f.requires_artifacts.iter().filter_map(|a| producers.get(a).cloned()));
        }
        d.into_iter().filter(|x| set.contains(x)).collect()
    };
    let mut done: Vec<String> = vec![];
    let declared: Vec<&str> = r.steps.iter().map(|s| s.id()).filter(|id| set.contains(&id.to_string())).collect();
    while done.len() < declared.len() {
        let next = declared.iter().find(|id| !done.contains(&id.to_string()) && deps_of(id).iter().all(|d| done.contains(d)));
        match next {
            Some(id) => done.push(id.to_string()),
            None => {
                // cycle — static checks reject it; fall back to declaration order
                for id in &declared {
                    if !done.contains(&id.to_string()) {
                        done.push(id.to_string());
                    }
                }
            }
        }
    }
    done
}

pub fn validate_param_value(def: &ParamDef, v: &Value) -> Result<(), String> {
    match def.type_.as_str() {
        "string" => {
            let s = v.as_str().ok_or("expected a string")?;
            if s.contains('\0') || s.contains('\n') || s.contains('\r') {
                return Err("control characters are not allowed".into());
            }
            if let Some(p) = &def.pattern {
                let re = regex::Regex::new(&format!("^(?:{p})$")).map_err(|e| e.to_string())?;
                if !re.is_match(s) {
                    return Err(format!("{s:?} does not match {p}"));
                }
            }
            Ok(())
        }
        "int" => {
            let n = v.as_i64().ok_or("expected an integer")?;
            if def.min.is_some_and(|m| n < m) || def.max.is_some_and(|m| n > m) {
                return Err(format!("{n} is outside [{}, {}]", def.min.map(|m| m.to_string()).unwrap_or_default(), def.max.map(|m| m.to_string()).unwrap_or_default()));
            }
            Ok(())
        }
        "bool" => v.as_bool().map(|_| ()).ok_or_else(|| "expected true or false".into()),
        "enum" => {
            let s = v.as_str().ok_or("expected a string")?;
            if def.values.as_ref().is_some_and(|vals| vals.iter().any(|x| x == s)) {
                Ok(())
            } else {
                Err(format!("{s:?} is not one of {}", def.values.clone().unwrap_or_default().join(", ")))
            }
        }
        other => Err(format!("unknown type {other}")),
    }
}

/// Parameter value as it goes into an environment variable or argument.
pub fn param_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Branch name glob: `*` does not cross `/`, `**` does.
pub fn ref_allowed(patterns: &[String], name: &str) -> bool {
    patterns.iter().any(|p| glob_match(p, name))
}

fn glob_match(pattern: &str, name: &str) -> bool {
    let mut re = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    re.push_str(".*");
                } else {
                    re.push_str("[^/]*");
                }
            }
            '?' => re.push_str("[^/]"),
            other => re.push_str(&regex::escape(&other.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re).is_ok_and(|r| r.is_match(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::resolve_project;
    use crate::tree::MemTree;
    use serde_json::json;

    fn resolved() -> Resolved {
        let mut t = MemTree::new();
        t.insert("toolchains.toml", "schema = 1\n");
        t.insert("projects/a/project.toml", "schema = 1\nid = \"a\"\nrepo = \"https://x\"\nallowed_refs = [\"main\", \"feature/**\"]\ndefault_ref = \"main\"\n");
        t.insert(
            "projects/a/recipe.toml",
            r#"schema = 1
[params.url]
type = "string"
pattern = 'https?://[a-z.]+'
default = "http://x"
env = "URL"
applies_to = ["build-apk"]

[[step]]
id = "setup"
kind = "setup"
run = ["s"]

[[step]]
id = "build-apk"
kind = "build"
run = ["b"]
tags = ["android"]
  [[step.artifact]]
  name = "app.apk"
  glob = "x.apk"
  expect = 1
  default = false

[[step]]
id = "package"
kind = "package"
run = ["p"]
after = ["test-shared"]
  [[step.artifact]]
  name = "dist.zip"
  glob = "d.zip"
  expect = 1

[[step]]
id = "test-shared"
kind = "test"
run = ["t"]
tags = ["unit"]
uncounted = true

[[step]]
id = "ui-test"
kind = "test"
run = ["u"]
tags = ["ui", "unit"]
needs = ["display"]
uncounted = true
"#,
        );
        resolve_project(&t, "a").unwrap()
    }

    #[test]
    fn defaults_and_exclusion() {
        let r = resolved();
        let p = select(&r, Tool::Run, &SelectRequest::default()).unwrap();
        assert_eq!(p.step_ids(), vec!["build-apk", "test-shared"]);
        assert_eq!(p.excluded, vec![("ui-test".to_string(), "display 필요 — 이 러너에는 없다".to_string())]);
        assert_eq!(p.setup, vec!["setup"]);
        assert!(p.artifacts.is_empty(), "app.apk is not default");
        assert_eq!(p.params["url"], json!("http://x"));
    }

    #[test]
    fn artifact_pulls_producer_across_kinds() {
        let r = resolved();
        let p = select(&r, Tool::Test, &SelectRequest { artifacts: Some(vec!["app.apk".into()]), ..Default::default() }).unwrap();
        assert!(p.runs("build-apk"));
        assert_eq!(p.artifacts, vec!["app.apk"]);
    }

    #[test]
    fn after_closes_and_orders() {
        let r = resolved();
        let p = select(&r, Tool::Build, &SelectRequest { steps: vec!["package".into()], ..Default::default() }).unwrap();
        assert_eq!(p.step_ids(), vec!["package", "test-shared"].into_iter().rev().map(String::from).collect::<Vec<_>>());
    }

    #[test]
    fn rejections() {
        let r = resolved();
        let e = select(&r, Tool::Build, &SelectRequest { steps: vec!["test-shared".into()], ..Default::default() }).unwrap_err();
        assert_eq!(e.code, "wrong_kind");
        let e = select(&r, Tool::Test, &SelectRequest { steps: vec!["ui-test".into()], ..Default::default() }).unwrap_err();
        assert_eq!(e.code, "needs_unavailable");
        let e = select(&r, Tool::Test, &SelectRequest { tags: vec!["ui".into()], ..Default::default() }).unwrap_err();
        assert_eq!(e.code, "nothing_to_run");
        let mut params = BTreeMap::new();
        params.insert("url".to_string(), json!("http://y"));
        let e = select(&r, Tool::Test, &SelectRequest { params: params.clone(), ..Default::default() }).unwrap_err();
        assert_eq!(e.code, "param_not_applicable");
        assert!(select(&r, Tool::Build, &SelectRequest { params, ..Default::default() }).is_ok());
        let mut bad = BTreeMap::new();
        bad.insert("url".to_string(), json!("ftp://y"));
        let e = select(&r, Tool::Build, &SelectRequest { params: bad, ..Default::default() }).unwrap_err();
        assert_eq!(e.code, "invalid_param");
        let e = select(&r, Tool::Build, &SelectRequest { steps: vec!["nope".into()], ..Default::default() }).unwrap_err();
        assert_eq!(e.code, "unknown_step");
    }

    #[test]
    fn tags_union_with_steps() {
        let r = resolved();
        let p = select(&r, Tool::Run, &SelectRequest { steps: vec!["build-apk".into()], tags: vec!["unit".into()], ..Default::default() }).unwrap();
        assert_eq!(p.step_ids(), vec!["build-apk", "test-shared"]);
        assert_eq!(p.excluded.len(), 1);
    }

    #[test]
    fn ref_globs() {
        let pats = vec!["main".to_string(), "feature/*".to_string(), "fix/**".to_string()];
        assert!(ref_allowed(&pats, "main"));
        assert!(ref_allowed(&pats, "feature/x"));
        assert!(!ref_allowed(&pats, "feature/x/y"));
        assert!(ref_allowed(&pats, "fix/a/b"));
        assert!(!ref_allowed(&pats, "release/1"));
        assert!(!ref_allowed(&pats, "mainx"));
    }
}
