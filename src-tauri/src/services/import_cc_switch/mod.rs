//! Import provider rows from a cc-switch SQLite database.
//!
//! cc-switch (https://github.com/farion1231/cc-switch) stores its
//! provider catalogue in `~/.cc-switch/cc-switch.db` — a SQLite file
//! whose `providers` table is schema-compatible with AAG's (same
//! column order and types, see
//! `~/Documents/.../cc-switch-src/src-tauri/src/database/schema.rs:27`).
//!
//! The importer:
//! 1. Opens the cc-switch DB read-only (`SQLITE_OPEN_READ_ONLY`).
//! 2. SELECTs every row from `providers`.
//! 3. Skips rows whose `(id, app_type)` already exists in AAG
//!    (`INSERT OR IGNORE` semantics) — re-runnable.
//! 4. Returns the list of `(id, app_type, name)` actually imported so
//!    the UI can show a toast / confirmation.
//!
//! Privacy: the DB lives outside the repo and is user-specific, so it's
//! already excluded from version control via `.gitignore`. The import
//! is explicit (one Tauri command invocation), not automatic.

use crate::database::Database;
use crate::error::AppError;
use crate::provider::Provider;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::path::PathBuf;

/// One row of import feedback for the UI.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedProvider {
    pub id: String,
    pub app_type: String,
    pub name: String,
}

/// One failover route that was auto-built from an imported provider's
/// `ANTHROPIC_DEFAULT_*_MODEL` env keys. Surfaced to the UI so the
/// "Import from cc-switch" toast can report how many routes were created.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedRoute {
    /// Client-facing alias (e.g. `claude-sonnet-5`).
    pub alias: String,
    /// `proxy_provider` id of the imported upstream used as the new hop.
    pub provider: String,
    /// Upstream model that the hop was filled with (from ANTHROPIC_*).
    pub model: String,
}

/// Result of an import run.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub source_db: String,
    pub imported: Vec<ImportedProvider>,
    pub skipped_existing: Vec<ImportedProvider>,
    pub total_in_source: usize,
    /// Failover routes auto-built from `env.ANTHROPIC_DEFAULT_*_MODEL` of
    /// the imported Claude providers. Empty for non-Claude imports.
    #[serde(default)]
    pub imported_routes: Vec<ImportedRoute>,
}

/// Resolve the cc-switch DB path under the user's home directory.
/// Honours `AAG_HOME_DIR` (for tests) and `CC_SWITCH_DB_PATH` (for users
/// who installed cc-switch to a non-default location).
pub fn cc_switch_db_path() -> PathBuf {
    if let Ok(custom) = std::env::var("CC_SWITCH_DB_PATH") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    crate::config::get_home_dir().join(".cc-switch").join("cc-switch.db")
}

/// Map cc-switch's `app_type` values onto AAG's. cc-switch uses short
/// hyphenated ids (`claude`, `claude-desktop`, `grokbuild`); AAG uses
/// snake_case (`claude_code`, `claude_desktop`, `grok_build`). Values
/// that already match pass through unchanged; unknown values pass
/// through as-is so the row still lands (switching it may fail with
/// "unknown app_type", but the data isn't lost).
pub fn map_app_type(cc_switch_app_type: &str) -> String {
    match cc_switch_app_type {
        "claude" => "claude_code".into(),
        "claude-desktop" => "claude_desktop".into(),
        "grokbuild" => "grok_build".into(),
        // OpenCode uses the same OpenAI-compatible protocol as Copilot CLI;
        // import its providers as copilot so they appear on the Copilot page.
        "opencode" => "copilot".into(),
        other => other.into(),
    }
}

/// One-shot full import: all providers (any app_type) from cc-switch.
/// UI uses this via the "Import from cc-switch" button.
pub fn import_from_db<P: AsRef<std::path::Path>>(
    db: &Database,
    cc_switch_path: P,
) -> Result<ImportReport, AppError> {
    let rows = read_cc_switch_providers(cc_switch_path.as_ref())?;
    insert_aag_providers(db, rows, /*respect_sync_locked=*/ false)
}

