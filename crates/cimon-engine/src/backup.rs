//! Backups (design §16.5). The recipe repository has no other copy, so every promote,
//! revert and `import --init` writes a bundle, and a stale offsite copy is reported on
//! every `recipe_*` result.

use crate::App;
use cimon_core::now;
use cimon_store::BackupRow;
use std::path::{Path, PathBuf};

/// Hours after which a missing fresh offsite copy turns into a warning.
pub const OFFSITE_STALE_HOURS: i64 = 26;

/// Bundles the recipe repository and copies it offsite. Failures are recorded and
/// returned, never swallowed.
pub fn backup_recipes(app: &App, why: &str) -> Result<PathBuf, String> {
    if app.recipes.main_commit().is_none() {
        return Err("recipe repository is empty — nothing to back up".into());
    }
    let at = now();
    let file = app.layout.backup().join(format!("recipes-{}.bundle", stamp(&at)));
    let mut row = BackupRow { kind: "recipes".into(), at: at.clone(), path: file.to_string_lossy().into(), offsite_ok_at: None, error: None };
    if let Err(e) = app.recipes.bundle(&file) {
        row.error = Some(format!("bundle failed ({why}): {e}"));
        let _ = app.store.record_backup(&row);
        return Err(row.error.unwrap());
    }
    let res = copy_offsite(app, &file);
    match &res {
        Ok(Some(_)) => row.offsite_ok_at = Some(now()),
        Ok(None) => {}
        Err(e) => row.error = Some(format!("offsite copy failed ({why}): {e}")),
    }
    let _ = app.store.record_backup(&row);
    prune(app, "recipes");
    match res {
        Err(e) => Err(format!("bundle written to {} but offsite copy failed: {e}", file.display())),
        Ok(_) => Ok(file),
    }
}

/// `VACUUM INTO` copy of the job store, then offsite.
pub fn backup_db(app: &App) -> Result<PathBuf, String> {
    let at = now();
    let file = app.layout.backup().join(format!("cimon-{}.db", stamp(&at)));
    let _ = std::fs::create_dir_all(app.layout.backup());
    let mut row = BackupRow { kind: "db".into(), at: at.clone(), path: file.to_string_lossy().into(), offsite_ok_at: None, error: None };
    if let Err(e) = app.store.vacuum_into(&file) {
        row.error = Some(format!("VACUUM INTO failed: {e}"));
        let _ = app.store.record_backup(&row);
        return Err(row.error.unwrap());
    }
    let res = copy_offsite(app, &file);
    match &res {
        Ok(Some(_)) => row.offsite_ok_at = Some(now()),
        Ok(None) => {}
        Err(e) => row.error = Some(format!("offsite copy failed: {e}")),
    }
    let _ = app.store.record_backup(&row);
    prune(app, "db");
    res.map(|_| file)
}

fn copy_offsite(app: &App, file: &Path) -> Result<Option<PathBuf>, String> {
    let Some(dir) = app.config.backup.offsite_dir() else { return Ok(None) };
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dest = dir.join(file.file_name().unwrap());
    let tmp = dest.with_extension("partial");
    std::fs::copy(file, &tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    Ok(Some(dest))
}

/// Keeps the newest `[backup].keep` local files of `kind`.
fn prune(app: &App, kind: &str) {
    let keep = app.config.backup.keep.max(1) as usize;
    if let Ok(rows) = app.store.backups(kind) {
        for row in rows.iter().skip(keep) {
            let _ = std::fs::remove_file(&row.path);
            let _ = app.store.delete_backup_row(kind, &row.at);
        }
    }
}

fn stamp(t: &str) -> String {
    t.replace([':', '-'], "").replace('T', "-").trim_end_matches('Z').to_string()
}

/// Warnings to attach to `recipe_*` results and `/healthz`.
pub fn warnings(app: &App) -> Vec<String> {
    let mut w = vec![];
    if let Some(s) = app.config.backup_setting_warning() {
        w.push(s);
    } else if app.recipes.main_commit().is_some() {
        match app.store.last_offsite("recipes").ok().flatten() {
            None => w.push("레시피 저장소의 외부 사본이 아직 없다 — 백업이 offsite 로 복사되지 않았다".into()),
            Some(t) => {
                if let Some(at) = cimon_core::parse_time(&t) {
                    let age = chrono::Utc::now() - at;
                    if age.num_hours() >= OFFSITE_STALE_HOURS {
                        w.push(format!("레시피 저장소의 외부 사본이 {}시간 전 것이다 — 백업을 확인하라", age.num_hours()));
                    }
                }
            }
        }
    }
    if let Ok(rows) = app.store.backups("recipes") {
        if let Some(latest) = rows.first() {
            if let Some(e) = &latest.error {
                w.push(format!("마지막 레시피 백업이 실패했다: {e}"));
            }
        }
    }
    w
}
