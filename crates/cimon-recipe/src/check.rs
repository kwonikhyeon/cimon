//! Static checks (recipe-spec §15.1) — everything that can be decided from the recipe
//! repository alone.

use crate::catalog::{Catalog, CHECKSUM_SOURCES, INSTALLERS};
use crate::model::*;
use crate::resolve::{line_of, parse_file, project_path, recipe_path, resolve_project, step_diag, Resolved};
use crate::tree::{case_collision, is_safe_rel, Tree};
use crate::vars::{self, Known};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diag {
    pub severity: Severity,
    pub file: String,
    pub line: Option<usize>,
    pub message: String,
    /// The project this belongs to, when there is one — used to tell which projects an
    /// edit affects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl Diag {
    pub fn error(file: &str, line: Option<usize>, message: impl Into<String>) -> Diag {
        Diag { severity: Severity::Error, file: file.to_string(), line, message: message.into(), project: None }
    }
    pub fn warning(file: &str, line: Option<usize>, message: impl Into<String>) -> Diag {
        Diag { severity: Severity::Warning, file: file.to_string(), line, message: message.into(), project: None }
    }
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
    /// Identity across commits — lines move when a file is edited, so they are left out.
    /// The project is part of it: the same preset error reached by a new project is a
    /// new error for that project.
    pub fn key(&self) -> DiagKey {
        (self.project.clone(), self.file.clone(), self.message.clone())
    }
}

pub type DiagKey = (Option<String>, String, String);

impl std::fmt::Display for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(l) => write!(f, "{}:{}: {}", self.file, l, self.message),
            None => write!(f, "{}: {}", self.file, self.message),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub errors: Vec<Diag>,
    pub warnings: Vec<Diag>,
    /// Projects that resolved without errors.
    #[serde(skip)]
    pub resolved: BTreeMap<String, Resolved>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
    fn push(&mut self, d: Diag) {
        match d.severity {
            Severity::Error => self.errors.push(d),
            Severity::Warning => self.warnings.push(d),
        }
    }
    /// Errors in `self` that `base` did not have (design §6.3: only newly introduced
    /// errors block a save).
    pub fn new_errors_since(&self, base: &Report) -> Vec<Diag> {
        let old: BTreeSet<DiagKey> = base.errors.iter().map(|d| d.key()).collect();
        self.errors.iter().filter(|d| !old.contains(&d.key())).cloned().collect()
    }
    /// Errors that existed in `base` and are still here (reported as warnings on save).
    pub fn preexisting_errors(&self, base: &Report) -> Vec<Diag> {
        let old: BTreeSet<DiagKey> = base.errors.iter().map(|d| d.key()).collect();
        self.errors.iter().filter(|d| old.contains(&d.key())).cloned().collect()
    }
    pub fn errors_for_project(&self, id: &str) -> Vec<&Diag> {
        self.errors
            .iter()
            .filter(|d| d.project.as_deref() == Some(id) || d.file.starts_with(&format!("projects/{id}/")))
            .collect()
    }
}

pub const EXACT_VERSION: &str = r"^[0-9]+\.[0-9]+(\.[0-9]+)*([+\-][0-9A-Za-z.\-]+)?$";

pub fn check_tree(tree: &dyn Tree) -> Report {
    let mut report = Report::default();
    let catalog = match tree.read_str("toolchains.toml") {
        None => {
            report.push(Diag::error("toolchains.toml", None, "toolchain catalog is missing"));
            None
        }
        Some(text) => match Catalog::parse(text) {
            Ok(c) => Some(c),
            Err(e) => {
                let mut d = Diag::error("toolchains.toml", None, e);
                if let Err(pe) = toml::from_str::<crate::catalog::CatalogFile>(text) {
                    d.line = pe.span().map(|s| text[..s.start.min(text.len())].matches('\n').count() + 1);
                    d.message = pe.message().to_string();
                }
                report.push(d);
                None
            }
        },
    };
    // names a Windows checkout would merge or refuse
    for f in tree.files() {
        if !is_safe_rel(f) {
            report.push(Diag::error(f, None, "path is not valid on Windows (trailing dot or space, device name, or a character Windows refuses)"));
        }
    }
    if let Some((a, b)) = case_collision(tree.files().into_iter()) {
        report.push(Diag::error(&a, None, format!("{a} and {b} are the same path on Windows — a checkout would keep only one")));
    }
    let projects = tree.project_ids();
    if let Some(c) = &catalog {
        check_catalog(tree, c, &projects, &mut report);
    }
    // presets on their own, so a broken preset nobody extends still shows up
    for f in tree.files() {
        if let Some(name) = f.strip_prefix("presets/").and_then(|n| n.strip_suffix(".toml")) {
            if name.contains('/') {
                continue;
            }
            if let Err(d) = parse_file::<RecipeFile>(tree, f) {
                report.push(d);
            }
        }
    }
    for f in tree.files() {
        if let Some(rest) = f.strip_prefix("projects/") {
            if let Some((id, file)) = rest.split_once('/') {
                if file == "recipe.toml" && !projects.contains(&id.to_string()) {
                    report.push(Diag::error(f, None, format!("projects/{id}/project.toml is missing")));
                }
            }
        }
    }
    for id in &projects {
        match resolve_project(tree, id) {
            Err(diags) => {
                for mut d in diags {
                    d.project = Some(id.clone());
                    report.push(d);
                }
            }
            Ok(r) => {
                let before = report.errors.len();
                let before_w = report.warnings.len();
                check_project(tree, catalog.as_ref(), &projects, &r, &mut report);
                // everything found while checking this project belongs to it — also
                // what comes from a preset it extends
                for d in report.errors[before..].iter_mut().chain(report.warnings[before_w..].iter_mut()) {
                    if d.project.is_none() {
                        d.project = Some(id.clone());
                    }
                }
                if report.errors.len() == before {
                    report.resolved.insert(id.clone(), r);
                }
            }
        }
    }
    report.errors.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    report.warnings.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    report
}