/// Auto-sync: only `claude_code` providers (those are the ones used in the
/// failover queue), honouring each row's `sync_locked` bit — manually
/// tuned providers are skipped, not overwritten. Used by `proxy_start`
/// when the user has flipped the auto-sync switch on.
pub fn sync_claude_code_from_cc_switch<P: AsRef<std::path::Path>>(
    db: &Database,
    cc_switch_path: P,
) -> Result<ImportReport, AppError> {
    let all = read_cc_switch_providers(cc_switch_path.as_ref())?;
    let rows: Vec<CcSwitchRow> = all
        .into_iter()
        .filter(|r| r.app_type == "claude")
        .collect();
    insert_aag_providers(db, rows, /*respect_sync_locked=*/ true)
}

/// Read every row from the cc-switch `providers` table. Pure I/O — no
/// AAG DB touched. Returns an empty Vec if the DB doesn't exist.
fn read_cc_switch_providers(path: &std::path::Path) -> Result<Vec<CcSwitchRow>, AppError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| AppError::Database(format!("failed to open cc-switch DB: {e}")))?;

    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='providers'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| AppError::Database(format!("cc-switch DB probe failed: {e}")))?;
    if exists == 0 {
        return Err(AppError::Database(format!(
            "cc-switch DB at {} has no `providers` table — wrong file?",
            path.display()
        )));
    }

    let mut stmt = conn
        .prepare(
            "SELECT id, app_type, name, settings_config FROM providers",
        )
        .map_err(|e| AppError::Database(format!("cc-switch DB prepare failed: {e}")))?;
    let row_results = stmt
        .query_map([], |r| {
            Ok(CcSwitchRow {
                id: r.get(0)?,
                app_type: map_app_type(&r.get::<_, String>(1)?),
                name: r.get(2)?,
                settings_config: r.get(3)?,
            })
        })
        .map_err(|e| AppError::Database(format!("cc-switch DB query failed: {e}")))?;
    row_results
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Database(format!("cc-switch DB row decode failed: {e}")))
}

/// Internal: rows as read from cc-switch's DB. `app_type` already
/// mapped onto AAG's snake_case.
struct CcSwitchRow {
    id: String,
    app_type: String,
    name: String,
    settings_config: String,
}

