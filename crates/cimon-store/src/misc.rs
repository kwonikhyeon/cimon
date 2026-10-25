//! backups · web sessions · login codes.

use crate::{Result, Store};
use cimon_core::now;
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone)]
pub struct BackupRow {
    pub kind: String,
    pub at: String,
    pub path: String,
    pub offsite_ok_at: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WebSession {
    pub user: String,
    pub csrf: String,
    pub expires_at: String,
}

impl Store {
    pub fn record_backup(&self, b: &BackupRow) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO backups (kind, at, path, offsite_ok_at, error) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![b.kind, b.at, b.path, b.offsite_ok_at, b.error],
        )?;
        Ok(())
    }

    pub fn backups(&self, kind: &str) -> Result<Vec<BackupRow>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT kind, at, path, offsite_ok_at, error FROM backups WHERE kind = ?1 ORDER BY at DESC")?;
        let rows = st
            .query_map([kind], |r| {
                Ok(BackupRow { kind: r.get(0)?, at: r.get(1)?, path: r.get(2)?, offsite_ok_at: r.get(3)?, error: r.get(4)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Time of the newest backup of `kind` that reached the offsite copy.
    pub fn last_offsite(&self, kind: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT MAX(offsite_ok_at) FROM backups WHERE kind = ?1", [kind], |r| r.get(0))
            .optional()?
            .flatten())
    }

    pub fn last_backup(&self, kind: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT MAX(at) FROM backups WHERE kind = ?1", [kind], |r| r.get(0))
            .optional()?
            .flatten())
    }

    pub fn delete_backup_row(&self, kind: &str, at: &str) -> Result<()> {
        self.conn().execute("DELETE FROM backups WHERE kind = ?1 AND at = ?2", params![kind, at])?;
        Ok(())
    }

    // ── web ─────────────────────────────────────────────────────────────

    pub fn create_login_code(&self, user: &str, minutes: u32) -> Result<String> {
        let code = cimon_core::random_secret_hex();
        self.conn().execute(
            "INSERT INTO login_codes (code_sha256, user, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![cimon_core::sha256_hex(code.as_bytes()), user, now(), cimon_core::after_secs(minutes as i64 * 60)],
        )?;
        Ok(code)
    }

    /// Consumes a login code; returns the user if it was valid, unused and unexpired.
    pub fn consume_login_code(&self, code: &str) -> Result<Option<String>> {
        let hash = cimon_core::sha256_hex(code.as_bytes());
        self.tx(|tx| {
            let row: Option<(String, String, Option<String>)> = tx
                .query_row("SELECT user, expires_at, used_at FROM login_codes WHERE code_sha256 = ?1", [&hash], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?;
            let Some((user, expires, used)) = row else { return Ok(None) };
            if used.is_some() || expires < now() {
                return Ok(None);
            }
            tx.execute("UPDATE login_codes SET used_at = ?2 WHERE code_sha256 = ?1", params![hash, now()])?;
            Ok(Some(user))
        })
    }

    /// Creates a session; returns (session id, csrf token). Only the session id hash is stored.
    pub fn create_web_session(&self, user: &str, hours: u32) -> Result<(String, String)> {
        let sid = cimon_core::random_secret_hex();
        let csrf = cimon_core::random_secret_hex();
        self.conn().execute(
            "INSERT INTO web_sessions (id_sha256, user, csrf, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![cimon_core::sha256_hex(sid.as_bytes()), user, csrf, now(), cimon_core::after_secs(hours as i64 * 3600)],
        )?;
        Ok((sid, csrf))
    }

    pub fn web_session(&self, sid: &str) -> Result<Option<WebSession>> {
        let hash = cimon_core::sha256_hex(sid.as_bytes());
        let row: Option<WebSession> = self
            .conn()
            .query_row(
                "SELECT s.user, s.csrf, s.expires_at FROM web_sessions s JOIN users u ON u.name = s.user
                 WHERE s.id_sha256 = ?1 AND s.revoked_at IS NULL AND u.disabled_at IS NULL",
                [&hash],
                |r| Ok(WebSession { user: r.get(0)?, csrf: r.get(1)?, expires_at: r.get(2)? }),
            )
            .optional()?;
        match row {
            Some(s) if s.expires_at > now() => {
                self.conn().execute("UPDATE web_sessions SET last_seen_at = ?2 WHERE id_sha256 = ?1", params![hash, now()])?;
                Ok(Some(s))
            }
            _ => Ok(None),
        }
    }

    pub fn revoke_web_session(&self, sid: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE web_sessions SET revoked_at = ?2 WHERE id_sha256 = ?1",
            params![cimon_core::sha256_hex(sid.as_bytes()), now()],
        )?;
        Ok(())
    }

    pub fn revoke_web_sessions(&self, user: &str) -> Result<()> {
        self.conn()
            .execute("UPDATE web_sessions SET revoked_at = ?2 WHERE user = ?1 AND revoked_at IS NULL", params![user, now()])?;
        Ok(())
    }

    /// Deletes sessions and login codes that expired before `before`.
    pub fn purge_web(&self, before: &str) -> Result<usize> {
        let c = self.conn();
        let a = c.execute("DELETE FROM web_sessions WHERE expires_at < ?1", [before])?;
        let b = c.execute("DELETE FROM login_codes WHERE expires_at < ?1", [before])?;
        Ok(a + b)
    }

    pub fn purge_audit(&self, before: &str) -> Result<usize> {
        Ok(self.conn().execute("DELETE FROM audit WHERE at < ?1", [before])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cimon_core::Role;

    #[test]
    fn login_code_is_single_use() {
        let s = Store::open_in_memory().unwrap();
        s.create_user("u", Role::Member).unwrap();
        let code = s.create_login_code("u", 5).unwrap();
        assert_eq!(s.consume_login_code(&code).unwrap().as_deref(), Some("u"));
        assert_eq!(s.consume_login_code(&code).unwrap(), None);
        let (sid, _csrf) = s.create_web_session("u", 1).unwrap();
        assert!(s.web_session(&sid).unwrap().is_some());
        s.disable_user("u").unwrap();
        assert!(s.web_session(&sid).unwrap().is_none());
    }
}
