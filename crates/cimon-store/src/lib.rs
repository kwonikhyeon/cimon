//! SQLite job store (design §13). One connection behind a mutex: the service writes a
//! few rows per second at most, and every statement is short. WAL with
//! `synchronous=FULL` so a committed task survives a crash (design §7.5).

use cimon_core::{now, AuthUser, JobState, Role};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub mod jobs;
pub mod misc;
pub mod proposals;

pub use jobs::*;
pub use misc::*;
pub use proposals::*;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{0}")]
    Conflict(String),
    #[error("not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    conn: Mutex<Connection>,
    /// The database file (None in memory) — backups open their own connection so a
    /// long `VACUUM INTO` does not hold every request behind the mutex.
    path: Option<std::path::PathBuf>,
}

const SCHEMA: &str = include_str!("schema.sql");
const SCHEMA_VERSION: i64 = 1;

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        Self::init(conn, Some(path.to_path_buf()))
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(conn: Connection, path: Option<std::path::PathBuf>) -> Result<Store> {
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (v INTEGER NOT NULL)")?;
        let v: Option<i64> = conn.query_row("SELECT v FROM schema_version", [], |r| r.get(0)).optional()?;
        match v {
            None => {
                conn.execute("INSERT INTO schema_version (v) VALUES (?1)", [SCHEMA_VERSION])?;
            }
            Some(v) if v > SCHEMA_VERSION => {
                return Err(StoreError::Conflict(format!(
                    "job store schema {v} is newer than this cimon ({SCHEMA_VERSION}) — upgrade cimon"
                )))
            }
            Some(_) => {}
        }
        Ok(Store { conn: Mutex::new(conn), path })
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Runs `f` inside one transaction.
    pub fn tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// `VACUUM INTO` — a consistent copy even in WAL mode (design §16.5).
    pub fn vacuum_into(&self, target: &Path) -> Result<()> {
        let _ = std::fs::remove_file(target);
        match &self.path {
            Some(p) => {
                let c = Connection::open(p)?;
                c.busy_timeout(std::time::Duration::from_secs(30))?;
                c.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])?;
            }
            None => {
                self.conn().execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])?;
            }
        }
        Ok(())
    }

    // ── users ────────────────────────────────────────────────────────────

    /// Creates a user and returns the plaintext token (shown once).
    pub fn create_user(&self, name: &str, role: Role) -> Result<String> {
        validate_user_name(name)?;
        let token = cimon_core::random_secret_hex();
        let hash = cimon_core::sha256_hex(token.as_bytes());
        let n = self.conn().execute(
            "INSERT INTO users (name, token_sha256, role, created_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(name) DO NOTHING",
            params![name, hash, role.as_str(), now()],
        )?;
        if n == 0 {
            return Err(StoreError::Conflict(format!("user {name:?} already exists — use `cimon token rotate`")));
        }
        Ok(token)
    }

    /// New token for an existing user; re-enables a disabled user.
    pub fn rotate_token(&self, name: &str) -> Result<String> {
        let token = cimon_core::random_secret_hex();
        let hash = cimon_core::sha256_hex(token.as_bytes());
        let n = self
            .conn()
            .execute("UPDATE users SET token_sha256 = ?2, disabled_at = NULL WHERE name = ?1", params![name, hash])?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("user {name:?}")));
        }
        self.revoke_web_sessions(name)?;
        Ok(token)
    }

    pub fn disable_user(&self, name: &str) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE users SET disabled_at = ?2 WHERE name = ?1 AND disabled_at IS NULL",
            params![name, now()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("active user {name:?}")));
        }
        self.revoke_web_sessions(name)?;
        Ok(())
    }

    pub fn set_role(&self, name: &str, role: Role) -> Result<()> {
        let n = self.conn().execute("UPDATE users SET role = ?2 WHERE name = ?1", params![name, role.as_str()])?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("user {name:?}")));
        }
        Ok(())
    }

    /// Looks up an active user by plaintext token.
    pub fn authenticate(&self, token: &str) -> Result<Option<AuthUser>> {
        let hash = cimon_core::sha256_hex(token.as_bytes());
        let row = self
            .conn()
            .query_row(
                "SELECT name, role FROM users WHERE token_sha256 = ?1 AND disabled_at IS NULL",
                [hash],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(row.map(|(name, role)| AuthUser { name, role: Role::parse(&role).unwrap_or(Role::Member) }))
    }

    pub fn user(&self, name: &str) -> Result<Option<AuthUser>> {
        let row = self
            .conn()
            .query_row("SELECT name, role FROM users WHERE name = ?1 AND disabled_at IS NULL", [name], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?;
        Ok(row.map(|(name, role)| AuthUser { name, role: Role::parse(&role).unwrap_or(Role::Member) }))
    }

    pub fn list_users(&self) -> Result<Vec<(String, String, Option<String>)>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT name, role, disabled_at FROM users ORDER BY name")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn count_admins(&self) -> Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled_at IS NULL", [], |r| r.get(0))?)
    }

    // ── audit ────────────────────────────────────────────────────────────

    pub fn audit(&self, entry: AuditEntry<'_>) -> Result<()> {
        self.conn().execute(
            "INSERT INTO audit (at, user, action, job_id, proposal_id, recipe_commit, tailnet_login, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                now(),
                entry.user,
                entry.action,
                entry.job_id,
                entry.proposal_id,
                entry.recipe_commit,
                entry.tailnet_login,
                entry.detail
            ],
        )?;
        Ok(())
    }

    pub fn audit_since(&self, since: &str) -> Result<Vec<(String, String, String, Option<String>)>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT at, user, action, detail FROM audit WHERE at >= ?1 ORDER BY id")?;
        let rows = st
            .query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[derive(Debug, Default, Clone)]
