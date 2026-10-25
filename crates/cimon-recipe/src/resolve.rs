//! Layering: presets (`extends`) in order, then the project recipe, then fragments
//! (`include`) — recipe-spec §10.

use crate::check::Diag;
use crate::model::*;
use crate::tree::Tree;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    pub def: StepDef,
    pub kind: Kind,
    pub origin: String,
}

impl Step {
    pub fn id(&self) -> &str {
        &self.def.id
    }
    pub fn is_default(&self) -> bool {
        self.def.default.unwrap_or_else(|| self.kind.default_selected())
    }
    pub fn timeout_min(&self) -> u32 {
        self.def.timeout_min.unwrap_or(30)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Resolved {
    pub id: String,
    pub project: ProjectFile,
    pub description: Option<String>,
    pub vars: BTreeMap<String, String>,
    pub toolchains: BTreeMap<String, ToolchainReq>,
    pub keep: Vec<String>,
    pub clean_env: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
    pub isolate: BTreeMap<String, String>,
    pub account_env: BTreeMap<String, String>,
    pub params: BTreeMap<String, ParamDef>,
    pub steps: Vec<Step>,
    pub fixtures: BTreeMap<String, FixtureDef>,
    /// Files that make up this recipe (for the recipe sha256 and diffs).
    pub files: Vec<String>,
}

impl Resolved {
    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.def.id == id)
    }

    /// Artifact name → producing step id.
    pub fn artifact_producers(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        for s in &self.steps {
            for a in &s.def.artifact {
                m.insert(a.name.clone(), s.def.id.clone());
            }
        }
        m
    }

    pub fn artifact(&self, name: &str) -> Option<(&Step, &ArtifactDef)> {
        self.steps.iter().find_map(|s| s.def.artifact.iter().find(|a| a.name == name).map(|a| (s, a)))
    }
}

pub fn project_path(id: &str) -> String {
    format!("projects/{id}/project.toml")
}
pub fn recipe_path(id: &str) -> String {
    format!("projects/{id}/recipe.toml")
}

/// 1-based line of the first line containing `needle`.
pub fn line_of(text: &str, needle: &str) -> Option<usize> {
    text.lines().position(|l| l.contains(needle)).map(|i| i + 1)
}

/// Parses a TOML file into `T`, turning syntax/shape errors into a [`Diag`] with line.
pub fn parse_file<T: serde::de::DeserializeOwned>(tree: &dyn Tree, path: &str) -> Result<T, Diag> {
    let Some(bytes) = tree.read(path) else {
        return Err(Diag::error(path, None, "file not found"));
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(Diag::error(path, None, "not UTF-8"));
    };
    toml::from_str::<T>(text).map_err(|e| {
        let line = e.span().map(|s| text[..s.start.min(text.len())].matches('\n').count() + 1);
        Diag::error(path, line, e.message().to_string())
    })
}

pub fn parse_project(tree: &dyn Tree, id: &str) -> Result<ProjectFile, Diag> {
    let path = project_path(id);
    let p: ProjectFile = parse_file(tree, &path)?;
    if p.schema != 1 {
        return Err(Diag::error(&path, Some(1), format!("unknown schema {}", p.schema)));
    }
    if p.id != id {
        let line = tree.read_str(&path).and_then(|t| line_of(t, "id"));
        return Err(Diag::error(&path, line, format!("id {:?} must match its directory {id:?}", p.id)));
    }
    Ok(p)
}

