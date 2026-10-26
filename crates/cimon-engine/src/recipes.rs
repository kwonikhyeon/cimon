//! Reading recipes for tools: project list, `describe_project`, static `plan`, and the
//! shared validation every `build`/`test`/`run` call goes through before a task exists.

use crate::App;
use cimon_core::Tool;
use cimon_recipe::select::{self, Plan, SelectRequest};
use cimon_recipe::tree::Tree;
use cimon_recipe::{check_tree, Resolved};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// A caller-facing refusal (design §7.7).
#[derive(Debug, Clone)]
pub struct Rejection {
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl Rejection {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Rejection { code, message: message.into(), details: None }
    }
    pub fn with_details(mut self, v: Value) -> Self {
        self.details = Some(v);
        self
    }
}

impl From<cimon_recipestore::RsError> for Rejection {
    fn from(e: cimon_recipestore::RsError) -> Self {
        let details = (!e.diags.is_empty()).then(|| json!(e.diags.iter().map(|d| d.to_string()).collect::<Vec<_>>()));
        Rejection { code: e.code, message: e.message, details }
    }
}

impl From<cimon_store::StoreError> for Rejection {
    fn from(e: cimon_store::StoreError) -> Self {
        Rejection::new("store_error", e.to_string())
    }
}

/// Arguments common to `build`/`test`/`run`/`plan` (design §7.3).
#[derive(Debug, Clone, Deserialize, Default, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunArgs {
    pub project: String,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
    #[serde(default)]
    pub recipe_ref: Option<String>,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub artifacts: Option<Vec<String>>,
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
    #[serde(default)]
    pub clean: bool,
    /// `[upload.<name>]` → the handle `POST /uploads` returned (design §11.7).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub uploads: BTreeMap<String, String>,
    /// only for `plan`
    #[serde(default)]
    pub tool: Option<String>,
}

impl RunArgs {
    /// A job that gets uploads builds in a one-off clone with its own caches
    /// (design §11.7), whatever the call said.
    pub fn effective_clean(&self, plan: &Plan) -> bool {
        self.clean || !plan.uploads.is_empty()
    }
}

/// Everything a validated call pins down before the job exists.
#[derive(Debug, Clone)]
pub struct Validated {
    pub tool: Tool,
    pub recipe_ref: String,
    pub recipe_commit: String,
    pub recipe_sha256: String,
    pub trial: bool,
    pub resolved: Resolved,
    pub plan: Plan,
    pub ref_: String,
    /// Key under `mirrors\`, `ws\` — `<id>@<repo hash>` for a trial-only project.
    pub project_key: String,
}

pub fn list_projects(app: &App) -> Result<Value, Rejection> {
    let Some(main) = app.recipes.main_commit() else {
        return Ok(json!({ "projects": [], "summary": "레시피 저장소가 비어 있다 — 관리자가 `cimon recipe import --init` 을 실행해야 한다" }));
    };
    let tree = app.recipes.tree(&main)?;
    let report = check_tree(&*tree);
    let mut out = vec![];
    for id in tree.project_ids() {
        match report.resolved.get(&id) {
            Some(r) => out.push(json!({
                "id": id,
                "display_name": r.project.display_name,
                "default_ref": r.project.default_ref,
                "description": r.description,
                "steps": r.steps.len(),
            })),
            None => out.push(json!({
                "id": id,
                "error": report.errors_for_project(&id).iter().map(|d| d.to_string()).collect::<Vec<_>>(),
            })),
        }
    }
    let n = out.len();
    Ok(json!({ "projects": out, "recipe_commit": main, "summary": format!("{n}개 프로젝트 (레시피 main {})", short(&main)) }))
}