pub struct AuditEntry<'a> {
    pub user: &'a str,
    pub action: &'a str,
    pub job_id: Option<i64>,
    pub proposal_id: Option<i64>,
    pub recipe_commit: Option<&'a str>,
    pub tailnet_login: Option<&'a str>,
    pub detail: Option<String>,
}

fn validate_user_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Err(StoreError::Conflict(format!("user name {name:?} must be 1-64 chars of [A-Za-z0-9._-]")));
    }
    Ok(())
}

pub(crate) fn opt_state(r: &Row<'_>, idx: usize) -> rusqlite::Result<JobState> {
    let s: String = r.get(idx)?;
    JobState::parse(&s).ok_or_else(|| rusqlite::Error::InvalidColumnType(idx, s, rusqlite::types::Type::Text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn users_and_tokens() {
        let s = Store::open_in_memory().unwrap();
        let t = s.create_user("admin-a", Role::Admin).unwrap();
        assert!(s.create_user("admin-a", Role::Admin).is_err());
        let u = s.authenticate(&t).unwrap().unwrap();
        assert_eq!(u.name, "admin-a");
        assert!(u.is_admin());
        assert!(s.authenticate("nope").unwrap().is_none());
        s.disable_user("admin-a").unwrap();
        assert!(s.authenticate(&t).unwrap().is_none());
        let t2 = s.rotate_token("admin-a").unwrap();
        assert!(s.authenticate(&t2).unwrap().is_some());
        assert!(s.authenticate(&t).unwrap().is_none());
        assert!(s.create_user("bad name", Role::Member).is_err());
    }

    #[test]
    fn file_store_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state").join("cimon.db");
        {
            let s = Store::open(&p).unwrap();
            s.create_user("m", Role::Member).unwrap();
        }
        let s = Store::open(&p).unwrap();
        assert_eq!(s.list_users().unwrap().len(), 1);
        let copy = dir.path().join("copy.db");
        s.vacuum_into(&copy).unwrap();
        assert!(copy.exists());
    }
}