/// Resolves one project. Errors are the first problem that stops layering; the full
/// set of problems comes from [`crate::check::check_tree`].
pub fn resolve_project(tree: &dyn Tree, id: &str) -> Result<Resolved, Vec<Diag>> {
    let project = parse_project(tree, id).map_err(|d| vec![d])?;
    if project.recipe_source != "central" {
        return Err(vec![Diag::error(
            &project_path(id),
            tree.read_str(&project_path(id)).and_then(|t| line_of(t, "recipe_source")),
            if project.recipe_source == "project" {
                "recipe_source = \"project\" is not implemented yet (phase 2) — use \"central\"".to_string()
            } else {
                format!("recipe_source must be \"central\" or \"project\", got {:?}", project.recipe_source)
            },
        )]);
    }
    let rpath = recipe_path(id);
    let recipe: RecipeFile = parse_file(tree, &rpath).map_err(|d| vec![d])?;
    let mut diags = vec![];
    if recipe.schema != 1 {
        diags.push(Diag::error(&rpath, Some(1), format!("unknown schema {}", recipe.schema)));
    }
    let mut files = vec![project_path(id), rpath.clone(), "toolchains.toml".to_string()];

    let mut layers: Vec<(String, RecipeFile)> = vec![];
    for name in &recipe.extends {
        let p = format!("presets/{name}.toml");
        match parse_file::<RecipeFile>(tree, &p) {
            Ok(preset) => {
                for key in preset_violations(&preset) {
                    diags.push(Diag::error(&p, None, format!("a preset may not use {key}")));
                }
                files.push(p.clone());
                layers.push((p, preset));
            }
            Err(mut d) => {
                if d.message == "file not found" {
                    d = Diag::error(&rpath, tree.read_str(&rpath).and_then(|t| line_of(t, "extends")), format!("unknown preset {name:?}"));
                }
                diags.push(d);
            }
        }
    }
    layers.push((rpath.clone(), recipe.clone()));

    let mut out = Resolved {
        id: id.to_string(),
        description: recipe.description.clone(),
        project,
        vars: BTreeMap::new(),
        toolchains: BTreeMap::new(),
        keep: vec![],
        clean_env: BTreeMap::new(),
        env: BTreeMap::new(),
        isolate: BTreeMap::new(),
        account_env: BTreeMap::new(),
        params: BTreeMap::new(),
        steps: vec![],
        fixtures: BTreeMap::new(),
        files: vec![],
    };

    for (origin, layer) in &layers {
        out.vars.extend(layer.vars.clone());
        out.toolchains.extend(layer.toolchains.clone());
        out.env.extend(layer.env.clone());
        out.isolate.extend(layer.isolate.clone());
        out.account_env.extend(layer.account_env.clone());
        out.params.extend(layer.params.clone());
        if let Some(ws) = &layer.workspace {
            for k in &ws.keep {
                if !out.keep.contains(k) {
                    out.keep.push(k.clone());
                }
            }
            out.clean_env.extend(ws.clean_env.clone());
        }
        let mut seen_here = std::collections::BTreeSet::new();
        for s in &layer.step {
            if !seen_here.insert(s.id.clone()) {
                diags.push(step_diag(tree, origin, &s.id, format!("step {:?} is defined twice in this file", s.id)));
            }
            replace_step(&mut out.steps, s, origin, tree, &mut diags);
        }
        for (name, f) in &layer.fixture {
            out.fixtures.insert(name.clone(), f.clone());
        }
    }

    // fragments: appended; same ids as recipe or other fragments are errors.
    for inc in &recipe.include {
        let p = format!("projects/{id}/{inc}");
        if !crate::tree::is_safe_rel(inc) || inc.contains('/') {
            diags.push(Diag::error(&rpath, tree.read_str(&rpath).and_then(|t| line_of(t, "include")), format!("include {inc:?} must be a file in the same directory")));
            continue;
        }
        match parse_file::<RecipeFile>(tree, &p) {
            Ok(frag) => {
                for key in fragment_violations(&frag) {
                    diags.push(Diag::error(&p, None, format!("an include may only hold step, fixture and params — found {key}")));
                }
                files.push(p.clone());
                for s in &frag.step {
                    if out.steps.iter().any(|x| x.def.id == s.id) {
                        diags.push(step_diag(tree, &p, &s.id, format!("step {:?} already exists — replacing only happens through extends", s.id)));
                        continue;
                    }
                    if s.disabled {
                        continue;
                    }
                    match s.kind {
                        Some(kind) => out.steps.push(Step { def: s.clone(), kind, origin: p.clone() }),
                        None => diags.push(step_diag(tree, &p, &s.id, format!("step {:?} has no kind", s.id))),
                    }
                }
                for (name, f) in &frag.fixture {
                    if out.fixtures.contains_key(name) {
                        diags.push(Diag::error(&p, None, format!("fixture {name:?} already exists")));
                    } else {
                        out.fixtures.insert(name.clone(), f.clone());
                    }
                }
                for (name, prm) in &frag.params {
                    if out.params.contains_key(name) {
                        diags.push(Diag::error(&p, None, format!("parameter {name:?} already exists")));
                    } else {
                        out.params.insert(name.clone(), prm.clone());
                    }
                }
            }
            Err(mut d) => {
                if d.message == "file not found" {
                    d = Diag::error(&rpath, tree.read_str(&rpath).and_then(|t| line_of(t, "include")), format!("include {inc:?} not found"));
                }
                diags.push(d);
            }
        }
    }
    out.files = files;
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    Ok(out)
}

