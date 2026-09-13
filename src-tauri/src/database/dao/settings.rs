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
/// Well-known setting key: when `"1"`, skip the cc-switch coexistence
/// check before provider switching / CLI terminal launch. Allows advanced
/// users to opt out of the safety dialog when they knowingly run both
/// apps side-by-side (off by default).
pub const KEY_CCSWITCH_OVERRIDE_ENABLED: &str = "ccswitch_override_enabled";
/// Well-known setting key: when `"1"`, AAG actively blocks writes that
/// would clobber cc-switch's `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` in
/// `~/.claude/settings.json` and `~/.codex/config.toml`. Off by default
/// because the typical user setup is the reverse: cc-switch chains
/// *through* AAG (cc-switch → AAG → upstream), and AAG only serves
/// Anthropic `/v1/messages` to upstream-aware clients like Codex CLI's
/// isolated CODEX_HOME window. The banner is always shown when cc-switch
/// is detected; this toggle only flips the hard-block behaviour.
pub const KEY_CCSWITCH_BLOCK_ENABLED: &str = "ccswitch_block_enabled";
/// Well-known setting key: when `"1"`, `proxy_start` runs an
/// automatic `INSERT OR IGNORE` pass that pulls every claude_code
/// provider out of `~/.cc-switch/cc-switch.db` into AAG. Existing AAG
/// rows are not overwritten; manually-tuned providers can opt out via
/// `providers.sync_locked = true` (toggled in the Providers UI).
pub const KEY_SYNC_WITH_CCSWITCH: &str = "sync_with_cc_switch_enabled";
/// Well-known setting key: the most recent cc-switch detection status,
/// stored as a JSON-encoded `CcSwitchStatus`. Lets the frontend skip the
/// initial detect() round-trip after a relaunch when the snapshot is
/// fresh enough.
pub const KEY_CCSWITCH_LAST_STATUS: &str = "ccswitch_last_status_json";

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