fn check_catalog(tree: &dyn Tree, c: &Catalog, projects: &[String], report: &mut Report) {
    let text = tree.read_str("toolchains.toml").unwrap_or_default();
    let sha = regex::Regex::new("^[0-9a-f]{40}$").unwrap();
    let python_exact = regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+$").unwrap();
    for (name, k) in &c.kinds {
        let line = line_of(text, &format!("[kind.{name}]"));
        // a warning, not an error: a catalog written before the field must still pass,
        // so that it can be fixed through a proposal
        if k.installer == "uv-tool" && k.python.is_none() {
            report.push(Diag::warning(
                "toolchains.toml",
                line,
                format!("kind {name}: uv-tool without python = \"<exact version>\" — uv installs on the newest Python it can get, which the package's wheels may not cover"),
            ));
        }
        let mut err = |m: String| report.push(Diag::error("toolchains.toml", line, format!("kind {name}: {m}")));
        if !matches!(k.tier.as_str(), "managed" | "base") {
            err(format!("tier must be managed or base, got {:?}", k.tier));
        }
        if !INSTALLERS.contains(&k.installer.as_str()) {
            err(format!("unknown installer {:?}", k.installer));
        }
        if k.is_base() != (k.installer == "base") {
            err("base tier goes with installer = \"base\" and only with it".into());
        }
        if k.version_any && !k.is_base() {
            err("version_any is only allowed on base kinds".into());
        }
        if let Some(p) = &k.probe {
            if p.run.is_empty() {
                err("probe.run is empty".into());
            }
            let has_placeholder = p.expect.contains("{version}") || p.expect.contains("{version_probe}");
            if k.version_any && p.expect.contains('{') {
                err("with version_any, probe.expect must be a fixed string without placeholders".into());
            } else if !k.version_any && !has_placeholder {
                err("probe.expect must contain {version} or {version_probe} — a pattern that matches any version checks nothing".into());
            }
        }
        match k.installer.as_str() {
            "archive" => {
                if k.url.is_none() {
                    err("archive needs url".into());
                }
                match &k.checksum {
                    None => err("archive needs checksum — managed tools without a checksum source are not installed".into()),
                    Some(cs) => {
                        if !CHECKSUM_SOURCES.contains(&cs.source.as_str()) {
                            err(format!("unknown checksum source {:?}", cs.source));
                        }
                        if cs.source == "shasums" && cs.url.is_none() {
                            err("checksum source shasums needs url".into());
                        }
                    }
                }
            }
            "github-release" => {
                if k.repo.is_none() || k.tag.is_none() || k.asset.is_none() {
                    err("github-release needs repo, tag and asset".into());
                }
            }
            "uv-tool" => {
                if k.package.is_none() {
                    err("uv-tool needs package".into());
                }
                if let Some(v) = k.python.as_ref().filter(|v| !python_exact.is_match(v)) {
                    err(format!("uv-tool python must be an exact x.y.z version (3.13 lets uv take any 3.13.*), got {v:?}"));
                }
            }
            "script" => match &k.script {
                None => err("script installer needs script = { from_project, ref, path }".into()),
                Some(s) => {
                    if !sha.is_match(&s.ref_) {
                        err(format!("script.ref must be a full 40-character commit SHA, got {:?} — a branch lets anyone who can push to it change what gets installed", s.ref_));
                    }
                    if !projects.contains(&s.from_project) {
                        err(format!("script.from_project {:?} is not a registered project", s.from_project));
                    }
                    if !is_safe_rel(&s.path) {
                        err(format!("script.path {:?} must be a relative path inside the repository", s.path));
                    }
                }
            },
            "base" => {
                if k.check.is_none() && k.probe.is_none() {
                    err("base kinds need check or probe".into());
                }
            }
            _ => {}
        }
        for b in &k.bin {
            if !b.split(['/', '\\']).all(|c| c != "..") || b.starts_with('/') || b.contains(':') {
                err(format!("bin {b:?} must stay inside the install root"));
            }
        }
    }
}

struct Ctx<'a> {
    tree: &'a dyn Tree,
    r: &'a Resolved,
    report: &'a mut Report,
}

