//! The job result (design §7.4). Stored without links; artifact URLs are minted fresh
//! whenever a result is read (`view`).

use crate::run::StepRec;
use crate::App;
use cimon_core::{job_label, FailureClass, Verdict, Warning};
use cimon_store::JobRow;
use serde_json::{json, Value};

/// A result for a job that never ran steps (intake failure, cancellation, timeout).
pub fn build(app: &App, job: &JobRow, verdict: Option<Verdict>, class: Option<FailureClass>, summary: &str, warnings: &[Warning], reason_code: Option<&str>) -> Value {
    build_with_steps(app, job, verdict, class, summary, &[], warnings, reason_code)
}

#[allow(clippy::too_many_arguments)]
pub fn build_with_steps(
    app: &App,
    job: &JobRow,
    verdict: Option<Verdict>,
    class: Option<FailureClass>,
    summary: &str,
    recs: &[StepRec],
    warnings: &[Warning],
    reason_code: Option<&str>,
) -> Value {
    let plan: crate::jobs::JobPlan = job.plan_json.as_deref().and_then(|p| serde_json::from_str(p).ok()).unwrap_or_default();
    let request: Option<crate::jobs::JobRequest> = serde_json::from_str(&job.request_json).ok();
    let toolchains: serde_json::Map<String, Value> =
        app.store.job_toolchains(job.id).unwrap_or_default().into_iter().map(|(k, v, _, _)| (k, Value::String(v))).collect();
    let steps: Vec<Value> = recs
        .iter()
        .map(|r| {
            let mut s = json!({ "id": r.id, "kind": r.kind.as_str(), "result": r.result, "seconds": (r.seconds * 10.0).round() / 10.0 });
            if let Some(o) = &r.outcome {
                if o.verdict != Verdict::Pass || r.result == "skipped" {
                    s["reason"] = json!(o.reason);
                }
                if let Some(c) = o.class {
                    s["failure_class"] = json!(c.as_str());
                }
            }
            if let Some(obj) = r.extra.as_object() {
                for (k, v) in obj {
                    s[k] = v.clone();
                }
            }
            if let Some(e) = &r.excerpt {
                if r.result != "pass" && r.result != "skipped" {
                    s["first_error"] = json!(e);
                }
            }
            s
        })
        .collect();
    let excluded: Vec<Value> = plan.plan.excluded.iter().map(|(s, r)| json!({"step": s, "reason": r})).collect();
    json!({
        "job_id": job_label(job.id),
        "done": true,
        "tool": job.tool,
        "verdict": verdict.map(|v| v.as_str()),
        "failure_class": class.map(|c| c.as_str()),
        "reason_code": reason_code,
        "summary": summary,
        "warnings": warnings,
        "project": job.project,
        "ref": job.ref_,
        "commit": job.commit_sha,
        "siblings": plan.siblings.iter().map(|s| json!({"project": s.project, "ref": s.ref_, "commit": s.commit})).collect::<Vec<_>>(),
        "trial": job.trial,
        "recipe": { "source": job.recipe_source, "ref": job.recipe_ref, "commit": job.recipe_commit, "sha256": job.recipe_sha256 },
        "workspace": { "mode": job.workspace_mode, "slot": job.workspace_slot, "note": if job.workspace_mode.as_deref() == Some("clean") { Some("clean — 캐시 유지") } else { None } },
        "params": plan.plan.params,
        "excluded": excluded,
        "toolchains": toolchains,
        "steps": steps,
        "clean": request.map(|r| r.args.clean).unwrap_or(false),
    })
}

/// The stored result with fresh artifact links, task-specific fields and the web link.
pub fn view(app: &App, job: &JobRow, task_id: Option<&str>) -> Value {
    let mut v: Value = job.result_json.as_deref().and_then(|r| serde_json::from_str(r).ok()).unwrap_or_else(|| json!({}));
    if let Some(t) = task_id {
        v["task_id"] = json!(t);
        if let Ok(Some(origin)) = app.store.task_origin_job(t) {
            if origin != job.id {
                v["deduplicated"] = json!(true);
                v["follows_job"] = json!(job_label(job.id));
            }
        }
    }
    if v.get("deduplicated").is_none() {
        v["deduplicated"] = json!(false);
    }
    let arts = crate::artifacts::listing(app, job);
    if !arts.is_empty() {
        v["artifacts"] = json!(arts);
        v["download_hint"] = json!("Invoke-WebRequest <url> -OutFile <path>; (Get-FileHash <path>).Hash 를 sha256 과 대조");
    } else if v.get("artifacts").is_none() {
        v["artifacts"] = json!([]);
    }
    if app.config.web.enabled {
        v["job_url"] = json!(format!("{}/ui/jobs/{}", app.config.server.public_url.trim_end_matches('/'), job_label(job.id)));
    }
    v
}

/// Status view for a job that is not finished (job_status, task status message).
pub fn progress(app: &App, job: &JobRow) -> Value {
    let steps = app.store.steps(job.id).unwrap_or_default();
    json!({
        "job_id": job_label(job.id),
        "done": false,
        "state": job.state.as_str(),
        "summary": job.status_message.clone().unwrap_or_default(),
        "project": job.project,
        "ref": job.ref_,
        "commit": job.commit_sha,
        "trial": job.trial,
        "created_by": job.created_by,
        "created_at": job.created_at,
        "steps": steps.iter().map(|s| json!({"id": s.step_id, "kind": s.kind, "result": s.result})).collect::<Vec<_>>(),
        "excluded": app.store.excluded(job.id).unwrap_or_default().into_iter().map(|(s, r)| json!({"step": s, "reason": r})).collect::<Vec<_>>(),
    })
}