fn insert_aag_providers(
    db: &Database,
    rows: Vec<CcSwitchRow>,
    respect_sync_locked: bool,
) -> Result<ImportReport, AppError> {
    let source_db = cc_switch_db_path().to_string_lossy().into_owned();
    let total_in_source = rows.len();

    // Self-loop guard: if AAG is itself registered as a provider inside
    // cc-switch (the "cc-switch → AAG → upstream" topology), pulling it
    // back through auto-sync would make AAG route its own queue through
    // itself. Detect any entry whose settings_config contains the AAG
    // proxy listen address and drop it before insertion.
    let aag_listen = db
        .get_proxy_config()
        .ok()
        .map(|c| format!("{}:{}", c.host, c.port))
        .unwrap_or_default();

    let mut imported: Vec<ImportedProvider> = Vec::new();
    let mut skipped_existing: Vec<ImportedProvider> = Vec::new();
    let mut skipped_self: Vec<ImportedProvider> = Vec::new();
    // Successfully-imported Claude providers, kept by value so the
    // post-loop route builder can read their env block. Other app_types
    // don't contribute ANTHROPIC_* mapping, so they're not tracked.
    let mut imported_claude: Vec<(String, serde_json::Value)> = Vec::new();

    for row in rows {
        let settings_value: serde_json::Value = serde_json::from_str(&row.settings_config)
            .map_err(|e| AppError::Database(format!(
                "cc-switch provider {}/{}: invalid settings_config JSON: {e}",
                row.id, row.app_type
            )))?;
        if !aag_listen.is_empty() && entry_points_at_aag(&settings_value, &aag_listen) {
            log::warn!(
                "import_cc_switch: skipping {}/{} ({}) — settings_config targets AAG itself at {}",
                row.id, row.app_type, row.name, aag_listen
            );
            skipped_self.push(ImportedProvider {
                id: row.id,
                app_type: row.app_type,
                name: row.name,
            });
            continue;
        }
        let provider = Provider {
            id: row.id.clone(),
            name: row.name.clone(),
            settings_config: settings_value.clone(),
            website_url: None,
            category: None,
            created_at: Some(chrono::Utc::now().timestamp()),
            sort_index: None,
            notes: None,
            icon: None,
            icon_color: None,
            meta: Some(crate::provider::ProviderMeta {
                custom_endpoints: Default::default(),
            }),
            in_failover_queue: true,
            sync_locked: false,
        };
        let existing = db.get_provider_by_id(&row.id, &row.app_type).map_err(AppError::from)?;
        match existing {
            Some(p) if respect_sync_locked && p.sync_locked => {
                // Manually locked — leave alone.
            }
            Some(_) => {
                skipped_existing.push(ImportedProvider {
                    id: row.id,
                    app_type: row.app_type,
                    name: row.name,
                });
            }
            None => {
                db.save_provider(&row.app_type, &provider).map_err(AppError::from)?;

                // Also create a corresponding proxy_provider so the imported
                // entry appears in the upstream pool (Claude page). This lets
                // users configure model mappings and failover directly.
                let pp_id = format!("ccs-{}", row.id);
                let existing_pp = db.get_all_proxy_providers()
                    .map_err(AppError::from)?
                    .into_iter()
                    .any(|p| p.id == pp_id);
                if !existing_pp {
                    if let Some((base_url, api_key, provider_type)) =
                        extract_upstream_from_settings(&row.app_type, &provider.settings_config)
                    {
                        let pp = crate::database::dao::proxy_providers::ProxyProvider {
                            id: pp_id,
                            name: row.name.clone(),
                            provider_type,
                            api_key,
                            base_url,
                            models: vec![],
                            enabled: true,
                            timeout_ms: 60_000,
                            weight: 1,
                            circuit_breaker: Default::default(),
                            model_mapping: Default::default(),
                        };
                        let _ = db.save_proxy_provider(&pp); // best-effort; import still succeeds
                    }
                }

                imported.push(ImportedProvider {
                    id: row.id.clone(),
                    app_type: row.app_type.clone(),
                    name: row.name.clone(),
                });
                // Stash a copy of this Claude provider's settings_config
                // so the post-loop route builder can read its
                // `env.ANTHROPIC_DEFAULT_*_MODEL` keys without re-parsing
                // the row off disk.
                if row.app_type == "claude_code" {
                    imported_claude.push((row.id.clone(), settings_value.clone()));
                }
            }
        }
    }

    // Auto-create failover routes for official Claude models using each
    // newly imported provider's `ANTHROPIC_DEFAULT_*_MODEL` env keys as
    // the per-hop model. cc-switch stores per-tier mapping in the JSON
    // `env` block of each provider's `settings_config`; we round-trip
    // that into AAG's route format:
    //
    //   claude-fable-5      -> env.ANTHROPIC_DEFAULT_FABLE_MODEL    (fallback: opus / default)
    //   claude-opus-4-8/7/6 -> env.ANTHROPIC_DEFAULT_OPUS_MODEL     (fallback: default)
    //   claude-sonnet-5/-4-6-> env.ANTHROPIC_DEFAULT_SONNET_MODEL   (fallback: default)
    //   claude-haiku-4-5    -> env.ANTHROPIC_DEFAULT_HAIKU_MODEL    (fallback: default)
    //
    // Each imported Claude provider contributes ONE hop per official
    // model — prepended if the route already exists, created otherwise.
    // Non-Claude imports contribute nothing (Codex/Copilot/pi have no
    // ANTHROPIC_* env keys).
    let mut imported_routes: Vec<ImportedRoute> = Vec::new();
    for (ccs_id, settings_value) in &imported_claude {
        // Extract the env block from settings_config. Failures fall
        // through to empty env (and the route builder simply emits
        // routes with no upstream model — same as before this change).
        let env = settings_value
            .as_object()
            .and_then(|o| o.get("env"))
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let pick = |keys: &[&str]| -> Option<String> {
            for k in keys {
                if let Some(s) = env.get(*k).and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
            }
            None
        };
        let default_model = pick(&["ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_MODEL"]);
        let haiku = pick(&["ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_SMALL_FAST_MODEL"])
            .or_else(|| default_model.clone());
        let sonnet = pick(&["ANTHROPIC_DEFAULT_SONNET_MODEL"]).or_else(|| default_model.clone());
        let opus = pick(&["ANTHROPIC_DEFAULT_OPUS_MODEL"]).or_else(|| default_model.clone());
        let fable = pick(&["ANTHROPIC_DEFAULT_FABLE_MODEL"])
            .or_else(|| opus.clone())
            .or_else(|| default_model.clone());

        let model_for = |alias: &str| -> Option<String> {
            if alias == "claude-haiku-4-5" {
                haiku.clone()
            } else if alias == "claude-sonnet-5" || alias == "claude-sonnet-4-6" {
                sonnet.clone()
            } else if alias.starts_with("claude-opus-") {
                opus.clone()
            } else if alias == "claude-fable-5" {
                fable.clone()
            } else {
                None
            }
        };

        // Imported Claude providers get a corresponding `ccs-<id>`
        // proxy_provider created earlier in this loop; that id is the
        // hop's provider ref.
        let pp_id = format!("ccs-{}", ccs_id);
        for m in crate::proxy::models::OFFICIAL_MODELS {
            let Some(model_name) = model_for(m.id) else { continue };
            if model_name.is_empty() {
                continue;
            }
            let new_hop = crate::database::dao::routes::RouteHop {
                provider: pp_id.clone(),
                model: model_name.clone(),
            };
            match db.get_route(m.id) {
                Ok(Some(existing)) => {
                    // Idempotent: if this provider is already the head
                    // hop, leave the route alone but still surface it.
                    if existing.hops.first().map(|h| h.provider == pp_id).unwrap_or(false) {
                        imported_routes.push(ImportedRoute {
                            alias: m.id.to_string(),
                            provider: pp_id.clone(),
                            model: model_name,
                        });
                        continue;
                    }
                    let mut hops = vec![new_hop.clone()];
                    hops.extend(existing.hops.into_iter());
                    let _ = db.save_route(&crate::database::dao::routes::Route {
                        alias: m.id.to_string(),
                        description: format!(
                            "{} — extended by cc-switch import",
                            m.display_name
                        ),
                        hops,
                    });
                }
                Ok(None) => {
                    let _ = db.save_route(&crate::database::dao::routes::Route {
                        alias: m.id.to_string(),
                        description: format!("{} — imported from cc-switch", m.display_name),
                        hops: vec![new_hop.clone()],
                    });
                }
                Err(_) => {
                    // Best-effort: don't fail the whole import over a
                    // single route collision.
                }
            }
            imported_routes.push(ImportedRoute {
                alias: m.id.to_string(),
                provider: pp_id.clone(),
                model: model_name,
            });
        }
    }

    Ok(ImportReport {
        source_db,
        imported,
        skipped_existing,
        total_in_source,
        imported_routes,
    })
}

