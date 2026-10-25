//! Proposal metadata. Branch heads and `main` live in git (design §6.4) — this table only
//! keeps title, status, author, reasons and comments.

use crate::{Result, Store, StoreError};
use cimon_core::now;
use rusqlite::{params, OptionalExtension, Row};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalRow {
    pub id: i64,
    pub branch: String,
    pub title: String,
    pub description: Option<String>,
    pub author: String,
    pub status: String,
    pub base_commit: String,
    pub created_at: String,
    pub updated_at: String,
    pub promoted_commit: Option<String>,
    pub promoted_by: Option<String>,
    pub promoted_at: Option<String>,
    pub override_reason: Option<String>,
    pub closed_by: Option<String>,
    pub closed_at: Option<String>,
    pub close_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CommentRow {
    pub id: i64,
    pub proposal_id: i64,
    pub author: String,
    pub body: String,
    pub head_commit: String,
    pub created_at: String,
}

const COLS: &str = "id, branch, title, description, author, status, base_commit, created_at, updated_at,
  promoted_commit, promoted_by, promoted_at, override_reason, closed_by, closed_at, close_reason";

fn from_row(r: &Row<'_>) -> rusqlite::Result<ProposalRow> {
    Ok(ProposalRow {
        id: r.get(0)?,
        branch: r.get(1)?,
        title: r.get(2)?,
        description: r.get(3)?,
        author: r.get(4)?,
        status: r.get(5)?,
        base_commit: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        promoted_commit: r.get(9)?,
        promoted_by: r.get(10)?,
        promoted_at: r.get(11)?,
        override_reason: r.get(12)?,
        closed_by: r.get(13)?,
        closed_at: r.get(14)?,
        close_reason: r.get(15)?,
    })
}

impl Store {
    pub fn create_proposal(&self, branch: &str, title: &str, author: &str, base_commit: &str) -> Result<i64> {
        let t = now();
        let res = self.conn().execute(
            "INSERT INTO proposals (branch, title, author, status, base_commit, created_at, updated_at) VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?5)",
            params![branch, title, author, base_commit, t],
        );
        match res {
            Ok(_) => Ok(self.conn().last_insert_rowid()),
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                Err(StoreError::Conflict(format!("{branch} already has an open proposal")))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn proposal(&self, id: i64) -> Result<Option<ProposalRow>> {
        Ok(self.conn().query_row(&format!("SELECT {COLS} FROM proposals WHERE id = ?1"), [id], from_row).optional()?)
    }

    pub fn open_proposal_for_branch(&self, branch: &str) -> Result<Option<ProposalRow>> {
        Ok(self
            .conn()
            .query_row(&format!("SELECT {COLS} FROM proposals WHERE branch = ?1 AND status = 'open'"), [branch], from_row)
            .optional()?)
    }

    pub fn proposals(&self, status: Option<&str>, limit: i64) -> Result<Vec<ProposalRow>> {
        let c = self.conn();
        let rows = match status {
            Some(s) => {
                let mut st = c.prepare(&format!("SELECT {COLS} FROM proposals WHERE status = ?1 ORDER BY id DESC LIMIT ?2"))?;
                let r = st.query_map(params![s, limit], from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
                r
            }
            None => {
                let mut st = c.prepare(&format!("SELECT {COLS} FROM proposals ORDER BY id DESC LIMIT ?1"))?;
                let r = st.query_map([limit], from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
                r
            }
        };
        Ok(rows)
    }

    pub fn update_proposal_text(&self, id: i64, title: Option<&str>, description: Option<&str>) -> Result<()> {
        let c = self.conn();
        if let Some(t) = title {
            c.execute("UPDATE proposals SET title = ?2, updated_at = ?3 WHERE id = ?1", params![id, t, now()])?;
        }
        if let Some(d) = description {
            c.execute("UPDATE proposals SET description = ?2, updated_at = ?3 WHERE id = ?1", params![id, d, now()])?;
        }
        Ok(())
    }

    pub fn touch_proposal(&self, id: i64) -> Result<()> {
        self.conn().execute("UPDATE proposals SET updated_at = ?2 WHERE id = ?1", params![id, now()])?;
        Ok(())
    }

    pub fn set_proposal_base(&self, id: i64, base: &str) -> Result<()> {
        self.conn().execute("UPDATE proposals SET base_commit = ?2, updated_at = ?3 WHERE id = ?1", params![id, base, now()])?;
        Ok(())
    }

    /// Records who is promoting `commit` and why, before `main` moves — so a crash
    /// between the ref update and `mark_promoted` keeps the promoter and the override
    /// reason (reconcile reuses them).
    pub fn set_promote_intent(&self, id: i64, commit: &str, by: &str, override_reason: Option<&str>) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE proposals SET promoted_commit = ?2, promoted_by = ?3, override_reason = ?4 WHERE id = ?1 AND status = 'open'",
            params![id, commit, by, override_reason],
        )?;
        if n == 0 {
            return Err(StoreError::Conflict(format!("proposal {id} is not open")));
        }
        Ok(())
    }

    pub fn clear_promote_intent(&self, id: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE proposals SET promoted_commit = NULL, promoted_by = NULL, override_reason = NULL WHERE id = ?1 AND status = 'open'",
            [id],
        )?;
        Ok(())
    }

    /// Marks a proposal promoted. When an intent for the same commit was recorded, its
    /// promoter and reason win over `by`/`override_reason` (reconcile passes its own).
    pub fn mark_promoted(&self, id: i64, commit: &str, by: &str, override_reason: Option<&str>) -> Result<()> {
        let t = now();
        self.conn().execute(
            "UPDATE proposals SET status = 'promoted',
               promoted_by = CASE WHEN promoted_commit = ?2 AND promoted_by IS NOT NULL THEN promoted_by ELSE ?3 END,
               override_reason = CASE WHEN promoted_commit = ?2 AND promoted_by IS NOT NULL THEN override_reason ELSE ?5 END,
               promoted_commit = ?2, promoted_at = ?4, updated_at = ?4
             WHERE id = ?1 AND status = 'open'",
            params![id, commit, by, t, override_reason],
        )?;
        Ok(())
    }

    pub fn mark_closed(&self, id: i64, by: &str, reason: Option<&str>) -> Result<()> {
        let t = now();
        self.conn().execute(
            "UPDATE proposals SET status = 'closed', closed_by = ?2, closed_at = ?3, close_reason = ?4, updated_at = ?3
             WHERE id = ?1 AND status = 'open'",
            params![id, by, t, reason],
        )?;
        Ok(())
    }

    /// Closed proposals whose commits should be dropped from `refs/cimon/closed/*`.
    pub fn closed_before(&self, before: &str) -> Result<Vec<ProposalRow>> {
        let c = self.conn();
        let mut st = c.prepare(&format!("SELECT {COLS} FROM proposals WHERE status = 'closed' AND closed_at < ?1"))?;
        let rows = st.query_map([before], from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn add_comment(&self, proposal_id: i64, author: &str, body: &str, head: &str) -> Result<i64> {
        let c = self.conn();
        c.execute(
            "INSERT INTO proposal_comments (proposal_id, author, body, head_commit, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![proposal_id, author, body, head, now()],
        )?;
        let id = c.last_insert_rowid();
        c.execute("UPDATE proposals SET updated_at = ?2 WHERE id = ?1", params![proposal_id, now()])?;
        Ok(id)
    }

    pub fn comments(&self, proposal_id: i64) -> Result<Vec<CommentRow>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, proposal_id, author, body, head_commit, created_at FROM proposal_comments
             WHERE proposal_id = ?1 AND deleted_at IS NULL ORDER BY id",
        )?;
        let rows = st
            .query_map([proposal_id], |r| {
                Ok(CommentRow {
                    id: r.get(0)?,
                    proposal_id: r.get(1)?,
                    author: r.get(2)?,
                    body: r.get(3)?,
                    head_commit: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_open_proposal_per_branch() {
        let s = Store::open_in_memory().unwrap();
        let a = s.create_proposal("recipe/x", "t", "u", "c0").unwrap();
        assert!(s.create_proposal("recipe/x", "t", "u", "c0").is_err());
        s.mark_promoted(a, "c1", "admin", None).unwrap();
        let b = s.create_proposal("recipe/x", "again", "u", "c1").unwrap();
        assert_ne!(a, b);
        assert_eq!(s.open_proposal_for_branch("recipe/x").unwrap().unwrap().id, b);
        s.add_comment(b, "u", "hi", "c2").unwrap();
        assert_eq!(s.comments(b).unwrap().len(), 1);
    }
}
