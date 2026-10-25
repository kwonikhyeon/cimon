//! The janitor (design §16.4): retention, disk floor, start-up leftovers, mirror GC,
//! daily backups. It holds the runner lock while it deletes, so it never removes a
//! running job's directories.

use crate::App;
use cimon_core::{after_secs, job_label, now, JobState};
use std::sync::Arc;
use std::time::Duration;

/// Retention table — design §16.4 is the source; keep the two in step.
pub mod retention {
    pub const JOB_DAYS: i64 = 30;
    pub const RECIPE_CHECKOUT_DAYS: i64 = 7;
    pub const CLOSED_PROPOSAL_DAYS: i64 = 30;
    pub const IDLE_PROPOSAL_DAYS: i64 = 30;
    pub const TOOLCHAIN_DAYS: i64 = 30;
    pub const TRIAL_PROJECT_DAYS: i64 = 30;
    pub const SERVICE_LOG_DAYS: i64 = 30;
    pub const AUDIT_DAYS: i64 = 365;
    pub const WEB_GRACE_DAYS: i64 = 1;
}

/// Start-up cleanup: per-job temp/home dirs of finished jobs.
pub fn startup(app: &App) {
    let live: std::collections::BTreeSet<i64> = app
        .store
        .jobs_in_states(&[JobState::Intake, JobState::Provisioning, JobState::Ready, JobState::Running, JobState::Collecting, JobState::Cancelling])
        .unwrap_or_default()
        .into_iter()
        .map(|j| j.id)
        .collect();
    for dir in [app.layout.root.join("tmp"), app.layout.root.join("homes")] {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(id) = name.strip_prefix('j').and_then(|n| n.parse::<i64>().ok()) {
                    if !live.contains(&id) {
                        let _ = cimon_workspace::remove_dir_all_robust(&e.path());
                    }
                }
            }
        }
    }
    // clean clones (ws/<project>/j<id>) and isolated sibling trees
    // (ws/_sibling/<project>/j<id>) of finished jobs
    let sibling_root = app.layout.ws().join("_sibling");
    let project_dirs = std::fs::read_dir(app.layout.ws()).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| *p != sibling_root);
    let sibling_dirs = std::fs::read_dir(&sibling_root).into_iter().flatten().flatten().map(|e| e.path());
    {
        for p in project_dirs.chain(sibling_dirs) {
            if let Ok(inner) = std::fs::read_dir(&p) {
                for e in inner.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if let Some(id) = name.strip_prefix('j').and_then(|n| n.parse::<i64>().ok()) {
                        if !live.contains(&id) {
                            let _ = cimon_workspace::remove_dir_all_robust(&e.path());
                        }
                    }
                }
            }
        }
    }
}

/// Every minute: queue timeouts; every hour: retention; daily: backups and mirror GC.
/// A disk-floor wake-up runs `free_space` as soon as the runner lock is free. Work that
/// needs the runner lock goes to its own task, so a long job never holds up the
/// minute tick (queue timeouts).
pub async fn ticker(app: Arc<App>) {
    let mut last_hourly = std::time::Instant::now() - Duration::from_secs(3600);
    let busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    loop {
        let mut disk = false;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            _ = app.engine.janitor.notified() => disk = true,
            _ = app.engine.shutdown.cancelled() => return,
        }
        let a = app.clone();
        let _ = tokio::task::spawn_blocking(move || crate::jobs::tick(&a)).await;
        let hourly_due = last_hourly.elapsed() >= Duration::from_secs(3600);
        if (hourly_due || disk) && !busy.swap(true, std::sync::atomic::Ordering::SeqCst) {
            if hourly_due {
                last_hourly = std::time::Instant::now();
            }
            let a = app.clone();
            let busy = busy.clone();
            tokio::spawn(async move {
                let _guard = a.engine.runner_lock.lock().await;
                let b = a.clone();
                let res = tokio::task::spawn_blocking(move || {
                    if hourly_due {
                        hourly(&b);
                    }
                    if disk {
                        free_space(&b);
                    }
                })
                .await;
                if let Err(e) = res {
                    tracing::error!("janitor pass panicked: {e}");
                }
                busy.store(false, std::sync::atomic::Ordering::SeqCst);
            });
        }
    }
}

fn older_than_days(t: &str, days: i64) -> bool {
    cimon_core::parse_time(t).is_some_and(|t| (chrono::Utc::now() - t).num_days() >= days)
}

/// Retention pass. Called with the runner lock held.
pub fn hourly(app: &App) {
    retention_pass(app);
    daily_backups(app);
}

