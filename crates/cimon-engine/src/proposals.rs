//! Promote conditions (design §6.5). One function used by `recipe_promote`,
//! `recipe_proposal` and the review screen, evaluated against the trees as they are.

use crate::App;
use cimon_core::{job_label, proposal_label, Tool};
use cimon_recipe::diff::{semantic_diff, SemanticDiff};
use cimon_recipe::select::{select, SelectRequest};
use cimon_recipe::Tree;
use cimon_store::ProposalRow;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize)]
pub struct Condition {
    pub name: &'static str,
    /// pass | fail | na | error
    pub state: &'static str,
    pub detail: String,
    /// Can an admin promote anyway with a written reason?
    pub overridable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Conditions {
    pub head: String,
    pub main: String,
    pub list: Vec<Condition>,
    #[serde(skip)]
    pub diff: SemanticDiff,
}

impl Conditions {
    pub fn all_pass(&self) -> bool {
        self.list.iter().all(|c| matches!(c.state, "pass" | "na"))
    }
    /// Failing conditions that no reason can override.
    pub fn hard_failures(&self) -> Vec<&Condition> {
        self.list.iter().filter(|c| !matches!(c.state, "pass" | "na") && !c.overridable).collect()
    }
    pub fn soft_failures(&self) -> Vec<&Condition> {
        self.list.iter().filter(|c| !matches!(c.state, "pass" | "na") && c.overridable).collect()
    }
}

pub fn find(app: &App, key: &str) -> Result<ProposalRow, crate::recipes::Rejection> {
    use crate::recipes::Rejection;
    if let Some(id) = cimon_core::parse_proposal_label(key) {
        return app.store.proposal(id)?.ok_or_else(|| Rejection::new("not_found", format!("no proposal {}", proposal_label(id))));
    }
    if key.starts_with(&app.config.recipes.trial_prefix) {
        return app.store.open_proposal_for_branch(key)?.ok_or_else(|| Rejection::new("not_found", format!("{key} has no open proposal")));
    }
    Err(Rejection::new("invalid_param", format!("{key:?} is not a proposal id (p0012) or a {}* branch", app.config.recipes.trial_prefix)))
}

/// Computes every promote condition for a proposal at `head`.
pub fn conditions(app: &App, p: &ProposalRow, head: &str) -> Result<Conditions, String> {
    let main = app.recipes.require_main().map_err(|e| e.to_string())?;
    let main_tree = app.recipes.tree(&main).map_err(|e| e.to_string())?;
    let head_tree = app.recipes.tree(head).map_err(|e| e.to_string())?;
    let mut list = vec![];

    // static check — errors this proposal introduces relative to main
    match cimon_recipestore::gate(&main_tree, &head_tree) {
        Ok(_) => list.push(Condition { name: "static_check", state: "pass", detail: format!("head {} — 새 오류 없음", short(head)), overridable: false }),
        Err(e) => list.push(Condition {
            name: "static_check",
            state: "fail",
            detail: format!("{}: {}", e.message, e.diags.iter().take(5).map(|d| d.to_string()).collect::<Vec<_>>().join("; ")),
            overridable: false,
        }),
    }
    // fast-forward
    if app.recipes.git.is_ancestor(&main, head) {
        list.push(Condition { name: "fast_forward", state: "pass", detail: format!("main {} 이 head 에 포함됨", short(&main)), overridable: false });
    } else {
        list.push(Condition {
            name: "fast_forward",
            state: "fail",
            detail: format!("main 이 {} 로 움직였다 — recipe_sync 로 맞춘 뒤 새 head 로 시도 실행을 다시", short(&main)),
            overridable: false,
        });
    }

    let diff = semantic_diff(&*main_tree, &*head_tree);
    // trial runs (design §6.5)
    if app.config.review.require_trial == cimon_core::config::RequireTrial::None {
        list.push(Condition { name: "trial", state: "na", detail: "[review].require_trial = none".into(), overridable: true });
    } else {
        let mut missing = vec![];
        let mut ok = vec![];
        for pd in diff.projects.iter().filter(|p| p.change != "removed") {
            if pd.error.is_some() {
                missing.push(format!("{}: 레시피를 해석하지 못했다 — {}", pd.id, pd.error.clone().unwrap_or_default()));
                continue;
            }
            let mut required: Vec<String> = pd.changed_steps.clone();
            if pd.requires_default_run {
                if let Ok(r) = cimon_recipe::resolve_project(&*head_tree, &pd.id) {
                    if let Ok(plan) = select(&r, Tool::Run, &SelectRequest { capabilities: app.config.runner.capabilities.clone(), ..Default::default() }) {
                        for s in plan.step_ids() {
                            if !required.contains(&s) {
                                required.push(s);
                            }
                        }
                    }
                }
            }
            if required.is_empty() {
                ok.push(format!("{}: 돌릴 바뀐 단계 없음", pd.id));
                continue;
            }
            // excluded steps cannot be tried on this runner; they do not block
            let r = cimon_recipe::resolve_project(&*head_tree, &pd.id).ok();
            let caps = &app.config.runner.capabilities;
            let runnable: Vec<String> = required
                .iter()
                .filter(|s| r.as_ref().and_then(|r| r.step(s)).is_none_or(|st| st.def.needs.iter().all(|n| caps.contains(n))))
                .filter(|s| r.as_ref().and_then(|r| r.step(s)).is_none_or(|st| st.kind != cimon_recipe::Kind::Setup))
                .cloned()
                .collect();
            if runnable.is_empty() {
                ok.push(format!("{}: 바뀐 단계가 이 러너에서 돌 수 없다(제외) — 시도 실행 불가", pd.id));
                continue;
            }
            let jobs = app.store.trial_jobs(&pd.id, &p.branch, head).unwrap_or_default();
            let latest = jobs.iter().find(|j| {
                let plan: Option<crate::jobs::JobPlan> = j.plan_json.as_deref().and_then(|s| serde_json::from_str(s).ok());
                plan.is_some_and(|pl| runnable.iter().all(|s| pl.plan.runs(s)))
            });
            match latest {
                None => missing.push(format!(
                    "{}: head {} 로 돈 작업 중 바뀐 단계 {} 를 모두 포함한 것이 없다 — run(project=\"{}\", recipe_ref=\"{}\", steps=[{}])",
                    pd.id,
                    short(head),
                    runnable.join(", "),
                    pd.id,
                    p.branch,
                    runnable.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(", ")
                )),
                Some(j) => {
                    let v = j.verdict.clone().unwrap_or_default();
                    if v == "pass" || !app.config.review.trial_must_pass {
                        ok.push(format!("{}: {} {v}", pd.id, job_label(j.id)));
                    } else {
                        missing.push(format!("{}: 가장 최근 시도 {} 가 {v}({}) — 재시도로 덮을 수 없다, 고치고 새 head 로", pd.id, job_label(j.id), j.failure_class.clone().unwrap_or_default()));
                    }
                }
            }
        }
        if diff.projects.is_empty() {
            list.push(Condition { name: "trial", state: "na", detail: "영향받는 프로젝트 없음".into(), overridable: true });
        } else if missing.is_empty() {
            list.push(Condition { name: "trial", state: "pass", detail: ok.join("; "), overridable: true });
        } else {
            list.push(Condition { name: "trial", state: "fail", detail: missing.join("; "), overridable: true });
        }
    }

    // changes a trial cannot exercise (design §6.5)
    let mut untestable = vec![];
    for pd in &diff.projects {
        if pd.project_toml_changed {
            untestable.push(format!("{}: project.toml(저장소·허용 ref·형제) — 시도 실행은 main 의 것을 쓴다", pd.id));
        }
    }
    let head_cat = cimon_recipe::Catalog::parse(head_tree.read_str("toolchains.toml").unwrap_or("")).ok();
    for k in &diff.catalog {
        if k.change == "removed" {
            continue;
        }
        let entry = head_cat.as_ref().and_then(|c| c.hash(&k.kind)).unwrap_or_default().to_string();
        let tried = app.store.installs().unwrap_or_default().iter().any(|i| i.kind == k.kind && i.entry_sha256 == entry)
            && trial_used_entry(app, &p.branch, head, &k.kind, &entry);
        if !tried {
            untestable.push(format!("카탈로그 {} ({}) — 관리자가 `cimon provision --recipe-ref {} --kind {}` 로 설치해 시도 실행에 쓰면 시험된 것으로 친다", k.kind, k.change, p.branch, k.kind));
        }
    }
    if untestable.is_empty() {
        list.push(Condition { name: "untestable_changes", state: "na", detail: "없음".into(), overridable: true });
    } else {
        list.push(Condition { name: "untestable_changes", state: "fail", detail: untestable.join("; "), overridable: true });
    }
    Ok(Conditions { head: head.to_string(), main, list, diff })
}

fn trial_used_entry(app: &App, branch: &str, head: &str, kind: &str, entry: &str) -> bool {
    app.store.trial_jobs_for_ref(branch).unwrap_or_default().iter().filter(|j| j.recipe_commit.as_deref() == Some(head)).any(|j| {
        app.store.job_toolchains(j.id).unwrap_or_default().iter().any(|(k, _, e, _)| k == kind && e == entry)
    })
}

/// `recipe_proposal` body: metadata, conditions, semantic diff, trials, comments.
pub fn view(app: &App, p: &ProposalRow) -> Value {
    let head = app.recipes.branch_head(&p.branch);
    let conds = head.as_ref().map(|h| conditions(app, p, h));
    let trials: Vec<Value> = app
        .store
        .trial_jobs_for_ref(&p.branch)
        .unwrap_or_default()
        .iter()
        .take(30)
        .map(|j| {
            json!({"job_id": job_label(j.id), "project": j.project, "tool": j.tool, "state": j.state.as_str(), "verdict": j.verdict,
                   "failure_class": j.failure_class, "recipe_commit": j.recipe_commit, "current_head": j.recipe_commit == head,
                   "steps": j.plan_json.as_deref().and_then(|s| serde_json::from_str::<crate::jobs::JobPlan>(s).ok()).map(|pl| pl.plan.step_ids()).unwrap_or_default()})
        })
        .collect();
    let comments: Vec<Value> = app
        .store
        .comments(p.id)
        .unwrap_or_default()
        .iter()
        .map(|c| json!({"author": c.author, "body": c.body, "head": c.head_commit, "at": c.created_at, "outdated": Some(c.head_commit.clone()) != head}))
        .collect();
    let (cond_json, diff_json, risks) = match &conds {
        Some(Ok(c)) => {
            let risks: Vec<&str> = c.diff.risks().iter().map(|r| r.label()).collect();
            (json!(c.list), json!(c.diff), json!(risks))
        }
        Some(Err(e)) => (json!([{"name": "computation", "state": "error", "detail": e, "overridable": false}]), json!({"error": e}), json!([])),
        None => (json!([]), json!(null), json!([])),
    };
    let summary = match (&p.status[..], &conds) {
        ("open", Some(Ok(c))) => format!(
            "{} {} — {}",
            proposal_label(p.id),
            p.branch,
            if c.all_pass() {
                "반영 조건 충족".to_string()
            } else {
                format!("미충족: {}", c.list.iter().filter(|x| !matches!(x.state, "pass" | "na")).map(|x| x.name).collect::<Vec<_>>().join(", "))
            }
        ),
        _ => format!("{} {} — {}", proposal_label(p.id), p.branch, p.status),
    };
    let mut v = json!({
        "proposal": proposal_label(p.id),
        "branch": p.branch,
        "title": p.title,
        "description": p.description,
        "author": p.author,
        "status": p.status,
        "head": head,
        "base": p.base_commit,
        "created_at": p.created_at,
        "updated_at": p.updated_at,
        "promoted_commit": p.promoted_commit,
        "promoted_by": p.promoted_by,
        "override_reason": p.override_reason,
        "closed_by": p.closed_by,
        "close_reason": p.close_reason,
        "conditions": cond_json,
        "risks": risks,
        "semantic_diff": diff_json,
        "trials": trials,
        "comments": comments,
        "summary": summary,
    });
    if app.config.web.enabled {
        v["url"] = json!(format!("{}/ui/proposals/{}", app.config.server.public_url.trim_end_matches('/'), proposal_label(p.id)));
    }
    v
}

pub fn short(c: &str) -> &str {
    &c[..c.len().min(7)]
}