/// Resolves a project at a recipe ref, applying the trial rules (design §6.2):
/// a registered project keeps `main`'s `project.toml`; a trial-only project must point
/// at an allowed repository.
pub fn resolve_for(app: &App, project: &str, recipe_ref: Option<&str>) -> Result<(Resolved, String, String, bool, String), Rejection> {
    let main = app.recipes.require_main()?;
    let rref = recipe_ref.unwrap_or(&app.config.recipes.default_branch).to_string();
    let commit = app.recipes.resolve(&rref)?;
    let trial = commit != main || rref != app.config.recipes.default_branch;
    let tree = app.recipes.tree(&commit)?;
    let main_tree = app.recipes.tree(&main)?;
    if !tree.exists(&format!("projects/{project}/project.toml")) {
        let known = tree.project_ids();
        return Err(Rejection::new("unknown_project", format!("project {project:?} does not exist — list_projects: {}", known.join(", "))));
    }
    let report = check_tree(&*tree);
    let Some(mut r) = report.resolved.get(project).cloned() else {
        let errs: Vec<String> = report.errors_for_project(project).iter().map(|d| d.to_string()).collect();
        return Err(Rejection::new(
            "recipe_invalid",
            format!("the recipe of {project} at {} has errors: {}", short(&commit), errs.first().cloned().unwrap_or_default()),
        )
        .with_details(json!(errs)));
    };
    let registered = main_tree.exists(&format!("projects/{project}/project.toml"));
    let mut project_key = project.to_string();
    if trial {
        if registered {
            match cimon_recipe::resolve::parse_project(&*main_tree, project) {
                Ok(p) => r.project = p,
                Err(d) => return Err(Rejection::new("recipe_invalid", format!("main's project.toml for {project} is broken: {d}"))),
            }
        } else {
            let allowed = &app.config.recipes.allowed_repo_prefixes;
            if !allowed.iter().any(|p| r.project.repo.starts_with(p)) {
                return Err(Rejection::new(
                    "trial_ref_not_allowed",
                    format!("{project} exists only on {rref}, and its repo {} is outside [recipes].allowed_repo_prefixes", r.project.repo),
                ));
            }
            project_key = format!("{project}@{}", &cimon_core::sha256_hex(r.project.repo.as_bytes())[..8]);
        }
    }
    // every file that decides what the job runs — two jobs with the same sha must run
    // the same thing (merging relies on it)
    let sha = tree.digest(&cimon_recipe::refs::recipe_files(&*tree, &r));
    Ok((r, commit, sha, trial, project_key))
}

/// Validation shared by `build`/`test`/`run`/`plan` — everything that can be refused
/// before a task is created (design §7.3, §7.7).
pub fn validate(app: &App, tool: Tool, args: &RunArgs) -> Result<Validated, Rejection> {
    let (r, commit, sha, trial, project_key) = resolve_for(app, &args.project, args.recipe_ref.as_deref())?;
    let ref_ = args.ref_.clone().unwrap_or_else(|| r.project.default_ref.clone());
    let is_sha = ref_.len() >= 7 && ref_.len() <= 40 && ref_.bytes().all(|b| b.is_ascii_hexdigit());
    if !is_sha && !select::ref_allowed(&r.project.allowed_refs, &ref_) {
        return Err(Rejection::new(
            "ref_not_allowed",
            format!("{ref_:?} is not an allowed ref: {}", r.project.allowed_refs.join(", ")),
        ));
    }
    if ref_.starts_with('-') || ref_.contains("..") || ref_.contains(char::is_whitespace) {
        return Err(Rejection::new("ref_not_allowed", format!("{ref_:?} is not a branch name or commit")));
    }
    if !r.project.platforms.contains(&app.config.runner.platform) {
        return Err(Rejection::new(
            "needs_unavailable",
            format!("{} builds on {} — this runner is {}", args.project, r.project.platforms.join(", "), app.config.runner.platform),
        ));
    }
    let req = SelectRequest {
        steps: args.steps.clone(),
        tags: args.tags.clone(),
        artifacts: args.artifacts.clone(),
        params: args.params.clone(),
        capabilities: app.config.runner.capabilities.clone(),
    };
    let plan = select::select(&r, tool, &req).map_err(|e| Rejection::new(e.code, e.message))?;
    let rref = args.recipe_ref.clone().unwrap_or_else(|| app.config.recipes.default_branch.clone());
    let v = Validated { tool, recipe_ref: rref, recipe_commit: commit, recipe_sha256: sha, trial, resolved: r, plan, ref_, project_key };
    crate::uploads::check_declared(&v, args)?;
    Ok(v)
}