pub fn retention_pass(app: &App) {
    use retention::*;
    let t = now();
    // artifacts
    for a in app.store.expired_artifacts(&t).unwrap_or_default() {
        let _ = std::fs::remove_file(&a.path);
        let _ = app.store.mark_artifact_deleted(a.job_id, &a.name);
    }
    // job records, logs and remaining artifact dirs
    for id in app.store.expired_jobs(&t).unwrap_or_default() {
        let _ = cimon_workspace::remove_dir_all_robust(&app.layout.job_logs(id));
        let _ = cimon_workspace::remove_dir_all_robust(&app.layout.job_artifacts(id));
        let _ = app.store.delete_job(id);
    }
    // recipe checkouts not used by an unfinished job
    let live_commits: std::collections::BTreeSet<String> = app
        .store
        .jobs_in_states(&[JobState::Intake, JobState::Provisioning, JobState::Ready, JobState::Running, JobState::Collecting, JobState::Cancelling])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|j| j.recipe_commit)
        .collect();
    if let Ok(rd) = std::fs::read_dir(app.layout.recipe_checkouts()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if live_commits.contains(&name) {
                continue;
            }
            let old = e.metadata().ok().and_then(|m| m.modified().ok()).is_some_and(|m| m.elapsed().map(|d| d.as_secs() as i64 >= RECIPE_CHECKOUT_DAYS * 86400).unwrap_or(false));
            if old {
                let _ = cimon_workspace::remove_dir_all_robust(&e.path());
            }
        }
    }
    // proposals: close idle, drop closed refs after 30 days
    for p in app.store.proposals(Some("open"), 10_000).unwrap_or_default() {
        let last_trial = app.store.latest_trial_at(&p.branch).ok().flatten();
        let last = [Some(p.updated_at.clone()), last_trial].into_iter().flatten().max().unwrap_or_default();
        if older_than_days(&last, IDLE_PROPOSAL_DAYS) {
            if app.recipes.close(&app.store, "cimon(janitor)", p.id, Some("30일 동안 커밋·시도 실행 없음 — 자동으로 닫음")).is_ok() {
                let _ = app.store.add_comment(p.id, "cimon", "30일 동안 커밋도 시도 실행도 없어 자동으로 닫았다. 같은 브랜치 이름으로 다시 고치면 새 제안이 열린다.", &app.recipes.branch_head(&p.branch).unwrap_or_default());
            }
        }
    }
    for p in app.store.closed_before(&after_secs(-CLOSED_PROPOSAL_DAYS * 86400)).unwrap_or_default() {
        let _ = app.recipes.drop_closed(&cimon_core::proposal_label(p.id));
    }
    // toolchains unused for 30 days and not used by an unfinished job (job_toolchains
    // records the install's version key, the same key installs are stored under)
    let in_use = app.store.installs_in_use().unwrap_or_default();
    for i in app.store.installs().unwrap_or_default() {
        if older_than_days(&i.last_used_at, TOOLCHAIN_DAYS) && !in_use.iter().any(|(k, v, e)| *k == i.kind && *v == i.version && *e == i.entry_sha256) {
            if !i.root.contains("_bootstrap") {
                let _ = cimon_workspace::remove_dir_all_robust(std::path::Path::new(&i.root));
            }
            let _ = app.store.delete_install(&i.kind, &i.version, &i.entry_sha256);
        }
    }
    // service logs, audit, web sessions
    if let Ok(rd) = std::fs::read_dir(app.layout.service_logs()) {
        for e in rd.flatten() {
            let old = e.metadata().ok().and_then(|m| m.modified().ok()).is_some_and(|m| m.elapsed().map(|d| d.as_secs() as i64 >= SERVICE_LOG_DAYS * 86400).unwrap_or(false));
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = app.store.purge_audit(&after_secs(-AUDIT_DAYS * 86400));
    let _ = app.store.purge_web(&after_secs(-WEB_GRACE_DAYS * 86400));
    trial_project_leftovers(app);
    cache_limits(app);
}

/// Mirrors and work trees of trial-only projects (`<id>@<hash>`) no open proposal
/// points at any more.
fn trial_project_leftovers(app: &App) {
    let open: Vec<String> = app.store.proposals(Some("open"), 10_000).unwrap_or_default().into_iter().map(|p| p.branch).collect();
    let mut referenced = std::collections::BTreeSet::new();
    for b in &open {
        for j in app.store.trial_jobs_for_ref(b).unwrap_or_default() {
            referenced.insert(j.project_key.clone());
        }
    }
    for dir in [app.layout.mirrors(), app.layout.ws()] {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().trim_end_matches(".git").to_string();
                if name.contains('@') && !referenced.contains(&name) {
                    let old = e.metadata().ok().and_then(|m| m.modified().ok()).is_some_and(|m| m.elapsed().map(|d| d.as_secs() as i64 >= retention::TRIAL_PROJECT_DAYS * 86400).unwrap_or(false));
                    if old {
                        let _ = cimon_workspace::remove_dir_all_robust(&e.path());
                    }
                }
            }
        }
    }
}

