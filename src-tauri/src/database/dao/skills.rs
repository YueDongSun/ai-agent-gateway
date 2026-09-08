//! Skills DAO (skills table).
//!
//! Mirrors cc-switch `database/dao/skills.rs` + `database/schema.rs:84`. Tracks
//! installed skills (per-app enabled flags + repo metadata + content hash).
//! The filesystem symlink/copy layer (SSOT in `~/.aag/skills/`) lands in a later
//! step.

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// Metadata for an installed skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledSkill {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Filesystem directory where skill files live.
    pub directory: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "repoOwner")]
    pub repo_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "repoName")]
    pub repo_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "repoBranch")]
    pub repo_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "contentHash")]
    pub content_hash: Option<String>,
    #[serde(default, rename = "enabledClaude")]
    pub enabled_claude: bool,
    #[serde(default, rename = "enabledCodex")]
    pub enabled_codex: bool,
    #[serde(rename = "installedAt")]
    pub installed_at: i64,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
}

impl Database {
    /// List all installed skills, sorted by id.
    #[allow(dead_code)]
    pub fn get_all_skills(&self) -> Result<Vec<InstalledSkill>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, name, description, directory, repo_owner, repo_name, repo_branch,
                    content_hash, enabled_claude, enabled_codex, installed_at, updated_at
             FROM skills ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(InstalledSkill {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                directory: row.get(3)?,
                repo_owner: row.get(4)?,
                repo_name: row.get(5)?,
                repo_branch: row.get(6)?,
                content_hash: row.get(7)?,
                enabled_claude: row.get(8)?,
                enabled_codex: row.get(9)?,
                installed_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Fetch a single skill by id.
    #[allow(dead_code)]
    pub fn get_skill(&self, id: &str) -> Result<Option<InstalledSkill>, AppError> {
        let conn = lock_conn!(self.conn);
        let result = conn.query_row(
            "SELECT id, name, description, directory, repo_owner, repo_name, repo_branch,
                    content_hash, enabled_claude, enabled_codex, installed_at, updated_at
             FROM skills WHERE id = ?1",
            params![id],
            |row| {
                Ok(InstalledSkill {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    directory: row.get(3)?,
                    repo_owner: row.get(4)?,
                    repo_name: row.get(5)?,
                    repo_branch: row.get(6)?,
                    content_hash: row.get(7)?,
                    enabled_claude: row.get(8)?,
                    enabled_codex: row.get(9)?,
                    installed_at: row.get(10)?,
                    updated_at: row.get(11)?,
                })
            },
        );
        match result {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Upsert a skill.
    #[allow(dead_code)]
    pub fn save_skill(&self, s: &InstalledSkill) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO skills (
                id, name, description, directory, repo_owner, repo_name, repo_branch,
                content_hash, enabled_claude, enabled_codex, installed_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, description=excluded.description,
                directory=excluded.directory, repo_owner=excluded.repo_owner,
                repo_name=excluded.repo_name, repo_branch=excluded.repo_branch,
                content_hash=excluded.content_hash,
                enabled_claude=excluded.enabled_claude, enabled_codex=excluded.enabled_codex,
                updated_at=?12",
            params![
                s.id, s.name, s.description, s.directory, s.repo_owner, s.repo_name,
                s.repo_branch, s.content_hash, s.enabled_claude, s.enabled_codex,
                s.installed_at, now,
            ],
        )?;
        Ok(())
    }

    /// Delete a skill by id.
    #[allow(dead_code)]
    pub fn delete_skill(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM skills WHERE id = ?1", params![id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Database {
        Database::memory().expect("memory db")
    }

    fn make_skill(id: &str) -> InstalledSkill {
        InstalledSkill {
            id: id.into(),
            name: format!("Skill {id}"),
            description: Some("A test skill".into()),
            directory: format!("/tmp/skills/{id}"),
            repo_owner: Some("anthropics".into()),
            repo_name: Some("skills".into()),
            repo_branch: Some("main".into()),
            content_hash: Some("abc123".into()),
            enabled_claude: true,
            enabled_codex: false,
            installed_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        }
    }

    #[test]
    fn save_get_delete_skill() {
        let db = make_db();
        let s = make_skill("test-skill");
        db.save_skill(&s).unwrap();

        let fetched = db.get_skill("test-skill").unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().name, "Skill test-skill");

        let all = db.get_all_skills().unwrap();
        assert_eq!(all.len(), 1);

        db.delete_skill("test-skill").unwrap();
        assert!(db.get_skill("test-skill").unwrap().is_none());
    }

    #[test]
    fn save_skill_updates_updated_at() {
        let db = make_db();
        let mut s = make_skill("s1");
        s.updated_at = 0;
        db.save_skill(&s).unwrap();
        let fetched = db.get_skill("s1").unwrap().unwrap();
        assert!(fetched.updated_at > 0, "save_skill should set updated_at to now");
    }
}