/// True if `settings_config` contains any URL field that resolves to the
/// AAG proxy listen address (e.g. `127.0.0.1:8989`). Used by
/// `insert_aag_providers` to break the cc-switch ↔ AAG self-loop when
/// the user has registered AAG itself as a provider inside cc-switch.
fn entry_points_at_aag(settings: &serde_json::Value, aag_listen: &str) -> bool {
    if aag_listen.is_empty() {
        return false;
    }
    // URL fields commonly seen across cc-switch app_type schemas:
    //   Claude Code  : env.ANTHROPIC_BASE_URL
    //   Codex        : providers.<id>.base_url, top-level base_url / baseUrl
    //   Gemini       : env.GOOGLE_GEMINI_BASE_URL
    //   Grok build   : env.GROK_API_BASE / GROK_BASE_URL
    // We walk the JSON tree and match any string field whose value
    // contains the AAG listen address (host:port). Substring match is
    // intentional — a value like "http://127.0.0.1:8989/v1" or
    // "http://localhost:8989" both count, and a stray digit "8989" in
    // an unrelated field has no URL semantics to mis-flag.
    let needle = aag_listen;
    fn visit(v: &serde_json::Value, needle: &str, hit: &mut bool) {
        match v {
            serde_json::Value::String(s) => {
                if s.contains(needle) {
                    *hit = true;
                }
            }
            serde_json::Value::Array(a) => {
                for item in a {
                    if *hit {
                        return;
                    }
                    visit(item, needle, hit);
                }
            }
            serde_json::Value::Object(o) => {
                for (_k, val) in o {
                    if *hit {
                        return;
                    }
                    visit(val, needle, hit);
                }
            }
            _ => {}
        }
    }
    let mut hit = false;
    visit(settings, needle, &mut hit);
    hit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use rusqlite::Connection;

    fn write_fake_cc_switch_db(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                name TEXT NOT NULL,
                settings_config TEXT NOT NULL,
                website_url TEXT,
                category TEXT,
                created_at INTEGER,
                sort_index INTEGER,
                notes TEXT,
                icon TEXT,
                icon_color TEXT,
                meta TEXT NOT NULL DEFAULT '{}',
                is_current BOOLEAN NOT NULL DEFAULT 0,
                in_failover_queue BOOLEAN NOT NULL DEFAULT 0,
                PRIMARY KEY (id, app_type)
            )",
        )
        .unwrap();
        // Two providers: one will be skipped (claude_code already in AAG),
        // one will be imported.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, is_current, in_failover_queue) \
             VALUES ('existing-claude', 'claude_code', 'Pre-existing', '{\"env\":{}}', 0, 0)",
            [],
        )
        .unwrap();
        // Use cc-switch's native app_type ids ("claude" not "claude_code")
        // so the mapping code path is exercised.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, is_current, in_failover_queue) \
             VALUES ('imported-codex', 'codex', 'Imported Codex', '{\"auth\":{},\"config\":\"model=\\\"gpt-5\\\"\"}', 1, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, is_current, in_failover_queue) \
             VALUES ('imported-grok', 'grokbuild', 'Imported Grok', '{}', 0, 1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn missing_db_is_noop() {        let aag = Database::memory().unwrap();
        let report = import_from_db(
            &aag,
            std::path::Path::new("/tmp/definitely-does-not-exist-aag-test.db"),
        )
        .unwrap();
        assert_eq!(report.total_in_source, 0);
        assert!(report.imported.is_empty());
        assert!(report.skipped_existing.is_empty());
    }

    #[test]
    fn missing_providers_table_errors() {
        let aag = Database::memory().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_noprov_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        // Create a SQLite file with only a different table.
        let conn = Connection::open(&tmp).unwrap();
        conn.execute_batch("CREATE TABLE something_else (x INTEGER)")
            .unwrap();
        drop(conn);

        let err = import_from_db(&aag, &tmp).unwrap_err();
        assert!(
            err.to_string().contains("no `providers` table"),
            "expected schema probe error, got: {err}"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn import_inserts_missing_and_skips_existing() {
        let aag = Database::memory().unwrap();
        // Seed AAG with one provider that overlaps the source.
        aag.save_provider(
            "claude_code",
            &Provider::new(
                "existing-claude".into(),
                "Pre-existing".into(),
                serde_json::json!({"env": {}}),
            ),
        )
        .unwrap();

        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_real_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        write_fake_cc_switch_db(&tmp);

        let report = import_from_db(&aag, &tmp).unwrap();
        assert_eq!(report.total_in_source, 3);
        // existing-claude was already in AAG → skipped.
        assert_eq!(report.skipped_existing.len(), 1);
        assert_eq!(report.skipped_existing[0].id, "existing-claude");
        // imported-codex + imported-grok → imported.
        assert_eq!(report.imported.len(), 2);
        let ids: Vec<&str> = report.imported.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"imported-codex"));
        assert!(ids.contains(&"imported-grok"));

        // Verify AAG now has the rows.
        let codex = aag
            .get_provider_by_id("imported-codex", "codex")
            .unwrap()
            .expect("imported-codex should exist");
        assert_eq!(codex.name, "Imported Codex");
        assert_eq!(
            codex.settings_config["config"].as_str(),
            Some("model=\"gpt-5\"")
        );

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn reimporting_is_idempotent() {
        let aag = Database::memory().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_idem_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        write_fake_cc_switch_db(&tmp);

        let r1 = import_from_db(&aag, &tmp).unwrap();
        let r2 = import_from_db(&aag, &tmp).unwrap();
        assert_eq!(r1.imported.len(), 3);
        assert_eq!(r2.imported.len(), 0);
        assert_eq!(r2.skipped_existing.len(), 3);

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn aag_self_loop_entry_is_skipped() {
        let aag = Database::memory().unwrap();
        // Database::memory runs the seed, which writes a default proxy_config
        // at 127.0.0.1:8989. Any cc-switch entry whose settings_config points
        // there must be dropped before insertion — pulling AAG back into its
        // own queue would route traffic through itself in a loop.
        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_selfloop_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let conn = Connection::open(&tmp).unwrap();
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                name TEXT NOT NULL,
                settings_config TEXT NOT NULL,
                PRIMARY KEY (id, app_type)
            )",
        )
        .unwrap();
        // AAG itself — the entry cc-switch would create when the user adds
        // AAG as an upstream provider.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config) VALUES \
             ('aag-self', 'claude', 'AI Agent Gateway', \
              '{\"env\":{\"ANTHROPIC_BASE_URL\":\"http://127.0.0.1:8989\",\"ANTHROPIC_AUTH_TOKEN\":\"\"}}')",
            [],
        )
        .unwrap();
        // A normal upstream — must still import.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config) VALUES \
             ('real-upstream', 'claude', 'Real Upstream', \
              '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://api.example.com\",\"ANTHROPIC_AUTH_TOKEN\":\"x\"}}')",
            [],
        )
        .unwrap();
        // A loose 8989 in a non-URL field — must still import (the detector
        // walks all string fields but a base URL is what matters in practice;
        // the only "8989" we drop is one inside a URL-shaped value).
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config) VALUES \
             ('unrelated', 'claude', 'Unrelated', \
              '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://api.real.com\",\"ANTHROPIC_AUTH_TOKEN\":\"token-8989\"}}')",
            [],
        )
        .unwrap();
        drop(conn);

        let report = import_from_db(&aag, &tmp).unwrap();
        let ids: Vec<&str> = report.imported.iter().map(|r| r.id.as_str()).collect();
        assert!(
            !ids.contains(&"aag-self"),
            "AAG self entry must not be imported, got: {ids:?}"
        );
        assert!(ids.contains(&"real-upstream"));
        assert!(ids.contains(&"unrelated"));
        // Verify AAG did not end up with the self entry.
        assert!(
            aag.get_provider_by_id("aag-self", "claude_code")
                .unwrap()
                .is_none(),
            "aag-self must not have been written to the providers table"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// Each imported Claude provider's `ANTHROPIC_DEFAULT_*_MODEL` env
    /// keys should auto-seed failover routes for every official Claude
    /// model id. The hop count = number of official models × number of
    /// imported Claude providers (when every tier key is present).
    #[test]
    fn import_seeds_routes_from_anthropic_default_models() {
        let aag = Database::memory().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_seedrt_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let conn = Connection::open(&tmp).unwrap();
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                name TEXT NOT NULL,
                settings_config TEXT NOT NULL,
                PRIMARY KEY (id, app_type)
            )",
        )
        .unwrap();
        // A Claude provider with full per-tier mapping. cc-switch's
        // `claude` app_type is mapped to AAG `claude_code`.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config) VALUES \
             ('zhipu', 'claude', 'Zhipu GLM', \
              '{\"env\":{\
                 \"ANTHROPIC_BASE_URL\":\"https://open.bigmodel.cn/api/anthropic\",\
                 \"ANTHROPIC_AUTH_TOKEN\":\"x\",\
                 \"ANTHROPIC_DEFAULT_HAIKU_MODEL\":\"glm-5.2\",\
                 \"ANTHROPIC_DEFAULT_SONNET_MODEL\":\"glm-5.2[1M]\",\
                 \"ANTHROPIC_DEFAULT_OPUS_MODEL\":\"glm-5.3[1M]\",\
                 \"ANTHROPIC_DEFAULT_FABLE_MODEL\":\"glm-5.3\"\
              }}')",
            [],
        )
        .unwrap();
        drop(conn);

        let report = import_from_db(&aag, &tmp).unwrap();
        assert_eq!(report.imported.len(), 1);

        // OFFICIAL_MODELS has 7 entries (fable-5 + opus-4-8/7/6 + sonnet-5/4-6 + haiku-4-5)
        // and each one should now have a route seeded with a single hop
        // pointing at the imported `ccs-zhipu` proxy provider.
        let routes = aag.get_all_routes().unwrap();
        assert_eq!(
            routes.len(),
            crate::proxy::models::OFFICIAL_MODELS.len(),
            "expected one route per official model, got {} routes",
            routes.len()
        );
        for r in &routes {
            assert_eq!(r.hops.len(), 1, "route {} should have 1 hop", r.alias);
            assert_eq!(r.hops[0].provider, "ccs-zhipu");
            let expected = match r.alias.as_str() {
                "claude-haiku-4-5" => "glm-5.2",
                "claude-sonnet-5" | "claude-sonnet-4-6" => "glm-5.2[1M]",
                "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => "glm-5.3[1M]",
                "claude-fable-5" => "glm-5.3",
                _ => panic!("unexpected alias {}", r.alias),
            };
            assert_eq!(
                r.hops[0].model, expected,
                "wrong model for route {}",
                r.alias
            );
        }

        // imported_routes report field must mirror what we just wrote.
        assert_eq!(report.imported_routes.len(), crate::proxy::models::OFFICIAL_MODELS.len());
        let _ = std::fs::remove_file(&tmp);
    }

    /// If a route already exists for an official model, a re-import
    /// must prepend the new provider as the head hop rather than
    /// clobbering the existing chain (preserves user-tuned order).
    #[test]
    fn reimport_prepends_new_provider_to_existing_route() {
        let aag = Database::memory().unwrap();
        // Seed an existing user-tuned route: opus tier hops to provider-A first.
        aag.save_route(&crate::database::dao::routes::Route {
            alias: "claude-opus-4-8".into(),
            description: "user-tuned".into(),
            hops: vec![
                crate::database::dao::routes::RouteHop {
                    provider: "existing-a".into(),
                    model: "user-model-a".into(),
                },
                crate::database::dao::routes::RouteHop {
                    provider: "existing-b".into(),
                    model: "user-model-b".into(),
                },
            ],
        })
        .unwrap();

        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_reimport_rt_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let conn = Connection::open(&tmp).unwrap();
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                name TEXT NOT NULL,
                settings_config TEXT NOT NULL,
                PRIMARY KEY (id, app_type)
            )",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config) VALUES \
             ('newp', 'claude', 'New', \
              '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://x\",\"ANTHROPIC_AUTH_TOKEN\":\"k\",\
                \"ANTHROPIC_DEFAULT_OPUS_MODEL\":\"newp-opus\"}}')",
            [],
        )
        .unwrap();
        drop(conn);

        let report = import_from_db(&aag, &tmp).unwrap();
        assert_eq!(report.imported.len(), 1);

        // The opus-4-8 route must now have 3 hops with `ccs-newp` first.
        let route = aag.get_route("claude-opus-4-8").unwrap().unwrap();
        assert_eq!(route.hops.len(), 3);
        assert_eq!(route.hops[0].provider, "ccs-newp");
        assert_eq!(route.hops[0].model, "newp-opus");
        assert_eq!(route.hops[1].provider, "existing-a");
        assert_eq!(route.hops[2].provider, "existing-b");

        // Re-importing must be idempotent (head hop unchanged, no extra hops).
        let r2 = import_from_db(&aag, &tmp).unwrap();
        assert_eq!(r2.imported.len(), 0, "nothing should import on re-run");
        let route2 = aag.get_route("claude-opus-4-8").unwrap().unwrap();
        assert_eq!(route2.hops.len(), 3);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn malformed_source_row_is_reported() {
        let aag = Database::memory().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_malformed_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let conn = Connection::open(&tmp).unwrap();
        conn.execute_batch(
            "CREATE TABLE providers (
                id BLOB,
                app_type TEXT,
                name TEXT,
                settings_config BLOB
            )",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config) VALUES (X'00', 'claude', 'Malformed', '{}')",
            [],
        )
        .unwrap();
        drop(conn);

        let err = import_from_db(&aag, &tmp).unwrap_err();
        assert!(err.to_string().contains("row decode"), "unexpected error: {err}");
        let _ = std::fs::remove_file(&tmp);
    }

    /// End-to-end test against the user's real cc-switch SQLite DB.
    /// Gated behind `AAG_TEST_CCSWITCH_DB` env var so CI / other devs
    /// don't accidentally hit a 27 MB personal DB. Run with:
    ///
    ///   AAG_TEST_CCSWITCH_DB="/c/Users/ydsun/.cc-switch/cc-switch.db" \
    ///     cargo test --lib -- --test-threads=1 real_cc_switch
    ///
    /// Prints the full report so the user can sanity-check the import
    /// against their actual provider catalogue.
    #[test]
    fn real_cc_switch_db_smoke_test() {
        let Ok(path) = std::env::var("AAG_TEST_CCSWITCH_DB") else {
            eprintln!(
                "real_cc_switch_db_smoke_test skipped: set \
                 AAG_TEST_CCSWITCH_DB=/path/to/cc-switch.db to enable"
            );
            return;
        };
        let path = std::path::PathBuf::from(&path);
        if !path.exists() {
            panic!("AAG_TEST_CCSWITCH_DB points at non-existent file: {}", path.display());
        }
        let aag = Database::memory().unwrap();
        let report = import_from_db(&aag, &path).expect("import_from_db failed");

        eprintln!(
            "\n=== real_cc_switch_db_smoke_test ===\n\
             source: {}\n\
             total in source: {}\n\
             imported:        {}\n\
             skipped:         {}",
            report.source_db,
            report.total_in_source,
            report.imported.len(),
            report.skipped_existing.len(),
        );
        eprintln!("\n--- imported ---");
        for r in &report.imported {
            eprintln!("  + [{:>14}] {:<32} ({})", r.app_type, r.name, r.id);
        }
        eprintln!("\n--- skipped (already in AAG) ---");
        for r in &report.skipped_existing {
            eprintln!("  = [{:>14}] {:<32} ({})", r.app_type, r.name, r.id);
        }
    }
}

