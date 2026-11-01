//! Atomic workspace-registry replacement with one recoverable prior value.

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::{params, OptionalExtension};

pub(crate) const WORKSPACES_KEY: &str = "prompt_workspaces_v1";
const BACKUP_KEY: &str = "prompt_workspaces_v1_previous";

impl Database {
    pub(crate) fn replace_prompt_workspaces(
        &self,
        expected: Option<&str>,
        replacement: &str,
    ) -> Result<(), AppError> {
        let mut connection = lock_conn!(self.conn);
        let transaction = connection.transaction()?;
        let current: Option<String> = transaction.query_row(
            "SELECT value FROM app_meta WHERE key = ?1", [WORKSPACES_KEY], |row| row.get(0),
        ).optional()?;
        if current.as_deref() != expected {
            return Err(AppError::InvalidInput("prompt_workspace_registry_changed".into()));
        }
        if let Some(previous) = current {
            transaction.execute(
                "INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![BACKUP_KEY, previous],
            )?;
        }
        transaction.execute(
            "INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![WORKSPACES_KEY, replacement],
        )?;
        transaction.commit()?;
        Ok(())
    }
}
