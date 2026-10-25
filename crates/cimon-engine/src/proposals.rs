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
    // something to promote at all (review-ui §13: "변경 없음" cannot be promoted)
    if diff.is_empty() {
        list.push(Condition { name: "has_changes", state: "fail", detail: format!("head {} 가 main 과 같다 — 반영할 변경 없음", short(head)), overridable: false });
    }
    // steps a trial cannot run here are listed with the untestable changes
    let mut untestable = vec![];
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
            let Ok(r) = cimon_recipe::resolve_project(&*head_tree, &pd.id) else {
                missing.push(format!("{}: head 에서 레시피를 해석하지 못했다", pd.id));
                continue;
            };
            let mut required: Vec<String> = pd.changed_steps.clone();
            if pd.requires_default_run {
                match select(&r, Tool::Run, &SelectRequest { capabilities: app.config.runner.capabilities.clone(), ..Default::default() }) {
                    Ok(plan) => {
                        for s in plan.step_ids() {
                            if !required.contains(&s) {
                                required.push(s);
                            }
                        }
                    }
                    // a project-wide change with nothing to run by default is not "tried"
                    Err(e) => {
                        missing.push(format!("{}: 프로젝트 전체 설정이 바뀌었는데 기본 run 을 고르지 못했다 — {}", pd.id, e.message));
                        continue;
                    }
                }
            }
            if required.is_empty() {
                ok.push(format!("{}: 돌릴 바뀐 단계 없음", pd.id));
                continue;
            }
            let caps = &app.config.runner.capabilities;
            let step_of = |s: &String| r.step(s);
            let excluded: Vec<&String> = required.iter().filter(|s| step_of(s).is_some_and(|st| !st.def.needs.iter().all(|n| caps.contains(n)))).collect();
            if !excluded.is_empty() {
                untestable.push(format!(
                    "{}: 바뀐 단계 {} 는 이 러너({})에서 돌 수 없다(needs) — 시도 실행으로 확인되지 않는다",
                    pd.id,
                    excluded.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
                    app.config.runner.name
                ));
            }
            let setup_changed = required.iter().any(|s| step_of(s).is_some_and(|st| st.kind == cimon_recipe::Kind::Setup));
            let runnable: Vec<String> = required
                .iter()
                .filter(|s| !excluded.contains(s))
                .filter(|s| step_of(s).is_none_or(|st| st.kind != cimon_recipe::Kind::Setup))
                .cloned()
                .collect();
            if runnable.is_empty() && !setup_changed {
                // only needs-excluded steps changed — reported as untestable above
                continue;
            }
            let changed_setup: Vec<&String> = required.iter().filter(|s| step_of(s).is_some_and(|st| st.kind == cimon_recipe::Kind::Setup)).collect();
            let plan_of = |j: &cimon_store::JobRow| -> Option<crate::jobs::JobPlan> { j.plan_json.as_deref().and_then(|s| serde_json::from_str(s).ok()) };
            let jobs = app.store.trial_jobs(&pd.id, &p.branch, head).unwrap_or_default();
            // every trial at head that ran any changed step (setup runs in all of them)
            // counts for failures — a later, wider run that passes does not cover it
            let relevant: Vec<&cimon_store::JobRow> = jobs
                .iter()
                .filter(|j| setup_changed || plan_of(j).is_some_and(|pl| runnable.iter().any(|s| pl.plan.runs(s))))
                .collect();
            // a covering trial ran every changed step, and a changed setup really ran
            // (not "skipped" because a warm slot matched)
            let setup_ran = |j: &cimon_store::JobRow| {
                changed_setup.is_empty() || app.store.steps(j.id).is_ok_and(|ss| changed_setup.iter().all(|sid| ss.iter().any(|s| &s.step_id == *sid && s.result == "pass")))
            };
            let covering: Vec<&cimon_store::JobRow> = relevant
                .iter()
                .copied()
                .filter(|j| plan_of(j).is_some_and(|pl| runnable.iter().all(|s| pl.plan.runs(s))) && setup_ran(j))
                .collect();
            let what = if runnable.is_empty() { "setup".to_string() } else { runnable.join(", ") };
            match covering.first() {
                None => missing.push(format!(
                    "{}: head {} 로 돈 작업 중 바뀐 단계 {what} 를 모두 포함한 것이 없다 — run(project=\"{}\", recipe_ref=\"{}\"{})",
                    pd.id,
                    short(head),
                    pd.id,
                    p.branch,
                    if runnable.is_empty() { String::new() } else { format!(", steps=[{}]", runnable.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(", ")) }
                )),
                Some(latest) => {
                    // any failure at this head counts — a passing retry does not cover it
                    let failed = relevant.iter().find(|j| j.verdict.as_deref() != Some("pass"));
                    match failed {
                        Some(f) if app.config.review.trial_must_pass => missing.push(format!(
                            "{}: head {} 의 시도 {} 가 {}({}) — 재시도로 덮을 수 없다, 고치고 새 head 로",
                            pd.id,
                            short(head),
                            job_label(f.id),
                            f.verdict.clone().unwrap_or_default(),
                            f.failure_class.clone().unwrap_or_default()
                        )),
                        _ => ok.push(format!("{}: {} {}", pd.id, job_label(latest.id), latest.verdict.clone().unwrap_or_default())),
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
    if let Some(e) = &diff.error {
        untestable.push(format!("의미 비교 불가: {e}"));
    }
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

/// Promotes `p` at `head` (design §6.5): admin only, conditions recomputed under the
/// write lock, soft conditions only with a 10+ character reason, bundle backup after.
/// Returns the new main and whether the reason was used to override something.
pub fn promote(app: &App, user: &cimon_core::AuthUser, p: &ProposalRow, head: &str, reason: Option<&str>) -> Result<(String, bool), crate::recipes::Rejection> {
    use crate::recipes::Rejection;
    use cimon_recipestore::RsError;
    if !user.is_admin() {
        return Err(Rejection::new("forbidden", "recipe_promote is admin-only — ask an admin"));
    }
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    let mut used_override = false;
    let promoted = app
        .recipes
        .promote(&app.store, &user.name, p.id, head, reason, |_main, head| {
            let c = conditions(app, p, head).map_err(|e| RsError::new("promote_conditions_unmet", format!("could not compute conditions: {e}")))?;
            let hard = c.hard_failures();
            if !hard.is_empty() {
                let code = if hard.iter().any(|x| x.name == "fast_forward") { "not_fast_forward" } else { "promote_conditions_unmet" };
                return Err(RsError::new(code, hard.iter().map(|x| format!("{}: {}", x.name, x.detail)).collect::<Vec<_>>().join("; ")));
            }
            let soft = c.soft_failures();
            if !soft.is_empty() {
                match reason {
                    Some(r) if r.chars().count() >= 10 => used_override = true,
                    _ => {
                        return Err(RsError::new(
                            "promote_conditions_unmet",
                            format!(
                                "{} — run the trials, or give override_reason (10+ characters, recorded permanently)",
                                soft.iter().map(|x| format!("{}: {}", x.name, x.detail)).collect::<Vec<_>>().join("; ")
                            ),
                        ))
                    }
                }
            }
            Ok(())
        })
        .map_err(Rejection::from)?;
    if !used_override && reason.is_some() {
        // the reason overrode nothing — do not record it as an override
        let _ = app.store.tx(|tx| {
            tx.execute("UPDATE proposals SET override_reason = NULL WHERE id = ?1", [p.id])?;
            Ok(())
        });
    }
    let _ = app.store.audit(cimon_store::AuditEntry {
        user: &user.name,
        action: "recipe_promote",
        proposal_id: Some(p.id),
        recipe_commit: Some(&promoted),
        detail: if used_override { reason.map(str::to_string) } else { None },
        ..Default::default()
    });
    if let Err(e) = crate::backup::backup_recipes(app, "promote") {
        tracing::error!("backup after promote: {e}");
    }
    Ok((promoted, used_override))
}