impl Ctx<'_> {
    fn err_step(&mut self, step: &crate::resolve::Step, msg: String) {
        let d = step_diag(self.tree, &step.origin, step.id(), format!("step {:?}: {msg}", step.id()));
        self.report.push(d);
    }
    fn warn_step(&mut self, step: &crate::resolve::Step, msg: String) {
        let mut d = step_diag(self.tree, &step.origin, step.id(), format!("step {:?}: {msg}", step.id()));
        d.severity = Severity::Warning;
        self.report.push(d);
    }
    fn err_recipe(&mut self, needle: &str, msg: String) {
        let path = recipe_path(&self.r.id);
        let line = self.tree.read_str(&path).and_then(|t| line_of(t, needle));
        self.report.push(Diag::error(&path, line, msg));
    }
    fn warn_recipe(&mut self, needle: &str, msg: String) {
        let path = recipe_path(&self.r.id);
        let line = self.tree.read_str(&path).and_then(|t| line_of(t, needle));
        self.report.push(Diag::warning(&path, line, msg));
    }
}

fn check_project(tree: &dyn Tree, catalog: Option<&Catalog>, projects: &[String], r: &Resolved, report: &mut Report) {
    let mut cx = Ctx { tree, r, report };
    let ppath = project_path(&r.id);
    let ptext = tree.read_str(&ppath).unwrap_or_default().to_string();
    let p = &r.project;

    // project.toml
    if !p.repo.starts_with("https://") {
        cx.report.push(Diag::error(&ppath, line_of(&ptext, "repo"), "repo must be an HTTPS git URL"));
    }
    if p.allowed_refs.is_empty() {
        cx.report.push(Diag::error(&ppath, line_of(&ptext, "allowed_refs"), "allowed_refs must not be empty"));
    }
    if !crate::select::ref_allowed(&p.allowed_refs, &p.default_ref) {
        cx.report.push(Diag::error(&ppath, line_of(&ptext, "default_ref"), format!("default_ref {:?} is not in allowed_refs", p.default_ref)));
    }
    if !r.id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
        cx.report.push(Diag::error(&ppath, line_of(&ptext, "id"), "id must match [A-Za-z0-9_.-]+"));
    }
    for s in &p.sibling {
        if s.project == r.id {
            cx.report.push(Diag::error(&ppath, line_of(&ptext, "[[sibling]]"), "a project cannot be its own sibling"));
        } else if !projects.contains(&s.project) {
            cx.report.push(Diag::error(&ppath, line_of(&ptext, &s.project), format!("sibling {:?} is not a registered project", s.project)));
        }
        if !matches!(s.place.as_str(), "isolated" | "beside") {
            cx.report.push(Diag::error(&ppath, line_of(&ptext, "place"), format!("sibling place must be isolated or beside, got {:?}", s.place)));
        }
        for e in s.env.to_vec() {
            if !valid_env_name(&e) {
                cx.report.push(Diag::error(&ppath, line_of(&ptext, &e), format!("sibling env {e:?} is not a valid variable name")));
            }
        }
    }

    // toolchains
    let exact = regex::Regex::new(EXACT_VERSION).unwrap();
    for (kind, req) in &r.toolchains {
        let Some(c) = catalog else { break };
        let Some(def) = c.kinds.get(kind) else {
            cx.err_recipe(&format!("{kind}"), format!("toolchain {kind:?} is not in the catalog (toolchains.toml)"));
            continue;
        };
        if def.is_base() {
            if req.version.is_some() || req.version_file.is_some() {
                cx.err_recipe(kind, format!("toolchain {kind}: base kinds take min, not version"));
            }
        } else {
            if req.version_file.is_none() && !req.ignore_version_files.is_empty() {
                cx.err_recipe(kind, format!("toolchain {kind}: ignore_version_files only goes with version_file"));
            }
            match (&req.version, &req.version_file) {
                (Some(v), None) => {
                    if !exact.is_match(v) {
                        cx.err_recipe(kind, format!("toolchain {kind}: exact version required, got {v:?} — ranges and channel names are not allowed"));
                    }
                }
                (None, Some(f)) => {
                    if kind != "rust" {
                        cx.err_recipe(kind, format!("toolchain {kind}: version_file is only supported for rust"));
                    } else if !is_safe_rel(f) || !(f.ends_with("rust-toolchain.toml") || f.ends_with("rust-toolchain")) {
                        cx.err_recipe(kind, format!("toolchain {kind}: version_file must be a relative path to rust-toolchain.toml"));
                    }
                    for g in &req.ignore_version_files {
                        if !is_safe_rel(g) || !(g.ends_with("rust-toolchain.toml") || g.ends_with("rust-toolchain")) {
                            cx.err_recipe(kind, format!("toolchain {kind}: ignore_version_files entry {g:?} must be a relative path to a rust-toolchain file"));
                        } else if g == f {
                            cx.err_recipe(kind, format!("toolchain {kind}: ignore_version_files cannot name version_file itself"));
                        }
                    }
                }
                (Some(_), Some(_)) => cx.err_recipe(kind, format!("toolchain {kind}: give version or version_file, not both")),
                (None, None) => cx.err_recipe(kind, format!("toolchain {kind}: managed kinds need version or version_file")),
            }
            if req.min.is_some() {
                cx.err_recipe(kind, format!("toolchain {kind}: min is only for base kinds"));
            }
        }
        for f in req.extra_fields() {
            if !def.fields.iter().any(|x| x == f) {
                cx.err_recipe(kind, format!("toolchain {kind}: field {f} is not declared in the catalog's fields"));
            }
        }
        if let Some(pkgs) = &req.packages {
            for pkg in pkgs {
                if !valid_package_pin(pkg) {
                    cx.err_recipe(pkg, format!("toolchain {kind}: package {pkg:?} must be name==x.y.z (an exact pin; no options, URLs or paths)"));
                }
            }
        }
    }

    // isolate / account_env
    for (k, v) in &r.isolate {
        if !matches!(v.as_str(), "job" | "project") {
            cx.err_recipe(k, format!("[isolate] {k}: scope must be job or project, got {v:?}"));
        }
    }
    for (k, v) in &r.account_env {
        if r.isolate.get(k) != Some(v) {
            cx.err_recipe(k, format!("[account_env] {k} = {v:?} needs the same name and scope in [isolate] — direct steps and scheduled tasks must see the same home"));
        }
    }
    let no_artifacts_env = BTreeSet::new();
    for (k, v) in r.env.iter().chain(r.clean_env.iter()) {
        if !valid_env_name(k) {
            cx.err_recipe(k, format!("env {k:?} is not a valid variable name"));
        }
        if vars::unterminated(v) {
            cx.err_recipe(k, format!("env {k}: unterminated ${{…}} in {v:?}"));
        }
        for name in vars::refs(v) {
            if name.starts_with("param.") || name.starts_with("artifact.") || name == "step.out" {
                cx.err_recipe(k, format!("env {k}: ${{{name}}} is per step — set it in the step's env"));
                continue;
            }
            if let Err(e) = vars::validate_ref(&name, &Known { allow_secret: true, ..known(r, &no_artifacts_env, false) }) {
                cx.err_recipe(k, format!("env {k}: {e}"));
            }
        }
        check_recipes_path(&mut cx, v, k);
    }
    for k in ["CARGO_TARGET_DIR", "RUSTFLAGS"] {
        if r.env.contains_key(k) {
            cx.warn_recipe(k, format!("[env] sets {k} — cimon does not set it by default; make sure the project expects it"));
        }
    }
    for (k, v) in &r.clean_env {
        if let Some(base) = r.env.get(k) {
            if !v.contains(base.as_str()) {
                cx.warn_recipe(k, format!("[workspace].clean_env {k} replaces the [env] value {base:?} without containing it — values are not merged"));
            }
        }
    }

    // params
    let step_ids: BTreeSet<&str> = r.steps.iter().map(|s| s.id()).collect();
    let mut param_users: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in &r.steps {
        for text in step_strings(&s.def) {
            for name in vars::refs(&text) {
                if let Some(p) = name.strip_prefix("param.") {
                    param_users.entry(p.to_string()).or_default().insert(s.id().to_string());
                }
            }
        }
    }
    for (name, prm) in &r.params {
        let needle = format!("[params.{name}]");
        if !matches!(prm.type_.as_str(), "string" | "int" | "bool" | "enum") {
            cx.err_recipe(&needle, format!("param {name}: type must be string, int, bool or enum"));
        }
        if prm.type_ == "enum" && prm.values.as_ref().is_none_or(|v| v.is_empty()) {
            cx.err_recipe(&needle, format!("param {name}: enum needs values"));
        }
        if let Some(pat) = &prm.pattern {
            if regex::Regex::new(pat).is_err() {
                cx.err_recipe(&needle, format!("param {name}: pattern does not compile"));
            }
        }
        if let Some(d) = &prm.default {
            if let Err(e) = crate::select::validate_param_value(prm, &toml_to_json(d)) {
                cx.err_recipe(&needle, format!("param {name}: default is invalid: {e}"));
            }
        }
        if let Some(env) = &prm.env {
            if !valid_env_name(env) {
                cx.err_recipe(&needle, format!("param {name}: env {env:?} is not a valid variable name"));
            }
        }
        match &prm.applies_to {
            None => cx.err_recipe(&needle, format!("param {name}: applies_to is required — name the steps that really use the value")),
            Some(list) => {
                for a in list {
                    if !step_ids.contains(a.as_str()) {
                        cx.err_recipe(&needle, format!("param {name}: applies_to names unknown step {a:?}"));
                    }
                }
                if let Some(users) = param_users.get(name) {
                    for u in users {
                        if !list.contains(u) {
                            cx.err_recipe(&needle, format!("param {name}: ${{param.{name}}} is used in step {u:?}, which is not in applies_to"));
                        }
                    }
                }
                if prm.env.is_none() && prm.args.is_none() && !param_users.contains_key(name) {
                    cx.err_recipe(&needle, format!("param {name}: no consumer — set env or args, or use ${{param.{name}}} in an applies_to step"));
                }
            }
        }
        if let Some(args) = &prm.args {
            for a in args {
                for v in vars::refs(a) {
                    if v != "value" {
                        cx.err_recipe(&needle, format!("param {name}: args may only use ${{value}}"));
                    }
                }
            }
        }
    }

    // steps
    let producers = r.artifact_producers();
    let mut artifact_names = BTreeSet::new();
    let id_re = regex::Regex::new("^[a-z0-9][a-z0-9-]*$").unwrap();
    let has_siblings = !p.sibling.is_empty();
    let mut any_default = false;
    let fixtures = r.fixtures.clone();
    for s in r.steps.clone().iter() {
        let d = &s.def;
        if !id_re.is_match(d.id.as_str()) {
            cx.err_step(s, "id must match [a-z0-9][a-z0-9-]*".into());
        }
        if d.run.is_empty() {
            cx.err_step(s, "run is empty".into());
        }
        if let Some(sh) = &d.shell {
            if !matches!(sh.as_str(), "bash" | "powershell") {
                cx.err_step(s, format!("shell must be bash or powershell, got {sh:?}"));
            }
        }
        if let Some(cwd) = &d.cwd {
            let expanded = crate::refs::expand_vars(cwd, &r.vars).replace('\\', "/");
            let rel = match expanded.strip_prefix("${ws}") {
                Some("") => Some(String::new()),
                Some(rest) => rest.strip_prefix('/').map(str::to_string),
                None => Some(expanded.clone()),
            };
            let rel = rel.map(|x| crate::tree::normalize(&x).trim_end_matches('/').to_string());
            let ok = match &rel {
                Some(x) if x.is_empty() || x == "." => true,
                Some(x) if x.contains("${") => !x.split('/').any(|c| c == ".."),
                Some(x) => is_safe_rel(x),
                None => false,
            };
            if !ok {
                cx.err_step(s, format!("cwd {cwd:?} must be a relative path inside the work tree"));
            }
        }
        if d.timeout_min == Some(0) {
            cx.err_step(s, "timeout_min must be at least 1".into());
        }
        for a in &d.after {
            if !step_ids.contains(a.as_str()) {
                cx.err_step(s, format!("after → unknown step {a:?}"));
            }
            if a == d.id.as_str() {
                cx.err_step(s, "a step cannot run after itself".into());
            }
        }
        for n in &d.needs {
            if !matches!(n.as_str(), "display" | "device" | "gpu") {
                cx.err_step(s, format!("needs: unknown capability {n:?}"));
            }
        }
        for u in &d.uses {
            match r.toolchains.get(u) {
                None => cx.err_step(s, format!("uses → toolchain {u:?} is not declared")),
                Some(t) if !t.optional => cx.err_step(s, format!("uses → toolchain {u:?} is not optional (only optional toolchains go in uses)")),
                _ => {}
            }
        }
        if let Some(f) = &d.fixture {
            if !fixtures.contains_key(f) {
                cx.err_step(s, format!("fixture {f:?} does not exist"));
            }
        }
        if s.kind != Kind::Setup && (d.when.is_some() || !d.inputs.is_empty()) {
            cx.err_step(s, "when and inputs are only for setup steps".into());
        }
        if let Some(w) = &d.when {
            if !matches!(w.as_str(), "always" | "cold" | "changed") {
                cx.err_step(s, format!("when must be always, cold or changed, got {w:?}"));
            }
            if w == "changed" && d.inputs.is_empty() {
                cx.err_step(s, "when = \"changed\" needs inputs".into());
            }
        }
        if s.kind == Kind::Setup && d.default.is_some() {
            cx.err_step(s, "setup steps always run first; default does not apply".into());
        }
        for pat in &d.error_patterns {
            if regex::Regex::new(pat).is_err() {
                cx.err_step(s, format!("error_patterns: {pat:?} does not compile"));
            }
        }
        if s.kind != Kind::Setup && s.is_default() {
            any_default = true;
        }
        let judge_type = d.judge.as_ref().map(|j| j.type_.as_str()).unwrap_or("exit");
        match judge_type {
            "exit" => {}
            "cargo-test" => {}
            "junit" => {
                if d.judge.as_ref().and_then(|j| j.reports.as_ref()).is_none() {
                    cx.err_step(s, "junit judge needs reports".into());
                }
            }
            "script" => {
                if d.judge.as_ref().and_then(|j| j.run.as_ref()).is_none_or(|r| r.is_empty()) {
                    cx.err_step(s, "script judge needs run".into());
                }
            }
            other => cx.err_step(s, format!("unknown judge type {other:?}")),
        }
        if matches!(s.kind, Kind::Test | Kind::Scenario) && judge_type == "exit" && !d.uncounted {
            cx.warn_step(s, "exit judge cannot catch \"0 tests ran\" — use cargo-test/junit, or set uncounted = true if counting is impossible".into());
        }
        if d.uncounted && !(matches!(s.kind, Kind::Test | Kind::Scenario) && judge_type == "exit") {
            cx.warn_step(s, "uncounted only means something on a test/scenario step with the exit judge".into());
        }
        if matches!(s.kind, Kind::Setup | Kind::Test | Kind::Scenario) && !d.artifact.is_empty() {
            cx.err_step(s, format!("{} steps cannot have artifacts", s.kind.as_str()));
        }
        for a in &d.artifact {
            if !artifact_names.insert(a.name.clone()) {
                cx.err_step(s, format!("artifact name {:?} is used twice", a.name));
            }
            if a.expect.is_some() == a.expect_min.is_some() {
                cx.err_step(s, format!("artifact {:?}: give exactly one of expect and expect_min", a.name));
            }
            if a.expect == Some(0) || a.expect_min == Some(0) {
                cx.err_step(s, format!("artifact {:?}: expected count must be at least 1", a.name));
            }
            for g in a.glob.to_vec().iter().chain(a.exclude.iter()) {
                if g.starts_with('/') || g.contains(':') || g.split(['/', '\\']).any(|c| c == "..") {
                    cx.err_step(s, format!("artifact {:?}: glob {g:?} must stay inside the work tree", a.name));
                }
            }
            if let Some(pk) = &a.pack {
                if pk != "zip" {
                    cx.err_step(s, format!("artifact {:?}: pack must be \"zip\"", a.name));
                }
            }
            if a.pack.is_none() && a.expect != Some(1) {
                cx.err_step(s, format!("artifact {:?}: without pack = \"zip\" an artifact is a single file — use expect = 1", a.name));
            }
            if !a.name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) || a.name.starts_with('.') {
                cx.err_step(s, format!("artifact name {:?} must be [A-Za-z0-9._-]", a.name));
            }
            for i in &a.inspect {
                match i.normalize() {
                    Err(e) => cx.err_step(s, format!("artifact {:?}: {e}", a.name)),
                    Ok(Inspect::Builtin { name, .. }) if !BUILTIN_INSPECTS.contains(&name.as_str()) => {
                        cx.err_step(s, format!("artifact {:?}: unknown builtin inspect {name:?}", a.name))
                    }
                    _ => {}
                }
            }
        }
        // variables in step strings
        let fixture_artifacts: BTreeSet<String> =
            d.fixture.as_ref().and_then(|f| fixtures.get(f)).map(|f| f.requires_artifacts.iter().cloned().collect()).unwrap_or_default();
        for text in step_command_strings(d) {
            check_refs(&mut cx, s, &text, &fixture_artifacts, false);
        }
        for text in d.env.values() {
            check_refs(&mut cx, s, text, &fixture_artifacts, true);
        }
        for (k, v) in &d.env {
            if !valid_env_name(k) {
                cx.err_step(s, format!("env {k:?} is not a valid variable name"));
            }
            if let Some(base) = r.env.get(k) {
                if !v.contains(base.as_str()) {
                    cx.warn_step(s, format!("env {k} replaces the [env] value without containing it — values are not merged"));
                }
            }
            if let Some(fx) = d.fixture.as_ref().and_then(|f| fixtures.get(f)) {
                if let Some(base) = fx.env.get(k) {
                    if !v.contains(base.as_str()) {
                        cx.warn_step(s, format!("env {k} replaces the fixture's value without containing it"));
                    }
                }
            }
        }
    }
    if !any_default {
        cx.warn_recipe("schema", "no build or test step has default = true — build/test without steps will be rejected".into());
    }
    if has_siblings && !r.steps.iter().any(|s| !s.def.fail_on_output.is_empty()) {
        cx.warn_recipe("schema", "the project reads sibling repositories but no step has fail_on_output — scripts that skip a missing sibling would pass silently".into());
    }
    for (kind, req) in &r.toolchains {
        if req.optional && !r.steps.iter().any(|s| s.def.uses.contains(kind)) {
            cx.warn_recipe(kind, format!("optional toolchain {kind} is not used by any step"));
        }
    }

    // fixtures
    for (name, f) in &fixtures {
        let needle = format!("[fixture.{name}]");
        for a in &f.after {
            if !step_ids.contains(a.as_str()) {
                cx.err_recipe(&needle, format!("fixture {name}: after → unknown step {a:?}"));
            }
        }
        for a in &f.requires_artifacts {
            if !producers.contains_key(a) {
                cx.err_recipe(&needle, format!("fixture {name}: requires unknown artifact {a:?}"));
            }
        }
        if let Some(o) = &f.on_up_failure {
            if !matches!(o.as_str(), "infra" | "code") {
                cx.err_recipe(&needle, format!("fixture {name}: on_up_failure must be infra or code"));
            }
        }
        if f.up.is_empty() {
            cx.err_recipe(&needle, format!("fixture {name}: up is empty"));
        }
        for poll in [&f.ready, &f.gone].into_iter().flatten() {
            let n = [poll.until.exit.is_some(), poll.until.exit_not.is_some(), poll.until.json_all.is_some()].iter().filter(|b| **b).count();
            if n != 1 {
                cx.err_recipe(&needle, format!("fixture {name}: until needs exactly one of exit, exit_not, json_all"));
            }
            if let Some(j) = &poll.until.json_all {
                if !j.path.starts_with('$') {
                    cx.err_recipe(&needle, format!("fixture {name}: json_all.path must be a JSONPath starting with $"));
                }
            }
        }
        let allowed: BTreeSet<String> = f.requires_artifacts.iter().cloned().collect();
        let mut texts = vec![];
        for c in f.up.iter().chain(f.down.iter()) {
            texts.extend(c.run.clone());
        }
        for poll in [&f.ready, &f.gone].into_iter().flatten() {
            texts.extend(poll.run.clone());
            if let Some(ot) = &poll.on_timeout {
                texts.extend(ot.run.clone());
            }
        }
        texts.extend(f.collect.clone());
        let up_text = f.up.iter().flat_map(|c| c.run.iter()).cloned().collect::<Vec<_>>().join(" ");
        if up_text.contains("${artifact.") && f.on_up_failure.is_none() {
            cx.warn_recipe(&needle, format!("fixture {name}: up runs an artifact built by this branch but on_up_failure is not set — a broken build would look like an infra problem"));
        }
        let texts = texts.into_iter().map(|t| (t, false)).chain(f.env.values().map(|v| (v.clone(), true)));
        for (t, allow_secret) in texts {
            if vars::unterminated(&t) {
                cx.err_recipe(&needle, format!("fixture {name}: unterminated ${{…}} in {t:?}"));
            }
            for v in vars::refs(&t) {
                if let Some(a) = v.strip_prefix("artifact.") {
                    if !allowed.contains(a) {
                        cx.err_recipe(&needle, format!("fixture {name}: ${{{v}}} needs {a:?} in requires_artifacts"));
                        continue;
                    }
                }
                if v.starts_with("param.") {
                    cx.err_recipe(&needle, format!("fixture {name}: ${{{v}}} — parameters cannot be used in fixtures"));
                    continue;
                }
                let known = Known { allow_secret, ..known(r, &allowed, false) };
                if let Err(e) = vars::validate_ref(&v, &known) {
                    cx.err_recipe(&needle, format!("fixture {name}: {e}"));
                }
            }
            check_recipes_path(&mut cx, &t, &needle);
        }
    }
    let no_artifacts = BTreeSet::new();
    for (k, allow_secret) in r.keep.iter().map(|k| (k, false)).chain(r.clean_env.values().map(|v| (v, true))) {
        for v in vars::refs(k) {
            let known = Known { allow_secret, ..known(r, &no_artifacts, false) };
            if let Err(e) = vars::validate_ref(&v, &known) {
                cx.err_recipe("[workspace]", format!("[workspace]: {e}"));
            }
        }
    }

    // cycles (after + fixture after/producers)
    let mut edges: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for s in &r.steps {
        let mut deps = s.def.after.clone();
        if let Some(f) = s.def.fixture.as_ref().and_then(|f| fixtures.get(f)) {
            deps.extend(f.after.clone());
            deps.extend(f.requires_artifacts.iter().filter_map(|a| producers.get(a).cloned()));
        }
        edges.insert(s.id(), deps);
    }
    if let Some(cycle) = find_cycle(&edges) {
        cx.err_recipe("[[step]]", format!("steps depend on each other in a cycle: {}", cycle.join(" → ")));
    }
}

