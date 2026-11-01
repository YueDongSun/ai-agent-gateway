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
///
/// **Default is OFF** — `proxy_start` treats an absent key or any
/// value other than `"1"` as disabled. `aag-setup` clears any pre-
/// existing `"1"` after the explicit one-shot import so future boots
/// don't silently re-import cc-switch changes.
pub const KEY_SYNC_WITH_CCSWITCH: &str = "sync_with_cc_switch_enabled";
/// Well-known setting key: the most recent cc-switch detection status,
/// stored as a JSON-encoded `CcSwitchStatus`. Lets the frontend skip the
/// initial detect() round-trip after a relaunch when the snapshot is
/// fresh enough.
pub const KEY_CCSWITCH_LAST_STATUS: &str = "ccswitch_last_status_json";
/// Well-known setting key: the pinned proxy provider id. When set, ALL
/// requests are forced through that provider (bypassing the failover chain).
/// Cleared by setting to empty string.
pub const KEY_PROXY_PINNED_PROVIDER: &str = "proxy_pinned_provider";
/// Well-known setting key: when `"1"`, `/v1/responses` requests get the
/// built-in chain-of-thought system prompt injected as their first message.
/// Absent (or any other value) = disabled. Live-toggled via
/// `proxy_set_responses_cot` — no proxy restart needed; cleared by setting
/// to empty string (row delete).
pub const KEY_PROXY_RESPONSES_COT_ENABLED: &str = "proxy_responses_cot_enabled";
/// Well-known setting key: how the Codex mirror's config.toml exposes AAG.
/// `"exclusive"` (absent/unknown — historic default) forces the top-level
/// `model_provider = "aag"`; `"coexist"` keeps the builtin openai provider
/// as the default (a ChatGPT login works for official models) and exposes
/// AAG via `[model_providers.aag]` + a `[profiles.aag]` profile for manual
/// switching. Cleared by setting to empty string.
pub const KEY_CODEX_MIRROR_MODE: &str = "codex_mirror_mode";
/// Well-known setting key: the Claude mirror config directory
/// (CLAUDE_CONFIG_DIR override). When set, AAG writes the Claude mirror's
/// `settings.json` there instead of the default `~/.claude-cli`; a value
/// resolving to the native `~/.claude` is refused.
pub const KEY_CLAUDE_CLI_HOME: &str = "claude_cli_home";
/// Well-known setting key: how the Claude mirror authenticates to AAG.
/// `"token"` (absent/unknown — default) writes `ANTHROPIC_AUTH_TOKEN` with
/// AAG's proxy token into the mirror `settings.json`; `"passthrough"`
/// omits the key so a subscription login made inside the mirror flows to
/// AAG verbatim (relayed by `anthropic_passthrough` pool rows). Cleared
/// by setting to empty string.
pub const KEY_CLAUDE_MIRROR_MODE: &str = "claude_mirror_mode";
/// Well-known setting key: unix epoch seconds of the last model-discovery
/// refresh run (all triggers). Written by `model_discovery::run_refresh`.
pub const KEY_MODEL_DISCOVERY_LAST_RUN: &str = "model_discovery_last_run_epoch";
/// Well-known setting key: JSON-encoded `RefreshReport` of the last
/// model-discovery refresh, surfaced to the frontend via
/// `model_discovery_report`.
pub const KEY_MODEL_DISCOVERY_LAST_REPORT: &str = "model_discovery_last_report_json";
/// Well-known setting key: minimum hours between periodic model-discovery
/// refreshes. Absent or unparsable = 24; `0` disables the periodic trigger
/// (manual and startup refreshes still run).
pub const KEY_MODEL_DISCOVERY_INTERVAL_HOURS: &str = "model_discovery_interval_hours";
/// Well-known setting key: when `"1"`, the last model-discovery refresh
/// changed routing state while the proxy was running, so the new models
/// are pending until the next proxy restart. Cleared by `proxy_start`.
pub const KEY_MODEL_DISCOVERY_PENDING_RESTART: &str = "model_discovery_pending_restart";
/// Well-known setting key: how the failover queues order their candidate
/// hops. `"order"` (absent/unknown — default) keeps the configured order;
/// `"rotate"` round-robins the chain's starting position per request;
/// `"weight"` spreads first attempts over the chain by each provider's
/// `proxy_providers.weight` with smooth weighted round-robin (0 = standby
/// only, still present as failover). Explicit per-model routes and
/// planner-assessed alias candidates keep their order in every mode.
/// Part of the routing fingerprint, so writes hot-apply to new requests.
pub const KEY_ROUTING_MODE: &str = "routing_mode";

impl Database {
    /// Read a setting value. Returns `Ok(None)` if the key is absent.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, AppError> {
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
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), AppError> {
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
        db.set_setting(KEY_CODEX_CLI_HOME, "/home/me/.codex-cli")
            .unwrap();
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