pub fn describe(app: &App, project: &str, recipe_ref: Option<&str>) -> Result<Value, Rejection> {
    let (r, commit, sha, trial, _) = resolve_for(app, project, recipe_ref)?;
    let steps: Vec<Value> = r
        .steps
        .iter()
        .map(|s| {
            json!({
                "id": s.id(),
                "kind": s.kind.as_str(),
                "default": s.kind != cimon_recipe::Kind::Setup && s.is_default(),
                "description": s.def.description,
                "tags": s.def.tags,
                "needs": s.def.needs,
                "after": s.def.after,
                "fixture": s.def.fixture,
                "judge": s.def.judge.as_ref().map(|j| j.type_.clone()).unwrap_or_else(|| "exit".into()),
                "uncounted": s.def.uncounted,
                "timeout_min": s.timeout_min(),
                "from": s.origin,
                "artifacts": s.def.artifact.iter().map(|a| json!({"name": a.name, "default": a.default.unwrap_or(true), "description": a.description})).collect::<Vec<_>>(),
            })
        })
        .collect();
    let params: Vec<Value> = r
        .params
        .iter()
        .map(|(n, p)| json!({"name": n, "type": p.type_, "default": p.default.as_ref().map(cimon_recipe::check::toml_to_json), "applies_to": p.applies_to, "description": p.description, "values": p.values, "pattern": p.pattern}))
        .collect();
    let n_steps = steps.len();
    Ok(json!({
        "project": r.id,
        "display_name": r.project.display_name,
        "description": r.description,
        "default_ref": r.project.default_ref,
        "allowed_refs": r.project.allowed_refs,
        "siblings": r.project.sibling.iter().map(|s| json!({"project": s.project, "ref": s.ref_, "place": s.place})).collect::<Vec<_>>(),
        "recipe": {"ref": recipe_ref.unwrap_or(&app.config.recipes.default_branch), "commit": commit, "sha256": sha, "trial": trial},
        "toolchains": r.toolchains,
        "params": params,
        "uploads": r.uploads.iter().map(|(n, u)| json!({"name": n, "kind": u.kind_str(), "applies_to": u.applies_to, "refs": u.refs.clone().unwrap_or_else(|| vec![r.project.default_ref.clone()]), "description": u.description})).collect::<Vec<_>>(),
        "steps": steps,
        "fixtures": r.fixtures.keys().collect::<Vec<_>>(),
        "summary": format!("{} — 단계 {n_steps}개, 레시피 {}{}", r.id, short(&commit), if trial { " (시도 브랜치)" } else { "" }),
    }))
}

/// `plan`: what a call would do, without fetching (design §7.2).
pub fn plan(app: &App, user: &str, args: &RunArgs) -> Result<Value, Rejection> {
    let tool = match args.tool.as_deref() {
        None => return Err(Rejection::new("invalid_param", "plan needs tool: build | test | run")),
        Some(t) => Tool::parse(t).ok_or_else(|| Rejection::new("invalid_param", format!("tool must be build, test or run, got {t:?}")))?,
    };
    let v = validate(app, tool, args)?;
    let mut unknowns = vec![];
    for (kind, req) in &v.resolved.toolchains {
        if let Some(f) = &req.version_file {
            unknowns.push(format!("{kind}: 버전은 {f} 에서 접수 때 읽는다 — plan 은 프로젝트를 fetch 하지 않는다"));
        }
    }
    let toolchains = crate::provision_needs(app, &v);
    let plan = &v.plan;
    // the secrets the selected steps' environment names (design §11.6)
    let stored = crate::build_secrets::for_job(app, &args.project, v.trial, user);
    let needs: std::collections::BTreeSet<String> = plan
        .setup
        .iter()
        .chain(plan.steps.iter().map(|s| &s.id))
        .filter_map(|id| v.resolved.step(id))
        .flat_map(|s| crate::build_secrets::step_needs(&v.resolved, s, args.effective_clean(plan)))
        .collect();
    let secrets: Vec<Value> = needs
        .iter()
        .map(|n| {
            let state = match &stored {
                Ok(m) if m.contains_key(n) => "있음".to_string(),
                Ok(_) => crate::build_secrets::missing(&args.project, n),
                Err(e) => e.clone(),
            };
            json!({"name": n, "state": state})
        })
        .collect();
    let summary = format!(
        "{} {} — {}{}{}",
        args.project,
        tool.as_str(),
        plan.step_ids().join(", "),
        if plan.excluded.is_empty() { String::new() } else { format!(" · 제외 {}", plan.excluded.len()) },
        if plan.artifacts.is_empty() { String::new() } else { format!(" · 산출물 {}", plan.artifacts.join(", ")) },
    );
    let mut out = json!({
        "project": args.project,
        "tool": tool.as_str(),
        "ref": v.ref_,
        "recipe": {"ref": v.recipe_ref, "commit": v.recipe_commit, "trial": v.trial},
        "setup": plan.setup,
        "steps": plan.steps.iter().map(|s| json!({"id": s.id, "kind": s.kind.as_str()})).collect::<Vec<_>>(),
        "excluded": plan.excluded.iter().map(|(s, r)| json!({"step": s, "reason": r})).collect::<Vec<_>>(),
        "artifacts": plan.artifacts,
        "params": plan.params,
        "toolchains": toolchains,
        "secrets": secrets,
        "unknown": unknowns,
        "summary": summary,
    });
    if !plan.uploads.is_empty() {
        out["uploads"] = json!(crate::uploads::plan_view(app, user, &v, args));
        out["clean"] = json!("업로드를 받는 작업 — 일회 클론과 작업 전용 캐시로 짓는다");
    }
    Ok(out)
}

pub fn short(c: &str) -> &str {
    &c[..c.len().min(7)]
}