fn replace_step(steps: &mut Vec<Step>, s: &StepDef, origin: &str, tree: &dyn Tree, diags: &mut Vec<Diag>) {
    let pos = steps.iter().position(|x| x.def.id == s.id);
    if s.disabled {
        if let Some(i) = pos {
            steps.remove(i);
        }
        return;
    }
    let Some(kind) = s.kind else {
        diags.push(step_diag(tree, origin, &s.id, format!("step {:?} has no kind", s.id)));
        return;
    };
    let step = Step { def: s.clone(), kind, origin: origin.to_string() };
    match pos {
        Some(i) => steps[i] = step,
        None => steps.push(step),
    }
}

pub(crate) fn step_diag(tree: &dyn Tree, file: &str, step: &str, msg: String) -> Diag {
    let line = tree.read_str(file).and_then(|t| line_of(t, &format!("\"{step}\"")));
    Diag::error(file, line, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::MemTree;

    fn tree() -> MemTree {
        let mut t = MemTree::new();
        t.insert("toolchains.toml", "schema = 1\n");
        t.insert(
            "presets/rust-cargo.toml",
            "schema = 1\n[vars]\nrust_dir = \".\"\n[workspace]\nkeep = [\"t\"]\n[[step]]\nid = \"cargo-build\"\nkind = \"build\"\nrun = [\"cargo\", \"build\"]\n[[step]]\nid = \"cargo-fmt\"\nkind = \"test\"\nrun = [\"cargo\", \"fmt\"]\nuncounted = true\ndefault = false\n",
        );
        t.insert(
            "projects/a/project.toml",
            "schema = 1\nid = \"a\"\nrepo = \"https://x/a.git\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\n",
        );
        t.insert(
            "projects/a/recipe.toml",
            "schema = 1\nextends = [\"rust-cargo\"]\ninclude = [\"more.toml\"]\n[vars]\nrust_dir = \"backend\"\n[workspace]\nkeep = [\"u\"]\n[[step]]\nid = \"cargo-fmt\"\ndisabled = true\n[[step]]\nid = \"cargo-build\"\nkind = \"build\"\nrun = [\"cargo\", \"build\", \"--locked\"]\n",
        );
        t.insert("projects/a/more.toml", "schema = 1\n[[step]]\nid = \"extra\"\nkind = \"test\"\nrun = [\"x\"]\nuncounted = true\n");
        t
    }

    #[test]
    fn layering() {
        let r = resolve_project(&tree(), "a").unwrap();
        assert_eq!(r.vars["rust_dir"], "backend");
        assert_eq!(r.keep, vec!["t", "u"]);
        let ids: Vec<&str> = r.steps.iter().map(|s| s.id()).collect();
        assert_eq!(ids, vec!["cargo-build", "extra"]);
        assert_eq!(r.step("cargo-build").unwrap().def.run, vec!["cargo", "build", "--locked"]);
        assert_eq!(r.step("cargo-build").unwrap().origin, "projects/a/recipe.toml");
    }

    #[test]
    fn include_duplicate_is_error() {
        let mut t = tree();
        t.insert("projects/a/more.toml", "schema = 1\n[[step]]\nid = \"cargo-build\"\nkind = \"test\"\nrun = [\"x\"]\n");
        let errs = resolve_project(&t, "a").unwrap_err();
        assert!(errs.iter().any(|d| d.message.contains("already exists")), "{errs:?}");
    }

    #[test]
    fn unknown_key_has_line() {
        let mut t = tree();
        t.insert("projects/a/recipe.toml", "schema = 1\n[[step]]\nid = \"x\"\nkind = \"build\"\nrun = [\"a\"]\ntimeot_min = 3\n");
        let errs = resolve_project(&t, "a").unwrap_err();
        assert_eq!(errs[0].line, Some(6), "{errs:?}");
        assert!(errs[0].message.contains("timeot_min"), "{errs:?}");
    }
}