/// Extract upstream (base_url, api_key, provider_type) from a cc-switch
/// provider's settings_config for creating a proxy_provider entry.
/// Returns None if no usable URL is found.
fn extract_upstream_from_settings(
    app_type: &str,
    cfg: &serde_json::Value,
) -> Option<(String, String, String)> {
    let env = cfg.get("env").cloned().unwrap_or(serde_json::json!({}));
    match app_type {
        "claude_code" | "claude" => {
            let base = env.get("ANTHROPIC_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() { return None; }
            let key = env.get("ANTHROPIC_AUTH_TOKEN").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Some((base, key, "anthropic".into()))
        }
        "codex" => {
            let base = cfg.get("baseUrl").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                .or_else(|| {
                    cfg.get("config").and_then(|v| v.as_str()).and_then(|toml| {
                        toml.lines().find_map(|l| {
                            let (k, v) = l.trim().split_once('=')?;
                            if k.trim() == "base_url" { Some(v.trim().trim_matches('"')) } else { None }
                        })
                    })
                })?.to_string();
            let key = cfg.get("apiKey").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Some((base, key, "openai".into()))
        }
        "copilot" => {
            let base = env.get("COPILOT_PROVIDER_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() { return None; }
            let key = env.get("COPILOT_PROVIDER_API_KEY").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let wire = env.get("COPILOT_PROVIDER_TYPE").and_then(|v| v.as_str()).unwrap_or("openai");
            let pt = if wire == "anthropic" { "anthropic" } else { "openai" };
            Some((base, key, pt.into()))
        }
        "pi" => {
            let base = cfg.get("baseUrl").and_then(|v| v.as_str()).filter(|s| !s.is_empty())?.to_string();
            let key = cfg.get("apiKey").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let pt = match cfg.get("api").and_then(|v| v.as_str()).unwrap_or("") {
                "anthropic-messages" => "anthropic",
                _ => "openai",
            };
            Some((base, key, pt.into()))
        }
        _ => None,
    }
}
