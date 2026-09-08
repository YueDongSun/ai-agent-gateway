//! Sessions DAO (sessions table).
//!
//! Mirrors cc-switch `src-tauri/src/session_manager/mod.rs:10`. Tracks chat
//! session history per app (claude_code, codex, etc.). `source_path` points
//! at the original JSONL on disk; the UI loads full messages on demand by
//! reading that file (a later step).
//!
//! Session *scanning* (finding JSONL files on disk) and *message loading*
//! (parsing JSONL into `SessionMessage`) land in a later step.

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// Metadata for a chat session. One row per discovered JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    /// Composite primary key: `"{app_id}:{session_id}"` for uniqueness.
    pub id: String,
    pub app_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_command: Option<String>,
    #[serde(default)]
    pub message_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<i64>,
}

impl Database {
    /// List all sessions, newest activity first.
    #[allow(dead_code)]
    pub fn get_all_sessions(&self) -> Result<Vec<SessionMeta>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, app_id, session_id, title, summary, project_dir, source_path,
                    resume_command, message_count, started_at, last_active_at
             FROM sessions ORDER BY COALESCE(last_active_at, started_at, 0) DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SessionMeta {
                id: row.get(0)?,
                app_id: row.get(1)?,
                session_id: row.get(2)?,
                title: row.get(3)?,
                summary: row.get(4)?,
                project_dir: row.get(5)?,
                source_path: row.get(6)?,
                resume_command: row.get(7)?,
                message_count: row.get(8)?,
                started_at: row.get(9)?,
                last_active_at: row.get(10)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Fetch a single session by id.
    #[allow(dead_code)]
    pub fn get_session(&self, id: &str) -> Result<Option<SessionMeta>, AppError> {
        let conn = lock_conn!(self.conn);
        let result = conn.query_row(
            "SELECT id, app_id, session_id, title, summary, project_dir, source_path,
                    resume_command, message_count, started_at, last_active_at
             FROM sessions WHERE id = ?1",
            params![id],
            |row| {
                Ok(SessionMeta {
                    id: row.get(0)?,
                    app_id: row.get(1)?,
                    session_id: row.get(2)?,
                    title: row.get(3)?,
                    summary: row.get(4)?,
                    project_dir: row.get(5)?,
                    source_path: row.get(6)?,
                    resume_command: row.get(7)?,
                    message_count: row.get(8)?,
                    started_at: row.get(9)?,
                    last_active_at: row.get(10)?,
                })
            },
        );
        match result {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Upsert a session. The caller picks the id (typically `"{app_id}:{session_id}"`).
    #[allow(dead_code)]
    pub fn save_session(&self, s: &SessionMeta) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT INTO sessions (
                id, app_id, session_id, title, summary, project_dir, source_path,
                resume_command, message_count, started_at, last_active_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(id) DO UPDATE SET
                title=excluded.title, summary=excluded.summary,
                project_dir=excluded.project_dir, source_path=excluded.source_path,
                resume_command=excluded.resume_command,
                message_count=excluded.message_count,
                last_active_at=excluded.last_active_at",
            params![
                s.id, s.app_id, s.session_id, s.title, s.summary, s.project_dir,
                s.source_path, s.resume_command, s.message_count as i64,
                s.started_at, s.last_active_at,
            ],
        )?;
        Ok(())
    }

    /// Delete a session by id.
    #[allow(dead_code)]
    pub fn delete_session(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// List sessions filtered by app_id. Newest activity first.
    #[allow(dead_code)]
    pub fn get_sessions_by_app(&self, app_id: &str) -> Result<Vec<SessionMeta>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, app_id, session_id, title, summary, project_dir, source_path,
                    resume_command, message_count, started_at, last_active_at
             FROM sessions WHERE app_id = ?1
             ORDER BY COALESCE(last_active_at, started_at, 0) DESC",
        )?;
        let rows = stmt.query_map(params![app_id], |row| {
            Ok(SessionMeta {
                id: row.get(0)?,
                app_id: row.get(1)?,
                session_id: row.get(2)?,
                title: row.get(3)?,
                summary: row.get(4)?,
                project_dir: row.get(5)?,
                source_path: row.get(6)?,
                resume_command: row.get(7)?,
                message_count: row.get(8)?,
                started_at: row.get(9)?,
                last_active_at: row.get(10)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Database {
        Database::memory().expect("memory db")
    }

    fn make_session(id: &str, app_id: &str) -> SessionMeta {
        SessionMeta {
            id: id.into(),
            app_id: app_id.into(),
            session_id: id.split(':').last().unwrap_or(id).into(),
            title: Some(format!("Session {id}")),
            summary: None,
            project_dir: Some("/tmp/proj".into()),
            source_path: Some("/tmp/proj/.claude/sessions/x.jsonl".into()),
            resume_command: Some("claude --resume x".into()),
            message_count: 5,
            started_at: Some(1_700_000_000),
            last_active_at: Some(1_700_001_000),
        }
    }

    #[test]
    fn save_get_delete_session() {
        let db = make_db();
        let s = make_session("claude_code:abc", "claude_code");
        db.save_session(&s).unwrap();

        let fetched = db.get_session("claude_code:abc").unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().title.as_deref(), Some("Session claude_code:abc"));

        let all = db.get_all_sessions().unwrap();
        assert_eq!(all.len(), 1);

        db.delete_session("claude_code:abc").unwrap();
        assert!(db.get_session("claude_code:abc").unwrap().is_none());
    }

    #[test]
    fn get_sessions_by_app_filters() {
        let db = make_db();
        db.save_session(&make_session("claude_code:a", "claude_code")).unwrap();
        db.save_session(&make_session("claude_code:b", "claude_code")).unwrap();
        db.save_session(&make_session("codex:x", "codex")).unwrap();

        let claude = db.get_sessions_by_app("claude_code").unwrap();
        assert_eq!(claude.len(), 2);
        for s in &claude {
            assert_eq!(s.app_id, "claude_code");
        }

        let codex = db.get_sessions_by_app("codex").unwrap();
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].id, "codex:x");
    }
}