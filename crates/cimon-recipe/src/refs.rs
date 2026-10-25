//! Which recipe-repository files a project reaches through `${recipes}` — used by the
//! static checks (does it exist), the semantic diff (did it change) and the recipe
//! sha256 (what a job ran). Every expandable string counts, every occurrence, with
//! either path separator.

use crate::check::step_strings;
use crate::model::FixtureDef;
use crate::resolve::Resolved;
use crate::tree::Tree;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecipeRef {
    /// Repository path (forward slashes); empty means the whole repository.
    pub path: String,
    /// The reference goes on with a variable; `path` is the directory before it.
    pub dynamic: bool,
}

/// Every `${recipes}` reference in `text`, in order.
pub fn recipe_refs(text: &str) -> Vec<RecipeRef> {
    const TOKEN: &str = "${recipes}";
    let mut out = vec![];
    let mut rest = text;
    while let Some(i) = rest.find(TOKEN) {
        let after = &rest[i + TOKEN.len()..];
        let Some(tail) = after.strip_prefix('/').or_else(|| after.strip_prefix('\\')) else {
            // bare ${recipes}: the whole checkout (cwd, PYTHONPATH, …)
            out.push(RecipeRef { path: String::new(), dynamic: false });
            rest = after;
            continue;
        };
        let end = tail.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ';' | ',' | '|' | '&' | '<' | '>' | '(' | ')')).unwrap_or(tail.len());
        let seg = tail[..end].replace('\\', "/");
        let r = match seg.find("${") {
            Some(k) => {
                let before = &seg[..k];
                let dir = before.rfind('/').map(|j| &before[..j]).unwrap_or("");
                RecipeRef { path: dir.trim_matches('/').to_string(), dynamic: true }
            }
            None => RecipeRef { path: seg.trim_end_matches('/').to_string(), dynamic: false },
        };
        out.push(r);
        rest = &tail[end..];
    }
    out
}

/// Replaces `${vars.x}` with known values and leaves every other reference alone, so a
/// path built from a var is still recognised.
pub fn expand_vars(text: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                match name.strip_prefix("vars.").and_then(|k| vars.get(k)) {
                    Some(v) => out.push_str(v),
                    None => out.push_str(&rest[start..start + 2 + end + 1]),
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Every expandable string of a fixture.
pub fn fixture_strings(f: &FixtureDef) -> Vec<String> {
    let mut v = vec![];
    for c in f.up.iter().chain(f.down.iter()) {
        v.extend(c.run.clone());
    }
    for poll in [&f.ready, &f.gone].into_iter().flatten() {
        v.extend(poll.run.clone());
        if let Some(ot) = &poll.on_timeout {
            v.extend(ot.run.clone());
        }
    }
    v.extend(f.env.values().cloned());
    v.extend(f.collect.clone());
    v
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Applies to every step ([env], [workspace]).
    Project,
    Step(String),
}

/// Every expandable string of a project with the scope it affects. A fixture's strings
/// belong to the steps that use it.
pub fn project_texts(r: &Resolved) -> Vec<(Scope, String)> {
    let mut v = vec![];
    for s in &r.steps {
        let scope = Scope::Step(s.id().to_string());
        for t in step_strings(&s.def) {
            v.push((scope.clone(), t));
        }
        if let Some(f) = s.def.fixture.as_ref().and_then(|f| r.fixtures.get(f)) {
            for t in fixture_strings(f) {
                v.push((scope.clone(), t));
            }
        }
    }
    for t in r.env.values().chain(r.clean_env.values()).chain(r.keep.iter()) {
        v.push((Scope::Project, t.clone()));
    }
    for p in r.params.values() {
        for a in p.args.iter().flatten() {
            v.push((Scope::Project, a.clone()));
        }
    }
    v
}

/// Every recipe reference of a project ([vars] expanded), by scope.
pub fn project_refs(r: &Resolved) -> Vec<(Scope, RecipeRef)> {
    let mut out = vec![];
    for (scope, t) in project_texts(r) {
        for rr in recipe_refs(&expand_vars(&t, &r.vars)) {
            out.push((scope.clone(), rr));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Files a reference reaches in `tree`: the file, or every file under the directory.
pub fn ref_files(tree: &dyn Tree, r: &RecipeRef) -> Vec<String> {
    if r.path.is_empty() {
        return tree.files().into_iter().map(String::from).collect();
    }
    if !r.dynamic && tree.exists(&r.path) {
        return vec![r.path.clone()];
    }
    let prefix = format!("{}/", r.path);
    tree.files().into_iter().filter(|f| f.starts_with(&prefix)).map(String::from).collect()
}

/// Files that decide what a job of this project runs: its recipe files, every file its
/// references reach, and — because a referenced script can load helpers by relative
/// path — everything under `scripts/` and the project's own directory once it
/// references anything.
pub fn recipe_files(tree: &dyn Tree, r: &Resolved) -> Vec<String> {
    let mut files: BTreeSet<String> = r.files.iter().cloned().collect();
    let refs = project_refs(r);
    for (_, rr) in &refs {
        files.extend(ref_files(tree, rr));
    }
    if !refs.is_empty() {
        let own = format!("projects/{}/", r.id);
        files.extend(tree.files().into_iter().filter(|f| f.starts_with("scripts/") || f.starts_with(&own)).map(String::from));
    }
    files.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_every_reference() {
        let r = recipe_refs("python ${recipes}/scripts/a.py --lib ${recipes}\\scripts\\lib --x ${recipes}/data/${param.n}.json ${recipes}");
        assert_eq!(
            r,
            vec![
                RecipeRef { path: "scripts/a.py".into(), dynamic: false },
                RecipeRef { path: "scripts/lib".into(), dynamic: false },
                RecipeRef { path: "data".into(), dynamic: true },
                RecipeRef { path: "".into(), dynamic: false },
            ]
        );
        let mut vars = BTreeMap::new();
        vars.insert("tool".to_string(), "scripts/t.ps1".to_string());
        assert_eq!(expand_vars("${recipes}/${vars.tool} ${job.id}", &vars), "${recipes}/scripts/t.ps1 ${job.id}");
    }
}
