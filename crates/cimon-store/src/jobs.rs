//! jobs · tasks · steps · excluded · artifacts · toolchains · warm slots.

use crate::{opt_state, Result, Store, StoreError};
use cimon_core::{now, JobState};
use rusqlite::{params, OptionalExtension, Row};

#[derive(Debug, Clone)]
pub struct NewJob {
    pub tool: String,
    pub project: String,
    pub project_key: String,
    pub ref_: Option<String>,
    pub recipe_source: String,
    pub recipe_ref: String,
    pub recipe_commit: String,
    pub recipe_sha256: String,
    pub trial: bool,
    pub request_json: String,
    pub created_by: String,
    pub status_message: String,
    pub expires_at: String,
}

#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: i64,
    pub tool: String,
    pub project: String,
    pub project_key: String,
    pub ref_: Option<String>,
    pub commit_sha: Option<String>,
    pub siblings_json: Option<String>,
    pub recipe_source: Option<String>,
    pub recipe_ref: String,
    pub recipe_commit: Option<String>,
    pub recipe_sha256: Option<String>,
    pub trial: bool,
    pub request_json: String,
    pub plan_json: Option<String>,
    pub merge_key: Option<String>,
    pub merged_into: Option<i64>,
    pub state: JobState,
    pub verdict: Option<String>,
    pub failure_class: Option<String>,
    pub reason_code: Option<String>,
    pub status_message: Option<String>,
    pub summary: Option<String>,
    pub warnings_json: Option<String>,
    pub result_json: Option<String>,
    pub created_by: String,
    pub cancel_requested_by: Option<String>,
    pub workspace_mode: Option<String>,
    pub workspace_slot: Option<i64>,
    pub fixture_state: Option<String>,
    pub created_at: String,
    pub ready_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub last_updated_at: String,
    pub expires_at: String,
}

const JOB_COLS: &str = "id, tool, project, project_key, ref, commit_sha, siblings_json, recipe_source, recipe_ref,
  recipe_commit, recipe_sha256, trial, request_json, plan_json, merge_key, merged_into, state, verdict,
  failure_class, reason_code, status_message, summary, warnings_json, result_json, created_by,
  cancel_requested_by, workspace_mode, workspace_slot, fixture_state, created_at, ready_at, started_at,
  finished_at, last_updated_at, expires_at";

