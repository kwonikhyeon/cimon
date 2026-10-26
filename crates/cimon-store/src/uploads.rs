//! Uploads (design §11.7): what a caller sent from their own machine for one job. The
//! row holds the fingerprint; the bytes are a DPAPI-protected file under
//! `state\uploads\` that the engine writes and deletes.

use crate::{Result, Store, StoreError};
use cimon_core::now;
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone)]
pub struct UploadRow {
    pub handle: String,
    pub user: String,
    pub label: String,
    pub kind: String,
    pub bytes: i64,
    pub sha256: String,
    pub created_at: String,
    pub expires_at: String,
    pub job_id: Option<i64>,
    /// The recipe's upload name it was handed over as.
    pub name: Option<String>,
    pub deleted_at: Option<String>,
}

const COLS: &str = "handle, user, label, kind, bytes, sha256, created_at, expires_at, job_id, name, deleted_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<UploadRow> {
    Ok(UploadRow {
        handle: r.get(0)?,
        user: r.get(1)?,
        label: r.get(2)?,
        kind: r.get(3)?,
        bytes: r.get(4)?,
        sha256: r.get(5)?,
        created_at: r.get(6)?,
        expires_at: r.get(7)?,
        job_id: r.get(8)?,
        name: r.get(9)?,
        deleted_at: r.get(10)?,
    })
}

impl Store {
    #[allow(clippy::too_many_arguments)]
    pub fn create_upload(&self, handle: &str, user: &str, label: &str, kind: &str, bytes: i64, sha256: &str, expires_at: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO uploads (handle, user, label, kind, bytes, sha256, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![handle, user, label, kind, bytes, sha256, now(), expires_at],
        )?;
        Ok(())
    }

    /// Uploads of `user` no job has taken yet and that have not expired.
    pub fn pending_uploads(&self, user: &str) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM uploads WHERE user = ?1 AND job_id IS NULL AND deleted_at IS NULL AND expires_at > ?2",
            params![user, now()],
            |r| r.get(0),
        )?)
    }

    pub fn upload(&self, handle: &str) -> Result<Option<UploadRow>> {
        Ok(self.conn().query_row(&format!("SELECT {COLS} FROM uploads WHERE handle = ?1"), [handle], row).optional()?)
    }

    pub fn job_uploads(&self, job_id: i64) -> Result<Vec<UploadRow>> {
        let c = self.conn();
        let mut st = c.prepare(&format!("SELECT {COLS} FROM uploads WHERE job_id = ?1 ORDER BY name"))?;
        let v = st.query_map([job_id], row)?.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Hands `uploads` (recipe name, handle) to a new job in the transaction that
    /// creates it: each must still be the caller's, untaken and unexpired.
    pub(crate) fn bind_uploads(tx: &rusqlite::Transaction<'_>, job_id: i64, user: &str, uploads: &[(String, String)]) -> Result<()> {
        let t = now();
        for (name, handle) in uploads {
            let n = tx.execute(
                "UPDATE uploads SET job_id = ?1, name = ?2 WHERE handle = ?3 AND user = ?4 AND job_id IS NULL AND deleted_at IS NULL AND expires_at > ?5",
                params![job_id, name, handle, user, t],
            )?;
            if n != 1 {
                return Err(StoreError::Conflict(format!("upload {handle} is gone, expired or already used — upload it again")));
            }
        }
        Ok(())
    }

    /// Uploads whose bytes should go: never taken and expired, or taken by a job that
    /// has finished.
    pub fn uploads_to_delete(&self) -> Result<Vec<String>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT u.handle FROM uploads u LEFT JOIN jobs j ON j.id = u.job_id
             WHERE u.deleted_at IS NULL
               AND ((u.job_id IS NULL AND u.expires_at <= ?1)
                 OR j.state IN ('done', 'merged', 'cancelled', 'queue_timeout', 'interrupted'))",
        )?;
        let v = st.query_map([now()], |r| r.get(0))?.collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(v)
    }

    pub fn mark_upload_deleted(&self, handle: &str) -> Result<()> {
        self.conn().execute("UPDATE uploads SET deleted_at = ?2 WHERE handle = ?1 AND deleted_at IS NULL", params![handle, now()])?;
        Ok(())
    }
}
