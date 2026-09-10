//! `app_meta` key/value settings DAO.
//!
//! Device-level preferences stored in SQLite (e.g. the Codex CLI isolated
//! CODEX_HOME directory). Mirrors cc-switch's `settings.rs` concept but kept
//! minimal: a generic string key/value table with no schema beyond that.

use crate::database::{lock_conn, Database};
use crate::error::AppError;

/// Well-known setting key: Codex CLI isolated config directory (independent
/// CODEX_HOME). When set, AAG writes Codex's live config (config.toml /
/// auth.json) to this directory instead of the default `~/.codex`, fully
/// isolating the CLI from the Codex desktop app's shared config.
pub const KEY_CODEX_CLI_HOME: &str = "codex_cli_home";
/// Well-known setting key: global outbound proxy (injected into the Codex CLI
/// terminal).
pub const KEY_OUTBOUND_PROXY: &str = "outbound_proxy";

impl Database {
    /// Read a setting value. Returns `Ok(None)` if the key is absent.
    pub(crate) fn get_setting(&self, key: &str) -> Result<Option<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare("SELECT value FROM app_meta WHERE key = ?1")
            .map_err(AppError::from)?;
        let mut rows = stmt.query([key]).map_err(AppError::from)?;
        match rows.next().map_err(AppError::from)? {
            Some(row) => row.get::<_, String>(0).map(Some).map_err(AppError::from),
            None => Ok(None),
        }
    }

    /// Write (upsert) a setting value. An empty value removes the key.
    pub(crate) fn set_setting(&self, key: &str, value: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        if value.trim().is_empty() {
            conn.execute("DELETE FROM app_meta WHERE key = ?1", [key])
                .map_err(AppError::from)?;
        } else {
            conn.execute(
                "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [key, value],
            )
            .map_err(AppError::from)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;

    #[test]
    fn setting_roundtrip_and_delete() {
        let db = Database::memory().expect("memory db");
        assert_eq!(db.get_setting(KEY_CODEX_CLI_HOME).unwrap(), None);
        db.set_setting(KEY_CODEX_CLI_HOME, "C:\\Users\\me\\.codex-cli")
            .unwrap();
        assert_eq!(
            db.get_setting(KEY_CODEX_CLI_HOME).unwrap().as_deref(),
            Some("C:\\Users\\me\\.codex-cli")
        );
        // upsert
        db.set_setting(KEY_CODEX_CLI_HOME, "/home/me/.codex-cli").unwrap();
        assert_eq!(
            db.get_setting(KEY_CODEX_CLI_HOME).unwrap().as_deref(),
            Some("/home/me/.codex-cli")
        );
        // empty value deletes
        db.set_setting(KEY_CODEX_CLI_HOME, "  ").unwrap();
        assert_eq!(db.get_setting(KEY_CODEX_CLI_HOME).unwrap(), None);
    }

    #[test]
    fn setting_keys_are_independent() {
        let db = Database::memory().expect("memory db");
        db.set_setting(KEY_CODEX_CLI_HOME, "/a").unwrap();
        db.set_setting(KEY_OUTBOUND_PROXY, "http://127.0.0.1:7890")
            .unwrap();
        assert_eq!(
            db.get_setting(KEY_CODEX_CLI_HOME).unwrap().as_deref(),
            Some("/a")
        );
        assert_eq!(
            db.get_setting(KEY_OUTBOUND_PROXY).unwrap().as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }
}