fn job_from_row(r: &Row<'_>) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: r.get(0)?,
        tool: r.get(1)?,
        project: r.get(2)?,
        project_key: r.get(3)?,
        ref_: r.get(4)?,
        commit_sha: r.get(5)?,
        siblings_json: r.get(6)?,
        recipe_source: r.get(7)?,
        recipe_ref: r.get(8)?,
        recipe_commit: r.get(9)?,
        recipe_sha256: r.get(10)?,
        trial: r.get::<_, i64>(11)? != 0,
        request_json: r.get(12)?,
        plan_json: r.get(13)?,
        merge_key: r.get(14)?,
        merged_into: r.get(15)?,
        state: opt_state(r, 16)?,
        verdict: r.get(17)?,
        failure_class: r.get(18)?,
        reason_code: r.get(19)?,
        status_message: r.get(20)?,
        summary: r.get(21)?,
        warnings_json: r.get(22)?,
        result_json: r.get(23)?,
        created_by: r.get(24)?,
        cancel_requested_by: r.get(25)?,
        workspace_mode: r.get(26)?,
        workspace_slot: r.get(27)?,
        fixture_state: r.get(28)?,
        created_at: r.get(29)?,
        ready_at: r.get(30)?,
        started_at: r.get(31)?,
        finished_at: r.get(32)?,
        last_updated_at: r.get(33)?,
        expires_at: r.get(34)?,
    })
}

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub task_id: String,
    pub job_id: i64,
    pub requested_by: String,
    pub via_task: bool,
    pub created_at: String,
    pub detached_at: Option<String>,
    pub detached_reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct StepRow {
    pub seq: i64,
    pub step_id: String,
    pub kind: String,
    pub result: String,
    pub failure_class: Option<String>,
    pub reason: Option<String>,
    pub exit_code: Option<i64>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub excerpt: Option<String>,
    pub judge_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ArtifactRow {
    pub job_id: i64,
    pub name: String,
    pub path: String,
    pub bytes: i64,
    pub sha256: String,
    pub inspect_json: Option<String>,
    pub created_at: String,
    pub expires_at: String,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolchainInstall {
    pub kind: String,
    pub version: String,
    pub entry_sha256: String,
    pub root: String,
    pub checksum: Option<String>,
    pub source_ref: Option<String>,
    pub installed_by: String,
    pub installed_at: String,
    pub last_used_at: String,
}

#[derive(Debug, Clone)]
pub struct WarmStateRow {
    pub step_id: String,
    pub last_ok_commit: Option<String>,
    pub inputs_sha256: Option<String>,
    pub toolchains_hash: Option<String>,
    pub def_sha256: Option<String>,
}

impl Store {
    // ── jobs ─────────────────────────────────────────────────────────────

    /// Inserts a job and its first task in one transaction; the task is only returned
    /// to the client after this commits (design §7.5).
    pub fn insert_job_with_task(&self, job: &NewJob, task_id: &str, via_task: bool, client_info: Option<&str>) -> Result<i64> {
        self.tx(|tx| {
            let t = now();
            tx.execute(
                "INSERT INTO jobs (tool, project, project_key, ref, recipe_source, recipe_ref, recipe_commit, recipe_sha256,
                   trial, request_json, state, status_message, created_by, created_at, last_updated_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'intake', ?11, ?12, ?13, ?13, ?14)",
                params![
                    job.tool,
                    job.project,
                    job.project_key,
                    job.ref_,
                    job.recipe_source,
                    job.recipe_ref,
                    job.recipe_commit,
                    job.recipe_sha256,
                    job.trial as i64,
                    job.request_json,
                    job.status_message,
                    job.created_by,
                    t,
                    job.expires_at
                ],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO tasks (task_id, job_id, origin_job_id, requested_by, client_info, via_task, created_at) VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6)",
                params![task_id, id, job.created_by, client_info, via_task as i64, t],
            )?;
            Ok(id)
        })
    }

    pub fn job(&self, id: i64) -> Result<Option<JobRow>> {
        Ok(self
            .conn()
            .query_row(&format!("SELECT {JOB_COLS} FROM jobs WHERE id = ?1"), [id], job_from_row)
            .optional()?)
    }

    pub fn require_job(&self, id: i64) -> Result<JobRow> {
        self.job(id)?.ok_or_else(|| StoreError::NotFound(cimon_core::job_label(id)))
    }

    pub fn jobs_in_states(&self, states: &[JobState]) -> Result<Vec<JobRow>> {
        let list = states.iter().map(|s| format!("'{}'", s.as_str())).collect::<Vec<_>>().join(",");
        let c = self.conn();
        let mut st = c.prepare(&format!("SELECT {JOB_COLS} FROM jobs WHERE state IN ({list}) ORDER BY created_at, id"))?;
        let rows = st.query_map([], job_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn recent_jobs(&self, limit: i64) -> Result<Vec<JobRow>> {
        let c = self.conn();
        let mut st = c.prepare(&format!("SELECT {JOB_COLS} FROM jobs ORDER BY id DESC LIMIT ?1"))?;
        let rows = st.query_map([limit], job_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Oldest job waiting in `state` (requested time order).
    pub fn oldest_in_state(&self, state: JobState) -> Result<Option<JobRow>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {JOB_COLS} FROM jobs WHERE state = ?1 ORDER BY created_at, id LIMIT 1"),
                [state.as_str()],
                job_from_row,
            )
            .optional()?)
    }

    pub fn count_pre_start_by_user(&self, user: &str) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM jobs WHERE created_by = ?1 AND state IN ('intake','provisioning','ready')",
            [user],
            |r| r.get(0),
        )?)
    }

    pub fn set_state(&self, id: i64, state: JobState, status: &str) -> Result<()> {
        let t = now();
        let extra = match state {
            JobState::Ready => ", ready_at = ?4",
            JobState::Running => ", started_at = COALESCE(started_at, ?4)",
            _ => ", last_updated_at = ?4",
        };
        self.conn().execute(
            &format!("UPDATE jobs SET state = ?2, status_message = ?3, last_updated_at = ?4 {extra} WHERE id = ?1"),
            params![id, state.as_str(), status, t],
        )?;
        Ok(())
    }

    pub fn set_status(&self, id: i64, status: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE jobs SET status_message = ?2, last_updated_at = ?3 WHERE id = ?1",
            params![id, status, now()],
        )?;
        Ok(())
    }

    /// Records what intake decided: the pinned commit, siblings, plan and merge key.
    pub fn set_intake(&self, id: i64, commit: &str, siblings_json: &str, plan_json: &str, merge_key: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE jobs SET commit_sha = ?2, siblings_json = ?3, plan_json = ?4, merge_key = ?5, last_updated_at = ?6 WHERE id = ?1",
            params![id, commit, siblings_json, plan_json, merge_key, now()],
        )?;
        Ok(())
    }

    pub fn set_workspace(&self, id: i64, mode: &str, slot: Option<i64>) -> Result<()> {
        self.conn().execute(
            "UPDATE jobs SET workspace_mode = ?2, workspace_slot = ?3, last_updated_at = ?4 WHERE id = ?1",
            params![id, mode, slot, now()],
        )?;
        Ok(())
    }

    pub fn set_fixture_state(&self, id: i64, state: Option<&str>) -> Result<()> {
        self.conn().execute("UPDATE jobs SET fixture_state = ?2 WHERE id = ?1", params![id, state])?;
        Ok(())
    }

    /// Final state with verdict and result. Only applies to a job that is not already
    /// terminal, so a late runner cannot overwrite a cancellation.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_job(
        &self,
        id: i64,
        state: JobState,
        verdict: Option<&str>,
        failure_class: Option<&str>,
        reason_code: Option<&str>,
        status: &str,
        summary: &str,
        warnings_json: &str,
        result_json: &str,
    ) -> Result<bool> {
        let t = now();
        let n = self.conn().execute(
            "UPDATE jobs SET state = ?2, verdict = ?3, failure_class = ?4, reason_code = ?5, status_message = ?6,
               summary = ?7, warnings_json = ?8, result_json = ?9, finished_at = ?10, last_updated_at = ?10
             WHERE id = ?1 AND state NOT IN ('done','merged','cancelled','queue_timeout','interrupted')",
            params![id, state.as_str(), verdict, failure_class, reason_code, status, summary, warnings_json, result_json, t],
        )?;
        Ok(n > 0)
    }

    pub fn request_cancel(&self, id: i64, by: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE jobs SET cancel_requested_by = COALESCE(cancel_requested_by, ?2), last_updated_at = ?3 WHERE id = ?1",
            params![id, by, now()],
        )?;
        Ok(())
    }

    /// A job in `ready`/`running` with the same merge key, other than `id`.
    pub fn merge_target(&self, merge_key: &str, id: i64) -> Result<Option<i64>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id FROM jobs WHERE merge_key = ?1 AND id <> ?2 AND state IN ('ready','running')
                   AND cancel_requested_by IS NULL ORDER BY id LIMIT 1",
                params![merge_key, id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Ends `id` as merged into `into` and moves its tasks there.
    pub fn merge_job(&self, id: i64, into: i64, status: &str) -> Result<()> {
        self.tx(|tx| {
            let t = now();
            tx.execute(
                "UPDATE jobs SET state = 'merged', merged_into = ?2, status_message = ?3, finished_at = ?4, last_updated_at = ?4 WHERE id = ?1",
                params![id, into, status, t],
            )?;
            tx.execute("UPDATE tasks SET job_id = ?2 WHERE job_id = ?1", params![id, into])?;
            Ok(())
        })
    }

    /// Done trial jobs for one project at one recipe commit, newest first.
    pub fn trial_jobs(&self, project: &str, recipe_ref: &str, recipe_commit: &str) -> Result<Vec<JobRow>> {
        let c = self.conn();
        let mut st = c.prepare(&format!(
            "SELECT {JOB_COLS} FROM jobs WHERE trial = 1 AND project = ?1 AND recipe_ref = ?2 AND recipe_commit = ?3
               AND state = 'done' ORDER BY id DESC"
        ))?;
        let rows = st.query_map(params![project, recipe_ref, recipe_commit], job_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every trial job ever run on a branch (any commit), newest first.
    pub fn trial_jobs_for_ref(&self, recipe_ref: &str) -> Result<Vec<JobRow>> {
        let c = self.conn();
        let mut st = c.prepare(&format!("SELECT {JOB_COLS} FROM jobs WHERE trial = 1 AND recipe_ref = ?1 ORDER BY id DESC"))?;
        let rows = st.query_map([recipe_ref], job_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn latest_trial_at(&self, recipe_ref: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT MAX(created_at) FROM jobs WHERE trial = 1 AND recipe_ref = ?1", [recipe_ref], |r| r.get(0))
            .optional()?
            .flatten())
    }

    pub fn expired_jobs(&self, before: &str) -> Result<Vec<i64>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id FROM jobs WHERE expires_at < ?1 AND state IN ('done','merged','cancelled','queue_timeout','interrupted')",
        )?;
        let rows = st.query_map([before], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Deletes a job record and everything hanging off it (retention, design §16.4).
    pub fn delete_job(&self, id: i64) -> Result<()> {
        self.tx(|tx| {
            for table in ["tasks", "steps", "excluded", "artifacts", "job_toolchains"] {
                tx.execute(&format!("DELETE FROM {table} WHERE job_id = ?1"), [id])?;
            }
            tx.execute("UPDATE jobs SET merged_into = NULL WHERE merged_into = ?1", [id])?;
            tx.execute("DELETE FROM jobs WHERE id = ?1", [id])?;
            Ok(())
        })
    }

    // ── tasks ────────────────────────────────────────────────────────────

    pub fn task(&self, task_id: &str) -> Result<Option<TaskRow>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT task_id, job_id, requested_by, via_task, created_at, detached_at, detached_reason FROM tasks WHERE task_id = ?1",
                [task_id],
                |r| {
                    Ok(TaskRow {
                        task_id: r.get(0)?,
                        job_id: r.get(1)?,
                        requested_by: r.get(2)?,
                        via_task: r.get::<_, i64>(3)? != 0,
                        created_at: r.get(4)?,
                        detached_at: r.get(5)?,
                        detached_reason: r.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn touch_task(&self, task_id: &str) -> Result<()> {
        self.conn().execute("UPDATE tasks SET last_polled_at = ?2 WHERE task_id = ?1", params![task_id, now()])?;
        Ok(())
    }

    /// Detaches a task; returns how many tasks remain attached to the job.
    pub fn detach_task(&self, task_id: &str, reason: &str) -> Result<i64> {
        self.tx(|tx| {
            let job: Option<i64> = tx
                .query_row("SELECT job_id FROM tasks WHERE task_id = ?1", [task_id], |r| r.get(0))
                .optional()?;
            let Some(job) = job else {
                return Err(StoreError::NotFound(task_id.to_string()));
            };
            tx.execute(
                "UPDATE tasks SET detached_at = COALESCE(detached_at, ?2), detached_reason = COALESCE(detached_reason, ?3) WHERE task_id = ?1",
                params![task_id, now(), reason],
            )?;
            let remaining: i64 = tx.query_row(
                "SELECT COUNT(*) FROM tasks WHERE job_id = ?1 AND detached_at IS NULL",
                [job],
                |r| r.get(0),
            )?;
            Ok(remaining)
        })
    }

    pub fn tasks_for_job(&self, job: i64) -> Result<Vec<TaskRow>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT task_id, job_id, requested_by, via_task, created_at, detached_at, detached_reason FROM tasks WHERE job_id = ?1",
        )?;
        let rows = st
            .query_map([job], |r| {
                Ok(TaskRow {
                    task_id: r.get(0)?,
                    job_id: r.get(1)?,
                    requested_by: r.get(2)?,
                    via_task: r.get::<_, i64>(3)? != 0,
                    created_at: r.get(4)?,
                    detached_at: r.get(5)?,
                    detached_reason: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The job a task was created for, before a merge moved it. `deduplicated` on a
    /// result means this differs from the job it now follows.
    pub fn task_origin_job(&self, task_id: &str) -> Result<Option<i64>> {
        Ok(self
            .conn()
            .query_row("SELECT origin_job_id FROM tasks WHERE task_id = ?1", [task_id], |r| r.get(0))
            .optional()?)
    }

    // ── steps · excluded ────────────────────────────────────────────────

    pub fn replace_steps(&self, job: i64, steps: &[StepRow]) -> Result<()> {
        self.tx(|tx| {
            tx.execute("DELETE FROM steps WHERE job_id = ?1", [job])?;
            for s in steps {
                tx.execute(
                    "INSERT INTO steps (job_id, seq, step_id, kind, result, failure_class, reason, exit_code, started_at, finished_at, excerpt, judge_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        job,
                        s.seq,
                        s.step_id,
                        s.kind,
                        s.result,
                        s.failure_class,
                        s.reason,
                        s.exit_code,
                        s.started_at,
                        s.finished_at,
                        s.excerpt,
                        s.judge_json
                    ],
                )?;
            }
            Ok(())
        })
    }

    pub fn update_step(&self, job: i64, s: &StepRow) -> Result<()> {
        self.conn().execute(
            "UPDATE steps SET result = ?3, failure_class = ?4, reason = ?5, exit_code = ?6, started_at = ?7, finished_at = ?8,
               excerpt = ?9, judge_json = ?10 WHERE job_id = ?1 AND seq = ?2",
            params![job, s.seq, s.result, s.failure_class, s.reason, s.exit_code, s.started_at, s.finished_at, s.excerpt, s.judge_json],
        )?;
        Ok(())
    }

    pub fn steps(&self, job: i64) -> Result<Vec<StepRow>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT seq, step_id, kind, result, failure_class, reason, exit_code, started_at, finished_at, excerpt, judge_json
             FROM steps WHERE job_id = ?1 ORDER BY seq",
        )?;
        let rows = st
            .query_map([job], |r| {
                Ok(StepRow {
                    seq: r.get(0)?,
                    step_id: r.get(1)?,
                    kind: r.get(2)?,
                    result: r.get(3)?,
                    failure_class: r.get(4)?,
                    reason: r.get(5)?,
                    exit_code: r.get(6)?,
                    started_at: r.get(7)?,
                    finished_at: r.get(8)?,
                    excerpt: r.get(9)?,
                    judge_json: r.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_excluded(&self, job: i64, excluded: &[(String, String)]) -> Result<()> {
        self.tx(|tx| {
            tx.execute("DELETE FROM excluded WHERE job_id = ?1", [job])?;
            for (step, reason) in excluded {
                tx.execute("INSERT INTO excluded (job_id, step_id, reason) VALUES (?1, ?2, ?3)", params![job, step, reason])?;
            }
            Ok(())
        })
    }

    pub fn excluded(&self, job: i64) -> Result<Vec<(String, String)>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT step_id, reason FROM excluded WHERE job_id = ?1 ORDER BY step_id")?;
        let rows = st.query_map([job], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ── artifacts ───────────────────────────────────────────────────────

    pub fn insert_artifact(&self, a: &ArtifactRow) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO artifacts (job_id, name, path, bytes, sha256, inspect_json, created_at, expires_at, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
            params![a.job_id, a.name, a.path, a.bytes, a.sha256, a.inspect_json, a.created_at, a.expires_at],
        )?;
        Ok(())
    }

    pub fn artifacts(&self, job: i64) -> Result<Vec<ArtifactRow>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT job_id, name, path, bytes, sha256, inspect_json, created_at, expires_at, deleted_at FROM artifacts WHERE job_id = ?1 ORDER BY name",
        )?;
        let rows = st.query_map([job], artifact_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn artifact(&self, job: i64, name: &str) -> Result<Option<ArtifactRow>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT job_id, name, path, bytes, sha256, inspect_json, created_at, expires_at, deleted_at FROM artifacts WHERE job_id = ?1 AND name = ?2",
                params![job, name],
                artifact_from_row,
            )
            .optional()?)
    }

    pub fn expired_artifacts(&self, before: &str) -> Result<Vec<ArtifactRow>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT job_id, name, path, bytes, sha256, inspect_json, created_at, expires_at, deleted_at FROM artifacts
             WHERE expires_at < ?1 AND deleted_at IS NULL",
        )?;
        let rows = st.query_map([before], artifact_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn mark_artifact_deleted(&self, job: i64, name: &str) -> Result<()> {
        self.conn()
            .execute("UPDATE artifacts SET deleted_at = ?3 WHERE job_id = ?1 AND name = ?2", params![job, name, now()])?;
        Ok(())
    }

    // ── toolchains ──────────────────────────────────────────────────────

    pub fn upsert_install(&self, i: &ToolchainInstall) -> Result<()> {
        self.conn().execute(
            "INSERT INTO toolchain_installs (kind, version, entry_sha256, root, checksum, source_ref, installed_by, installed_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(kind, version, entry_sha256) DO UPDATE SET root = excluded.root, checksum = excluded.checksum,
               source_ref = excluded.source_ref, installed_by = excluded.installed_by, installed_at = excluded.installed_at,
               last_used_at = excluded.last_used_at",
            params![i.kind, i.version, i.entry_sha256, i.root, i.checksum, i.source_ref, i.installed_by, i.installed_at, i.last_used_at],
        )?;
        Ok(())
    }

    pub fn install(&self, kind: &str, version: &str, entry: &str) -> Result<Option<ToolchainInstall>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT kind, version, entry_sha256, root, checksum, source_ref, installed_by, installed_at, last_used_at
                 FROM toolchain_installs WHERE kind = ?1 AND version = ?2 AND entry_sha256 = ?3",
                params![kind, version, entry],
                install_from_row,
            )
            .optional()?)
    }

    pub fn installs(&self) -> Result<Vec<ToolchainInstall>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT kind, version, entry_sha256, root, checksum, source_ref, installed_by, installed_at, last_used_at
             FROM toolchain_installs ORDER BY kind, version",
        )?;
        let rows = st.query_map([], install_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn touch_install(&self, kind: &str, version: &str, entry: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE toolchain_installs SET last_used_at = ?4 WHERE kind = ?1 AND version = ?2 AND entry_sha256 = ?3",
            params![kind, version, entry, now()],
        )?;
        Ok(())
    }

    pub fn delete_install(&self, kind: &str, version: &str, entry: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM toolchain_installs WHERE kind = ?1 AND version = ?2 AND entry_sha256 = ?3",
            params![kind, version, entry],
        )?;
        Ok(())
    }

    pub fn set_job_toolchain(&self, job: i64, kind: &str, version: &str, entry: &str, probe: Option<&str>) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO job_toolchains (job_id, kind, version, entry_sha256, probe) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![job, kind, version, entry, probe],
        )?;
        Ok(())
    }

    pub fn job_toolchains(&self, job: i64) -> Result<Vec<(String, String, String, Option<String>)>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT kind, version, entry_sha256, probe FROM job_toolchains WHERE job_id = ?1 ORDER BY kind")?;
        let rows = st
            .query_map([job], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Install roots referenced by jobs that have not finished (never collected).
    pub fn installs_in_use(&self) -> Result<Vec<(String, String, String)>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT DISTINCT t.kind, t.version, t.entry_sha256 FROM job_toolchains t JOIN jobs j ON j.id = t.job_id
             WHERE j.state NOT IN ('done','merged','cancelled','queue_timeout','interrupted')",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ── warm slots ──────────────────────────────────────────────────────

    pub fn warm_slots(&self, project: &str) -> Result<Vec<(i64, Option<String>, Option<String>, Option<String>)>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT slot, last_ref, last_commit, last_used_at FROM warm_slots WHERE project = ?1 ORDER BY slot")?;
        let rows = st
            .query_map([project], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn all_warm_slots(&self) -> Result<Vec<(String, i64, Option<String>)>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT project, slot, last_used_at FROM warm_slots ORDER BY last_used_at")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn use_warm_slot(&self, project: &str, slot: i64, ref_: &str, commit: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO warm_slots (project, slot, last_ref, last_commit, last_used_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project, slot) DO UPDATE SET last_ref = excluded.last_ref, last_commit = excluded.last_commit, last_used_at = excluded.last_used_at",
            params![project, slot, ref_, commit, now()],
        )?;
        Ok(())
    }

    pub fn delete_warm_slot(&self, project: &str, slot: i64) -> Result<()> {
        self.tx(|tx| {
            tx.execute("DELETE FROM warm_slots WHERE project = ?1 AND slot = ?2", params![project, slot])?;
            tx.execute("DELETE FROM warm_state WHERE project = ?1 AND slot = ?2", params![project, slot])?;
            Ok(())
        })
    }

    pub fn warm_state(&self, project: &str, slot: i64, step: &str) -> Result<Option<WarmStateRow>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT step_id, last_ok_commit, inputs_sha256, toolchains_hash, def_sha256 FROM warm_state WHERE project = ?1 AND slot = ?2 AND step_id = ?3",
                params![project, slot, step],
                |r| {
                    Ok(WarmStateRow {
                        step_id: r.get(0)?,
                        last_ok_commit: r.get(1)?,
                        inputs_sha256: r.get(2)?,
                        toolchains_hash: r.get(3)?,
                        def_sha256: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_warm_state(&self, project: &str, slot: i64, w: &WarmStateRow) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO warm_state (project, slot, step_id, last_ok_commit, inputs_sha256, toolchains_hash, def_sha256, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![project, slot, w.step_id, w.last_ok_commit, w.inputs_sha256, w.toolchains_hash, w.def_sha256, now()],
        )?;
        Ok(())
    }

    /// Forget every setup success on a slot — after a failed setup, a cancel, a timeout
    /// or an interruption (design §11.3).
    pub fn clear_warm_state(&self, project: &str, slot: i64) -> Result<()> {
        self.conn().execute("DELETE FROM warm_state WHERE project = ?1 AND slot = ?2", params![project, slot])?;
        Ok(())
    }
}

fn artifact_from_row(r: &Row<'_>) -> rusqlite::Result<ArtifactRow> {
    Ok(ArtifactRow {
        job_id: r.get(0)?,
        name: r.get(1)?,
        path: r.get(2)?,
        bytes: r.get(3)?,
        sha256: r.get(4)?,
        inspect_json: r.get(5)?,
        created_at: r.get(6)?,
        expires_at: r.get(7)?,
        deleted_at: r.get(8)?,
    })
}

fn install_from_row(r: &Row<'_>) -> rusqlite::Result<ToolchainInstall> {
    Ok(ToolchainInstall {
        kind: r.get(0)?,
        version: r.get(1)?,
        entry_sha256: r.get(2)?,
        root: r.get(3)?,
        checksum: r.get(4)?,
        source_ref: r.get(5)?,
        installed_by: r.get(6)?,
        installed_at: r.get(7)?,
        last_used_at: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_job(user: &str) -> NewJob {
        NewJob {
            tool: "build".into(),
            project: "example-app".into(),
            project_key: "example-app".into(),
            ref_: Some("main".into()),
            recipe_source: "central".into(),
            recipe_ref: "main".into(),
            recipe_commit: "abc".into(),
            recipe_sha256: "def".into(),
            trial: false,
            request_json: "{}".into(),
            created_by: user.into(),
            status_message: "접수".into(),
            expires_at: cimon_core::after_secs(3600),
        }
    }

    #[test]
    fn job_lifecycle_and_merge() {
        let s = Store::open_in_memory().unwrap();
        let a = s.insert_job_with_task(&new_job("u"), "t1", true, None).unwrap();
        let b = s.insert_job_with_task(&new_job("u"), "t2", true, None).unwrap();
        assert_eq!(s.count_pre_start_by_user("u").unwrap(), 2);
        s.set_intake(a, "c1", "[]", "{}", "k").unwrap();
        s.set_state(a, JobState::Ready, "대기").unwrap();
        s.set_intake(b, "c1", "[]", "{}", "k").unwrap();
        assert_eq!(s.merge_target("k", b).unwrap(), Some(a));
        s.merge_job(b, a, "merged").unwrap();
        assert_eq!(s.tasks_for_job(a).unwrap().len(), 2);
        assert_eq!(s.require_job(b).unwrap().state, JobState::Merged);
        // one task detaches, one remains
        assert_eq!(s.detach_task("t1", "cancel").unwrap(), 1);
        assert_eq!(s.detach_task("t2", "cancel").unwrap(), 0);
        assert!(s.finish_job(a, JobState::Done, Some("pass"), None, None, "", "ok", "[]", "{}").unwrap());
        // a second finish does not overwrite
        assert!(!s.finish_job(a, JobState::Cancelled, None, None, None, "", "", "[]", "{}").unwrap());
        assert_eq!(s.require_job(a).unwrap().verdict.as_deref(), Some("pass"));
    }

    #[test]
    fn warm_state_cleared() {
        let s = Store::open_in_memory().unwrap();
        s.set_warm_state(
            "p",
            1,
            &WarmStateRow { step_id: "setup".into(), last_ok_commit: Some("c".into()), inputs_sha256: None, toolchains_hash: None, def_sha256: None },
        )
        .unwrap();
        assert!(s.warm_state("p", 1, "setup").unwrap().is_some());
        s.clear_warm_state("p", 1).unwrap();
        assert!(s.warm_state("p", 1, "setup").unwrap().is_none());
    }
}