pub(crate) fn known<'a>(r: &'a Resolved, artifacts: &'a BTreeSet<String>, allow_value: bool) -> Known<'a> {
    Known {
        vars: &r.vars,
        params: r.params.keys().map(|s| s.as_str()).collect(),
        toolchains: r.toolchains.keys().map(|s| s.as_str()).collect(),
        siblings: r.project.sibling.iter().map(|s| s.project.as_str()).collect(),
        artifacts: artifacts.iter().map(|s| s.as_str()).collect(),
        allow_value,
        allow_secret: false,
    }
}

fn check_refs(cx: &mut Ctx<'_>, s: &crate::resolve::Step, text: &str, fixture_artifacts: &BTreeSet<String>, allow_secret: bool) {
    if vars::unterminated(text) {
        cx.err_step(s, format!("unterminated ${{…}} in {text:?}"));
    }
    for v in vars::refs(text) {
        if let Some(a) = v.strip_prefix("artifact.") {
            if !fixture_artifacts.contains(a) {
                cx.err_step(s, format!("${{{v}}} may not be requested — only artifacts in the step's fixture requires_artifacts can be referenced"));
                continue;
            }
        }
        let k = Known { allow_secret, ..known(cx.r, fixture_artifacts, false) };
        match vars::validate_ref(&v, &k) {
            Err(e) => cx.err_step(s, e),
            Ok(Some(p)) => {
                // a run without the parameter would have nothing to put there
                if cx.r.params.get(&p).is_some_and(|d| d.default.is_none()) {
                    cx.err_step(s, format!("${{param.{p}}} needs a default in [params.{p}] — without one a run that does not pass it has no value"));
                }
            }
            Ok(None) => {}
        }
    }
    let origin = s.origin.clone();
    let needle = format!("\"{}\"", s.id());
    for msg in recipes_path_problems(cx.tree, &crate::refs::expand_vars(text, &cx.r.vars)) {
        let line = cx.tree.read_str(&origin).and_then(|t| line_of(t, &needle));
        cx.report.push(Diag::error(&origin, line, format!("step {:?}: {msg}", s.id())));
    }
}