/// Keeps each project's package cache under `[disk].cache_max_gb_per_project`,
/// deleting the least recently modified top-level entries first.
fn cache_limits(app: &App) {
    let limit = app.config.disk.cache_max_gb_per_project.saturating_mul(1 << 30);
    let Ok(rd) = std::fs::read_dir(app.layout.root.join("caches")) else { return };
    for project in rd.flatten() {
        let mut entries: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = vec![];
        let mut total = 0u64;
        for sub in std::fs::read_dir(project.path()).into_iter().flatten().flatten() {
            for e in std::fs::read_dir(sub.path()).into_iter().flatten().flatten() {
                let size = dir_size(&e.path());
                let m = e.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
                total += size;
                entries.push((m, size, e.path()));
            }
        }
        if total <= limit {
            continue;
        }
        entries.sort();
        for (_, size, p) in entries {
            if total <= limit {
                break;
            }
            if cimon_workspace::remove_dir_all_robust(&p).is_ok() || std::fs::remove_file(&p).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
}

fn dir_size(p: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(p).into_iter().flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()
}

fn daily_backups(app: &App) {
    let due = |kind: &str| app.store.last_backup(kind).ok().flatten().is_none_or(|t| older_than_days(&t, 1));
    if app.recipes.main_commit().is_some() && due("recipes") {
        if let Err(e) = crate::backup::backup_recipes(app, "daily") {
            tracing::error!("daily recipe backup: {e}");
        }
    }
    if due("db") {
        if let Err(e) = crate::backup::backup_db(app) {
            tracing::error!("daily db backup: {e}");
        }
    }
    // mirror GC — the caller holds the runner lock, so no job borrows objects right now;
    // the mirror lock keeps intake's fetch/resolve/keep out while it runs
    if let Ok(rd) = std::fs::read_dir(app.layout.mirrors()) {
        for e in rd.flatten() {
            let key = e.file_name().to_string_lossy().trim_end_matches(".git").to_string();
            let lock = app.engine.mirror_lock(&key);
            let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(m) = cimon_workspace::Mirror::open(&e.path()) {
                let stamp = e.path().join("cimon-last-gc");
                let fresh = std::fs::metadata(&stamp).ok().and_then(|m| m.modified().ok()).is_some_and(|m| m.elapsed().map(|d| d.as_secs() < 86400).unwrap_or(true));
                if !fresh {
                    if let Err(err) = m.gc() {
                        tracing::warn!("mirror gc {}: {err}", e.path().display());
                    }
                    let _ = std::fs::write(&stamp, now());
                }
            }
        }
    }
}

/// Disk floor (design §16.4): run retention, then drop least recently used warm slots
/// until the floor is met. Runs from the ticker with the runner lock held, after a call
/// was refused with `disk_low`.
pub fn free_space(app: &App) {
    retention_pass(app);
    let min = app.config.disk.min_free_gb.saturating_mul(1 << 30);
    let busy: std::collections::BTreeSet<(String, i64)> = app
        .store
        .jobs_in_states(&[JobState::Running, JobState::Collecting, JobState::Cancelling])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|j| j.workspace_slot.map(|s| (j.project_key, s)))
        .collect();
    for (project, slot, _) in app.store.all_warm_slots().unwrap_or_default() {
        let lock = app.engine.mirror_lock(&project);
        let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
        if cimon_runner::free_bytes(&app.layout.root).unwrap_or(u64::MAX) >= min {
            break;
        }
        if busy.contains(&(project.clone(), slot)) {
            continue;
        }
        let _ = cimon_workspace::remove_dir_all_robust(&app.layout.warm_slot(&project, slot as u32));
        let _ = app.store.delete_warm_slot(&project, slot);
        if let Some(m) = cimon_workspace::Mirror::open(&app.layout.mirror(&project)) {
            m.unkeep(&format!("slot-{slot}"));
        }
        tracing::warn!("disk floor: removed warm slot {project}/{slot}");
    }
    let _ = job_label(0);
}