fn check_recipes_path(cx: &mut Ctx<'_>, text: &str, needle: &str) {
    for msg in recipes_path_problems(cx.tree, &crate::refs::expand_vars(text, &cx.r.vars)) {
        cx.err_recipe(needle, msg);
    }
}

/// Every `${recipes}` reference in `text` that leaves the repository or names nothing.
fn recipes_path_problems(tree: &dyn Tree, text: &str) -> Vec<String> {
    let mut out = vec![];
    for r in crate::refs::recipe_refs(text) {
        if r.path.is_empty() {
            continue;
        }
        if !is_safe_rel(&r.path) {
            out.push(format!("${{recipes}}/{} must stay inside the recipe repository", r.path));
        } else if !path_exists(tree, &r.path) {
            out.push(format!("${{recipes}}/{}{} does not exist in this recipe commit", r.path, if r.dynamic { "/…" } else { "" }));
        }
    }
    out
}

/// A file, or a directory holding at least one file.
pub fn path_exists(tree: &dyn Tree, p: &str) -> bool {
    let p = p.trim_end_matches('/');
    tree.exists(p) || tree.files().iter().any(|f| f.starts_with(&format!("{p}/")))
}

/// Every string of a step that undergoes variable expansion.
pub fn step_strings(d: &StepDef) -> Vec<String> {
    let mut v = step_command_strings(d);
    v.extend(d.env.values().cloned());
    v
}

/// [`step_strings`] but the step's `env` values — the strings a secret may not reach.
fn step_command_strings(d: &StepDef) -> Vec<String> {
    let mut v: Vec<String> = d.run.clone();
    v.extend(d.cwd.clone());
    v.extend(d.inputs.clone());
    if let Some(j) = &d.judge {
        v.extend(j.run.clone().unwrap_or_default());
        v.extend(j.reports.as_ref().map(|r| r.to_vec()).unwrap_or_default());
    }
    for a in &d.artifact {
        v.extend(a.glob.to_vec());
        v.extend(a.exclude.clone());
        v.extend(a.pack_name.clone());
        for i in &a.inspect {
            if let Ok(Inspect::Script { run, .. }) = i.normalize() {
                v.extend(run);
            }
        }
    }
    v
}

/// `name==version` (optionally with extras) — nothing uv could read as an option, a
/// URL or a path.
pub fn valid_package_pin(p: &str) -> bool {
    let Some((name, ver)) = p.split_once("==") else { return false };
    let (base, extras) = match name.split_once('[') {
        Some((b, e)) => match e.strip_suffix(']') {
            Some(e) => (b, Some(e)),
            None => return false,
        },
        None => (name, None),
    };
    let word = |s: &str, extra: &[char]| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(&c));
    base.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && word(base, &['.', '_', '-'])
        && extras.is_none_or(|e| e.split(',').all(|x| word(x.trim(), &['.', '_', '-'])))
        && ver.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && word(ver, &['.', '+', '!', '_', '-'])
}

#[cfg(test)]
#[test]
fn package_pins() {
    assert!(valid_package_pin("ruff==0.6.9"));
    assert!(valid_package_pin("uvicorn[standard]==0.30.1"));
    assert!(!valid_package_pin("--index-url=https://evil==1"));
    assert!(!valid_package_pin("-e==1"));
    assert!(!valid_package_pin("pkg @ https://x==1"));
    assert!(!valid_package_pin("ruff>=0.6"));
    assert!(!valid_package_pin("ruff==-1"));
}

pub fn valid_env_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.chars().next().unwrap().is_ascii_digit()
}

pub fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

fn find_cycle(edges: &BTreeMap<&str, Vec<String>>) -> Option<Vec<String>> {
    fn dfs(n: &str, edges: &BTreeMap<&str, Vec<String>>, state: &mut BTreeMap<String, u8>, stack: &mut Vec<String>) -> Option<Vec<String>> {
        state.insert(n.to_string(), 1);
        stack.push(n.to_string());
        for d in edges.get(n).into_iter().flatten() {
            match state.get(d.as_str()).copied().unwrap_or(0) {
                1 => {
                    let start = stack.iter().position(|x| x == d).unwrap_or(0);
                    let mut c = stack[start..].to_vec();
                    c.push(d.clone());
                    return Some(c);
                }
                0 if edges.contains_key(d.as_str()) => {
                    if let Some(c) = dfs(d, edges, state, stack) {
                        return Some(c);
                    }
                }
                _ => {}
            }
        }
        stack.pop();
        state.insert(n.to_string(), 2);
        None
    }
    let mut state = BTreeMap::new();
    for n in edges.keys() {
        if state.get(*n).copied().unwrap_or(0) == 0 {
            if let Some(c) = dfs(n, edges, &mut state, &mut vec![]) {
                return Some(c);
            }
        }
    }
    None
}
