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

use crate::database::dao::proxy_providers::CircuitBreakerConfig;
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
    #[serde(default)]
    pub skipped_no_key: Vec<ImportedProvider>,
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
            "SELECT id, app_type, name, settings_config, website_url, category,
                    created_at, sort_index, notes, icon, icon_color, meta,
                    is_current, in_failover_queue
             FROM providers
             ORDER BY is_current DESC, in_failover_queue DESC, sort_index ASC, id ASC",
        )
        .map_err(|e| AppError::Database(format!("cc-switch DB prepare failed: {e}")))?;
    let row_results = stmt
        .query_map([], |r| {
            Ok(CcSwitchRow {
                id: r.get(0)?,
                app_type: map_app_type(&r.get::<_, String>(1)?),
                name: r.get(2)?,
                settings_config: r.get(3)?,
                website_url: r.get(4)?,
                category: r.get(5)?,
                created_at: r.get(6)?,
                sort_index: r.get(7)?,
                notes: r.get(8)?,
                icon: r.get(9)?,
                icon_color: r.get(10)?,
                is_current: r.get::<_, i64>(12)? != 0,
                in_failover_queue: r.get::<_, i64>(13)? != 0,
            })
        })
        .map_err(|e| AppError::Database(format!("cc-switch DB query failed: {e}")))?;
    row_results
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Database(format!("cc-switch DB row decode failed: {e}")))
}

/// Internal: rows as read from cc-switch's DB. `app_type` already
/// mapped onto AAG's snake_case. (cc-switch's `meta` JSON column is
/// selected but not carried over — AAG builds its own ProviderMeta.)
struct CcSwitchRow {
    id: String,
    app_type: String,
    name: String,
    settings_config: String,
    website_url: Option<String>,
    category: Option<String>,
    created_at: Option<i64>,
    sort_index: Option<usize>,
    notes: Option<String>,
    icon: Option<String>,
    icon_color: Option<String>,
    is_current: bool,
    in_failover_queue: bool,
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
    let mut skipped_no_key: Vec<ImportedProvider> = Vec::new();
    // Successfully-imported Claude providers, kept by value so the
    // post-loop route builder can read their env block. Other app_types
    // don't contribute ANTHROPIC_* mapping, so they're not tracked.
    let mut imported_claude: Vec<(String, serde_json::Value, bool)> = Vec::new();
    // Successfully-imported Codex providers + their upstream model list
    // (from `modelCatalog.models[].model`), for the codex route builder.
    let mut imported_codex: Vec<(String, Vec<String>)> = Vec::new();

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
        // Codex providers without an API key (ChatGPT-login `codex-official`,
        // `default`, etc.) can't be used for forwarding — skip them entirely.
        if row.app_type == "codex" {
            let key_ok = match extract_upstream_from_settings(&row.app_type, &settings_value) {
                Some((_, key, _)) => !key.is_empty(),
                None => false,
            };
            if !key_ok {
                log::info!(
                    "import_cc_switch: skipping codex provider {} ({}) — no API key",
                    row.id, row.name
                );
                skipped_no_key.push(ImportedProvider {
                    id: row.id,
                    app_type: row.app_type,
                    name: row.name,
                });
                continue;
            }
        }
        let provider = Provider {
            id: row.id.clone(),
            name: row.name.clone(),
            settings_config: settings_value.clone(),
            website_url: row.website_url.clone(),
            category: row.category.clone(),
            created_at: row.created_at,
            sort_index: row.sort_index,
            notes: row.notes.clone(),
            icon: row.icon.clone(),
            icon_color: row.icon_color.clone(),
            meta: Some(crate::provider::ProviderMeta {
                custom_endpoints: Default::default(),
            }),
            in_failover_queue: row.in_failover_queue,
            sync_locked: false,
        };
        let existing = db.get_provider_by_id(&row.id, &row.app_type).map_err(AppError::from)?;
        let mut collect_for_routes = true;
        match existing {
            Some(p) if respect_sync_locked && p.sync_locked => {
                // Manually locked — leave alone; don't rebuild routes either.
                collect_for_routes = false;
            }
            Some(_) => {
                skipped_existing.push(ImportedProvider {
                    id: row.id.clone(),
                    app_type: row.app_type.clone(),
                    name: row.name.clone(),
                });
            }
            None => {
                db.save_provider(&row.app_type, &provider).map_err(AppError::from)?;
                // cc-switch's `is_current` bit becomes AAG's CURRENT marker so
                // the chain head in the UI matches what cc-switch had selected.
                if row.is_current {
                    let _ = db.set_current_provider(&row.id, &row.app_type);
                }

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
                            // Imported providers see a mix of upstream
                            // health (many return HTTP 4xx for specific
                            // model ids that aren't in their plan). Tighten
                            // the breaker to 3 failures / 15 s cooldown so
                            // a consistently-broken upstream opens quickly
                            // and is skipped on subsequent requests — this
                            // bounds the streaming failover walk cost to
                            // ~5 s once the bad hops are open.
                            circuit_breaker: CircuitBreakerConfig {
                                failure_threshold: 3,
                                cooldown_ms: 15_000,
                                half_open_retries: 1,
                                success_threshold: 1,
                            },
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
            }
        }
        // Collect route inputs regardless of whether the provider was newly
        // imported or already present, so a re-import fills in hop models for
        // providers that predate the route builder. Kept in cc-switch queue
        // order (the SELECT ORDER BY's current+queue+sort_index). Locked rows
        // (respect_sync_locked + sync_locked) are excluded.
        if collect_for_routes && row.app_type == "claude_code" {
            imported_claude.push((row.id.clone(), settings_value.clone(), row.in_failover_queue));
        }
        if collect_for_routes && row.app_type == "codex" {
            // Codex providers' upstream model list from
            // `modelCatalog.models[].model` for the codex route builder.
            let models: Vec<String> = settings_value
                .get("modelCatalog")
                .and_then(|mc| mc.get("models"))
                .and_then(|m| m.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| e.get("model").and_then(|v| v.as_str()))
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            if !models.is_empty() {
                imported_codex.push((row.id.clone(), models));
            }
        }
    }

    // Auto-create failover routes for official Claude models using each
    // newly imported provider's `ANTHROPIC_DEFAULT_*_MODEL` env keys as
    // the per-hop model. cc-switch stores per-tier mapping in the JSON
    // `env` block of each provider's `settings_config`; we round-trip
    // that into AAG's route format:
    //
    //   claude-fable-5      -> env.ANTHROPIC_DEFAULT_FABLE_MODEL
    //   claude-opus-4-8/7/6 -> env.ANTHROPIC_DEFAULT_OPUS_MODEL
    //   claude-sonnet-5/-4-6-> env.ANTHROPIC_DEFAULT_SONNET_MODEL
    //   claude-haiku-4-5    -> env.ANTHROPIC_DEFAULT_HAIKU_MODEL
    //
    // Fallback rule (per user direction): if a tier has no explicit
    // mapping, ANY other tier's model substitutes for it — e.g. a vendor
    // that only maps `mimo-v2.5-pro[1M]` → fable-5 gets `mimo-v2.5-pro[1M]`
    // for haiku too. The `[1M]` context suffix is stripped at import time —
    // upstream Anthropic-compatible endpoints reject it as not-a-real-model.
    // The 1M-context capability is preserved separately: the Anthropic
    // provider injects `anthropic-beta: context-1m-2025-08-07` whenever
    // the original `env.ANTHROPIC_MODEL` carried `[1M]` (see
    // `services/provider/claude_code.rs` live-snapshot writer).
    //
    // Hops are built in cc-switch queue order (current first, then
    // sort_index), so the chain head matches what cc-switch had active.
    let mut imported_routes: Vec<ImportedRoute> = Vec::new();

    /// Resolve a provider's per-tier upstream model from its env block,
    /// applying the any-tier-substitutes fallback. The `[1M]` context marker
    /// is stripped — upstream Anthropic-compatible endpoints reject it as
    /// not-a-real-model and either 404 or return an empty SSE body. The
    /// 1M-context capability is preserved through a different mechanism
    /// (the `anthropic-beta: context-1m-2025-08-07` header) injected by
    /// the Anthropic provider, not the model id.
    fn tier_models(env: &serde_json::Map<String, serde_json::Value>) -> [Option<String>; 4] {
        let pick = |keys: &[&str]| -> Option<String> {
            for k in keys {
                if let Some(s) = env.get(*k).and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        return Some(crate::proxy::strip_one_m_suffix(s));
                    }
                }
            }
            None
        };
        let default_model = pick(&["ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_MODEL"]);
        let haiku = pick(&["ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_SMALL_FAST_MODEL"]);
        let sonnet = pick(&["ANTHROPIC_DEFAULT_SONNET_MODEL"]);
        let opus = pick(&["ANTHROPIC_DEFAULT_OPUS_MODEL"]);
        let fable = pick(&["ANTHROPIC_DEFAULT_FABLE_MODEL"]);
        // Any available model can substitute for a missing tier.
        let fallback = fable
            .clone()
            .or_else(|| opus.clone())
            .or_else(|| sonnet.clone())
            .or_else(|| haiku.clone())
            .or_else(|| default_model.clone());
        [
            haiku.or_else(|| fallback.clone()),
            sonnet.or_else(|| fallback.clone()),
            opus.or_else(|| fallback.clone()),
            fable.or_else(|| fallback.clone()),
        ]
    }

    // Per-imported-provider tier mapping: [haiku, sonnet, opus, fable].
    // Only providers flagged `in_failover_queue` in cc-switch contribute
    // hops — the queue is what cc-switch itself fails over through.
    let mut provider_tiers: Vec<(String, [Option<String>; 4])> = Vec::new();
    for (ccs_id, settings_value, in_queue) in &imported_claude {
        if !in_queue {
            log::info!(
                "import_cc_switch: {} in_failover_queue=0 — imported but not in failover chain",
                ccs_id
            );
            continue;
        }
        let env = settings_value
            .as_object()
            .and_then(|o| o.get("env"))
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let tiers = tier_models(&env);
        // Skip providers with no usable model at all.
        if tiers.iter().all(|m| m.is_none()) {
            continue;
        }
        provider_tiers.push((ccs_id.clone(), tiers));
    }

    if !provider_tiers.is_empty() {
        for m in crate::proxy::models::OFFICIAL_MODELS {
            let tier_idx = match m.id {
                "claude-haiku-4-5" => 0usize,
                "claude-sonnet-5" | "claude-sonnet-4-6" => 1,
                _ if m.id.starts_with("claude-opus-") => 2,
                _ => 3, // claude-fable-5
            };
            let new_hops: Vec<crate::database::dao::routes::RouteHop> = provider_tiers
                .iter()
                .filter_map(|(ccs_id, tiers)| {
                    tiers[tier_idx].as_ref().map(|model| crate::database::dao::routes::RouteHop {
                        provider: format!("ccs-{}", ccs_id),
                        model: model.clone(),
                    })
                })
                .collect();
            if new_hops.is_empty() {
                continue;
            }
            upsert_route_merge(
                db,
                m.id,
                format!("{} — imported from cc-switch", m.display_name),
                new_hops.clone(),
            );
            for h in &new_hops {
                imported_routes.push(ImportedRoute {
                    alias: m.id.to_string(),
                    provider: h.provider.clone(),
                    model: h.model.clone(),
                });
            }
        }
    }

    // Codex route builder: each imported codex provider's upstream models
    // are evenly distributed across the default Codex client models
    // (codex-5-6-sol / codex-5-6-luna / codex-5-6-terra / codex-5-5).
    // client[i] maps to upstream[i*N/4], so:
    //   N=1 → all four client models use the single upstream model
    //   N=2 → first two use m0, last two use m1 (two per group)
    //   N≥3 → spread across as many upstream models as available
    let codex_client_ids: Vec<&str> =
        crate::proxy::models::CODEX_MODELS.iter().map(|m| m.id).collect();
    // Build hops per client model — ALL providers at once (in cc-switch queue
    // order) so the chain head matches the queue. Processing one provider at
    // a time would reverse the order (each merge prepends to the head).
    for (i, cm) in codex_client_ids.iter().enumerate() {
        let new_hops: Vec<crate::database::dao::routes::RouteHop> = imported_codex
            .iter()
            .filter_map(|(ccs_id, upstream_models)| {
                let n = upstream_models.len();
                if n == 0 {
                    return None;
                }
                let up_idx = (i * n) / codex_client_ids.len();
                Some(crate::database::dao::routes::RouteHop {
                    provider: format!("ccs-{}", ccs_id),
                    model: upstream_models[up_idx].clone(),
                })
            })
            .collect();
        if new_hops.is_empty() {
            continue;
        }
        upsert_route_merge(
            db,
            cm,
            format!("{} — imported from cc-switch", cm),
            new_hops.clone(),
        );
        for h in &new_hops {
            imported_routes.push(ImportedRoute {
                alias: cm.to_string(),
                provider: h.provider.clone(),
                model: h.model.clone(),
            });
        }
    }

    Ok(ImportReport {
        source_db,
        imported,
        skipped_existing,
        skipped_no_key,
        total_in_source,
        imported_routes,
    })
}

/// Upsert a route: create it with `new_hops` if absent; otherwise prepend
/// any providers from `new_hops` not already in the chain (preserving
/// user-tuned hop order). Best-effort — never fails the import.
fn upsert_route_merge(
    db: &Database,
    alias: &str,
    description: String,
    new_hops: Vec<crate::database::dao::routes::RouteHop>,
) {
    match db.get_route(alias) {
        Ok(Some(existing)) => {
            let existing_ids: std::collections::HashSet<String> =
                existing.hops.iter().map(|h| h.provider.clone()).collect();
            let mut hops: Vec<crate::database::dao::routes::RouteHop> = Vec::new();
            for h in &new_hops {
                if existing_ids.contains(&h.provider) {
                    continue;
                }
                hops.push(h.clone());
            }
            hops.extend(existing.hops.into_iter());
            let _ = db.save_route(&crate::database::dao::routes::Route {
                alias: alias.to_string(),
                // Preserve any user-tuned description on merge.
                description: existing.description.clone(),
                hops,
            });
        }
        Ok(None) => {
            let _ = db.save_route(&crate::database::dao::routes::Route {
                alias: alias.to_string(),
                description,
                hops: new_hops,
            });
        }
        Err(_) => {
            // Best-effort: don't fail the whole import over a single
            // route collision.
        }
    }
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
    use std::sync::Arc;

    /// Full cc-switch `providers` table schema (mirrors the real DB the
    /// importer reads). Every fake DB in these tests uses this so the
    /// expanded SELECT (all metadata columns) succeeds.
    const FAKE_CC_SWITCH_SCHEMA: &str = "CREATE TABLE providers (
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
    )";

    fn write_fake_cc_switch_db(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
        // Two providers: one will be skipped (claude_code already in AAG),
        // one will be imported.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, is_current, in_failover_queue) \
             VALUES ('existing-claude', 'claude_code', 'Pre-existing', '{\"env\":{}}', 0, 0)",
            [],
        )
        .unwrap();
        // Use cc-switch's native app_type ids ("claude" not "claude_code")
        // so the mapping code path is exercised. This codex provider has a
        // valid key + base_url so it survives the no-key filter and imports.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, is_current, in_failover_queue) \
             VALUES ('imported-codex', 'codex', 'Imported Codex', \
              '{\"auth\":{\"OPENAI_API_KEY\":\"sk-fake\"},\"config\":\"base_url = \\\"https://api.fake.com\\\"\\nmodel=\\\"gpt-5\\\"\"}', 1, 0)",
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
        assert!(
            codex.settings_config["config"].as_str().unwrap_or("").contains("gpt-5"),
            "settings_config should be preserved verbatim"
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
        conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
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
        conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
        // A Claude provider with full per-tier mapping. cc-switch's
        // `claude` app_type is mapped to AAG `claude_code`.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
             ('zhipu', 'claude', 'Zhipu GLM', \
              '{\"env\":{\
                 \"ANTHROPIC_BASE_URL\":\"https://open.bigmodel.cn/api/anthropic\",\
                 \"ANTHROPIC_AUTH_TOKEN\":\"x\",\
                 \"ANTHROPIC_DEFAULT_HAIKU_MODEL\":\"glm-5.2\",\
                 \"ANTHROPIC_DEFAULT_SONNET_MODEL\":\"glm-5.2[1M]\",\
                 \"ANTHROPIC_DEFAULT_OPUS_MODEL\":\"glm-5.3[1M]\",\
                 \"ANTHROPIC_DEFAULT_FABLE_MODEL\":\"glm-5.3\"\
              }}', 1)",
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
                "claude-sonnet-5" | "claude-sonnet-4-6" => "glm-5.2",
                "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => "glm-5.3",
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

    /// Codex import: upstream models from `modelCatalog.models[].model` are
    /// evenly distributed across the default Codex client models, and
    /// providers without an API key (ChatGPT-login / default) are skipped.
    #[test]
    fn import_codex_distributes_models_and_skips_no_key() {
        let aag = Database::memory().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_codex_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let conn = Connection::open(&tmp).unwrap();
        conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
        // One upstream model → all four client models use it.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, sort_index, in_failover_queue) VALUES \
             ('codex-a', 'codex', 'Codex A', \
              '{\"auth\":{\"OPENAI_API_KEY\":\"key-a\"},\
                \"config\":\"base_url = \\\"https://api.a.com\\\"\\nwire_api = \\\"responses\\\"\\n\",\
                \"modelCatalog\":{\"models\":[{\"model\":\"glm-5.2\"}]}}', 0, 1)",
            [],
        )
        .unwrap();
        // Two upstream models → two-and-two (Sol+Terra→m0, Luna+5.5→m1).
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, sort_index, in_failover_queue) VALUES \
             ('codex-b', 'codex', 'Codex B', \
              '{\"auth\":{\"OPENAI_API_KEY\":\"key-b\"},\
                \"config\":\"base_url = \\\"https://api.b.com/anthropic\\\"\\n\",\
                \"modelCatalog\":{\"models\":[{\"model\":\"m0\"},{\"model\":\"m1\"}]}}', 1, 1)",
            [],
        )
        .unwrap();
        // No API key (ChatGPT-login) → must be skipped entirely.
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, sort_index, in_failover_queue) VALUES \
             ('codex-nokey', 'codex', 'Codex NoKey', \
              '{\"auth\":{\"OPENAI_API_KEY\":null},\
                \"config\":\"base_url = \\\"https://api.nokey.com\\\"\\n\",\
                \"modelCatalog\":{\"models\":[{\"model\":\"x\"}]}}', 2, 1)",
            [],
        )
        .unwrap();
        drop(conn);

        let report = import_from_db(&aag, &tmp).unwrap();
        assert_eq!(report.imported.len(), 2, "two codex providers with keys import");
        assert_eq!(report.skipped_no_key.len(), 1, "no-key provider is skipped");
        assert_eq!(report.skipped_no_key[0].id, "codex-nokey");
        assert!(
            aag.get_provider_by_id("codex-nokey", "codex").unwrap().is_none(),
            "no-key codex provider must not be stored"
        );

        // Client-model → upstream mapping per the even-distribution rule
        // (two upstream models → Sol+Luna→m0, Terra+5.5→m1).
        let expected: &[(&str, &[&str])] = &[
            ("codex-5-6-sol", &["glm-5.2", "m0"]),
            ("codex-5-6-luna", &["glm-5.2", "m0"]),
            ("codex-5-6-terra", &["glm-5.2", "m1"]),
            ("codex-5-5", &["glm-5.2", "m1"]),
        ];
        for (alias, models) in expected {
            let route = aag.get_route(alias).unwrap().unwrap_or_else(|| panic!("route {} missing", alias));
            let hops: Vec<(String, String)> = route.hops.iter().map(|h| (h.provider.clone(), h.model.clone())).collect();
            assert_eq!(
                hops,
                vec![
                    ("ccs-codex-a".into(), models[0].to_string()),
                    ("ccs-codex-b".into(), models[1].to_string()),
                ],
                "route {alias} hop mapping wrong"
            );
        }

        // Anthropic-compatible base_url → anthropic proxy_provider type.
        let pp_b = aag.get_all_proxy_providers().unwrap()
            .into_iter().find(|p| p.id == "ccs-codex-b").expect("ccs-codex-b exists");
        assert_eq!(pp_b.provider_type, "anthropic", "codex-b has /anthropic base_url");
        let pp_a = aag.get_all_proxy_providers().unwrap()
            .into_iter().find(|p| p.id == "ccs-codex-a").expect("ccs-codex-a exists");
        assert_eq!(pp_a.provider_type, "openai");
        assert_eq!(pp_a.api_key, "key-a");

        let _ = std::fs::remove_file(&tmp);
    }

    /// A re-import where every provider already exists in AAG (so none is
    /// newly inserted) must STILL build/repair the failover routes — this is
    /// the "already imported before the route builder existed" upgrade path.
    #[test]
    fn reimport_fills_hops_for_existing_providers() {
        let aag = Database::memory().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "aag_ccswitch_repair_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let conn = Connection::open(&tmp).unwrap();
        conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
             ('deepseek', 'claude', 'DeepSeek', \
              '{\"env\":{\
                 \"ANTHROPIC_BASE_URL\":\"https://api.deepseek.com/anthropic\",\
                 \"ANTHROPIC_AUTH_TOKEN\":\"x\",\
                 \"ANTHROPIC_DEFAULT_FABLE_MODEL\":\"deepseek-v4-pro[1M]\"\
              }}', 1)",
            [],
        )
        .unwrap();
        drop(conn);

        // First import builds providers + routes.
        let r1 = import_from_db(&aag, &tmp).unwrap();
        assert_eq!(r1.imported.len(), 1);
        let fable = aag.get_route("claude-fable-5").unwrap().expect("fable route");
        assert_eq!(fable.hops[0].provider, "ccs-deepseek");
        // The `[1M]` context marker is stripped — upstream endpoints reject
        // it as not-a-real-model; the Anthropic provider injects the
        // 1M-context beta header instead.
        assert_eq!(fable.hops[0].model, "deepseek-v4-pro");

        // Simulate pre-route-builder state: wipe the fable route, then
        // re-import. Every provider is now a skip, but the route must be
        // rebuilt with the hop model filled from the provider's mapping.
        aag.delete_route("claude-fable-5").expect("delete route");
        let r2 = import_from_db(&aag, &tmp).unwrap();
        assert_eq!(r2.imported.len(), 0, "nothing new to insert on re-import");
        let fable2 = aag.get_route("claude-fable-5").unwrap().expect("fable route repaired");
        assert_eq!(fable2.hops[0].provider, "ccs-deepseek");
        assert_eq!(
            fable2.hops[0].model, "deepseek-v4-pro",
            "re-import must fill the hop model from cc-switch's tier mapping ([1M] stripped)"
        );

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
        conn.execute_batch(FAKE_CC_SWITCH_SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, in_failover_queue) VALUES \
             ('newp', 'claude', 'New', \
              '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://x\",\"ANTHROPIC_AUTH_TOKEN\":\"k\",\
                \"ANTHROPIC_DEFAULT_OPUS_MODEL\":\"newp-opus\"}}', 1)",
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
                settings_config BLOB,
                website_url TEXT,
                category TEXT,
                created_at INTEGER,
                sort_index INTEGER,
                notes TEXT,
                icon TEXT,
                icon_color TEXT,
                meta TEXT,
                is_current BOOLEAN,
                in_failover_queue BOOLEAN
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
        eprintln!("\n--- skipped (no API key — codex login/default) ---");
        for r in &report.skipped_no_key {
            eprintln!("  x [{:>14}] {:<32} ({})", r.app_type, r.name, r.id);
        }
        // The ChatGPT-login codex providers must have been filtered out.
        assert!(
            report.skipped_no_key.iter().any(|r| r.id == "codex-official"),
            "codex-official (login) must be skipped"
        );
        assert!(
            report.skipped_no_key.iter().any(|r| r.id == "default"),
            "default-chatgpt must be skipped"
        );

        // ── Codex client-model routes: every default codex model got a chain
        // from the imported providers' modelCatalog, and the hops preserve
        // cc-switch queue order (MiniMax-style head = first import). ──
        eprintln!("\n--- codex routes (client model -> hops) ---");
        let codex_route_aliases = [
            "codex-5-6-sol", "codex-5-6-luna", "codex-5-6-terra", "codex-5-5",
        ];
        for alias in codex_route_aliases {
            let route = aag.get_route(alias).expect("get_route")
                .unwrap_or_else(|| panic!("codex route {} missing after import", alias));
            eprintln!("  {}:", alias);
            for (i, h) in route.hops.iter().enumerate() {
                eprintln!("    hop[{i}] {:<32} -> {}", h.provider, h.model);
            }
            assert!(
                !route.hops.is_empty(),
                "codex route {} should have hops",
                alias
            );
            assert!(
                route.hops.iter().all(|h| !h.model.is_empty()),
                "codex route {} must have a mapped upstream model per hop",
                alias
            );
        }

        // ── Verify per-tier model mappings for every imported Claude provider ──
        eprintln!("\n--- per-tier model mapping (haiku/sonnet/opus/fable) ---");
        let claude_imported: Vec<&str> = report
            .imported
            .iter()
            .filter(|r| r.app_type == "claude_code")
            .map(|r| r.id.as_str())
            .collect();
        assert!(
            claude_imported.len() >= 4,
            "expected several claude providers from cc-switch, got {}",
            claude_imported.len()
        );
        // Queue members → (id, fable model), for the verbatim-hop check below.
        let mut fable_by_provider: Vec<(String, String)> = Vec::new();
        for id in &claude_imported {
            let prov = aag
                .get_provider_by_id(id, "claude_code")
                .expect("get_provider")
                .expect("provider exists");
            let env = prov
                .settings_config
                .get("env")
                .and_then(|e| e.as_object())
                .cloned()
                .unwrap_or_default();
            let pick = |k: &str| env.get(k).and_then(|v| v.as_str()).unwrap_or("");
            let fable_val = pick("ANTHROPIC_DEFAULT_FABLE_MODEL");
            eprintln!(
                "  {:<24} haiku={:<20} sonnet={:<20} opus={:<20} fable={}",
                id,
                pick("ANTHROPIC_DEFAULT_HAIKU_MODEL"),
                pick("ANTHROPIC_DEFAULT_SONNET_MODEL"),
                pick("ANTHROPIC_DEFAULT_OPUS_MODEL"),
                fable_val,
            );
            // Queue members (in_failover_queue=1) must have their fable
            // mapping present verbatim (the [1M] suffix preserved).
            if prov.in_failover_queue && !fable_val.is_empty() {
                fable_by_provider.push((id.to_string(), fable_val.to_string()));
            }
        }

        // ── Every queue member's fable mapping appears in the claude-fable-5
        // route hop (chain head = cc-switch current), with the local `[1M]`
        // context marker stripped (upstream endpoints reject it). ──
        let fable_route = aag.get_route("claude-fable-5").expect("get_route")
            .expect("claude-fable-5 route should exist after import");
        for (id, fable_val) in &fable_by_provider {
            let pp_id = format!("ccs-{}", id);
            let hop = fable_route.hops.iter().find(|h| h.provider == pp_id)
                .unwrap_or_else(|| panic!("route claude-fable-5 missing hop for {pp_id}"));
            let expected = crate::proxy::strip_one_m_suffix(fable_val);
            assert_eq!(
                &hop.model, &expected,
                "claude-fable-5 hop for {id} must use cc-switch's model id (without [1M])"
            );
        }

        // ── Verify routes were built with imported providers in queue order ──
        eprintln!("\n--- failover routes (alias -> hops) ---");
        let routes = aag.get_all_routes().expect("get_all_routes");
        let imported_pp_ids: Vec<String> = claude_imported
            .iter()
            .map(|id| format!("ccs-{}", id))
            .collect();
        for alias in [
            "claude-fable-5",
            "claude-opus-4-8",
            "claude-sonnet-5",
            "claude-haiku-4-5",
        ] {
            let route = aag.get_route(alias).expect("get_route").unwrap_or_else(|| {
                panic!("route {} should exist after import", alias)
            });
            eprintln!("  {}:", alias);
            for (i, h) in route.hops.iter().enumerate() {
                eprintln!("    hop[{i}] {:<32} -> {}", h.provider, h.model);
            }
            // The imported cc-s providers must appear in the SAME order as
            // the cc-switch queue (current first, then sort_index). The
            // mock seed hops trail after the real providers.
            let ccs_in_chain: Vec<&str> = route
                .hops
                .iter()
                .filter(|h| h.provider.starts_with("ccs-"))
                .map(|h| h.provider.as_str())
                .collect();
            assert!(
                !ccs_in_chain.is_empty(),
                "route {} should contain imported cc-s providers",
                alias
            );
            // imported_pp_ids is in SELECT ORDER BY is_current, sort_index
            // order, so the chain must list them in the same relative order.
            let expected: Vec<&str> = imported_pp_ids.iter().map(|s| s.as_str()).collect();
            let filtered_expected: Vec<&str> = expected
                .iter()
                .filter(|id| ccs_in_chain.contains(id))
                .copied()
                .collect();
            assert_eq!(
                ccs_in_chain, filtered_expected,
                "route {} hops must preserve cc-switch queue order",
                alias
            );
        }

        // ── imported_routes report must cover every official model × provider ──
        assert!(!report.imported_routes.is_empty(), "imported_routes should be populated");
        eprintln!("\n--- imported_routes report ({} entries) ---", report.imported_routes.len());
        for ir in report.imported_routes.iter().take(14) {
            eprintln!("    {} <- {}/{}", ir.alias, ir.provider, ir.model);
        }

        // ── Build a Router from the imported data and verify every official
        // model resolves to a chain of registered providers (no 404 at route
        // resolution time). Deterministic — no upstream requests are sent. ──
        let mut providers: std::collections::HashMap<
            String,
            Arc<dyn crate::proxy::providers::Provider>,
        > = std::collections::HashMap::new();
        for pp in aag.get_all_proxy_providers().expect("get_all_proxy_providers") {
            match crate::proxy::providers::build_provider(&pp) {
                Ok(p) => {
                    providers.insert(pp.id.clone(), Arc::from(p));
                }
                Err(e) => eprintln!("  [warn] could not build provider {}: {e}", pp.id),
            }
        }
        let route_map: std::collections::HashMap<String, Vec<crate::database::dao::routes::RouteHop>> =
            routes
                .iter()
                .map(|r| (r.alias.clone(), r.hops.clone()))
                .collect();
        let router = crate::proxy::router::Router::new(providers.clone(), route_map);
        eprintln!("\n--- route resolution (SURFACE_CLAUDE) ---");
        for m in crate::proxy::models::OFFICIAL_MODELS {
            let hops = router.resolve_route_for(crate::proxy::router::SURFACE_CLAUDE, m.id)
                .unwrap_or_else(|| panic!("official model {} must resolve after import", m.id));
            let head = &hops[0];
            assert!(
                providers.contains_key(&head.provider),
                "route {} head provider {} must be registered",
                m.id, head.provider
            );
            eprintln!("  {:<20} -> {} hops, head {} / {}", m.id, hops.len(), head.provider, head.model);
        }
    }

    /// Real end-to-end failover: send one minimal completion through the
    /// imported chain and observe which upstream actually serves it. Gated on
    /// `AAG_TEST_CCSWITCH_DB` (like the smoke test above). `max_tokens=8` so
    /// the real upstream call is effectively free. The router walks hops in
    /// queue order; failing upstreams are skipped via the circuit breaker and
    /// the next hop is tried. Run with `RUST_LOG=warn` to see per-hop
    /// `router: FAIL` lines.
    #[tokio::test]
    async fn real_cc_switch_failover_request() {
        let Ok(path) = std::env::var("AAG_TEST_CCSWITCH_DB") else {
            eprintln!("real_cc_switch_failover_request skipped: set AAG_TEST_CCSWITCH_DB");
            return;
        };
        let path = std::path::PathBuf::from(&path);
        if !path.exists() {
            panic!("AAG_TEST_CCSWITCH_DB points at non-existent file: {}", path.display());
        }
        let aag = Database::memory().unwrap();
        let _ = import_from_db(&aag, &path).expect("import_from_db failed");

        let mut providers: std::collections::HashMap<String, Arc<dyn crate::proxy::providers::Provider>> =
            std::collections::HashMap::new();
        let mut build_errors: Vec<String> = Vec::new();
        for pp in aag.get_all_proxy_providers().expect("get_all_proxy_providers") {
            match crate::proxy::providers::build_provider(&pp) {
                Ok(p) => { providers.insert(pp.id.clone(), Arc::from(p)); }
                Err(e) => build_errors.push(format!("{}: {e}", pp.id)),
            }
        }
        let route_map: std::collections::HashMap<String, Vec<crate::database::dao::routes::RouteHop>> =
            aag.get_all_routes().expect("get_all_routes")
                .into_iter()
                .map(|r| (r.alias, r.hops))
                .collect();
        let router = crate::proxy::router::Router::new(providers, route_map);

        let req = crate::proxy::canonical::ChatCompletionRequest {
            model: "claude-fable-5".into(),
            messages: vec![crate::proxy::canonical::ChatMessage {
                role: "user".into(),
                content: serde_json::json!("hi"),
                tool_calls: None,
                tool_call_id: None,
                extra: Default::default(),
            }],
            stream: false,
            temperature: None,
            max_tokens: Some(8),
            tools: None,
            tool_choice: None,
            extra: Default::default(),
        };
        eprintln!("\n=== real_cc_switch_failover_request ===\n  (build errors: {})", if build_errors.is_empty() { "none".into() } else { build_errors.join("; ") });
        // Claude chain (SURFACE_CLAUDE /v1/messages): claude-fable-5.
        eprintln!("  model=claude-fable-5 (SURFACE_CLAUDE)");
        match tokio::time::timeout(
            std::time::Duration::from_secs(180),
            router.complete_for(crate::proxy::router::SURFACE_CLAUDE, "claude_code", &req),
        ).await {
            Ok(Ok(resp)) => {
                let text = resp.choices[0].message.content.as_deref().unwrap_or("");
                let model = &resp.model;
                eprintln!("  SERVED: model={} len={} preview={:?}", model, text.len(), &text.chars().take(60).collect::<String>());
            }
            Ok(Err(e)) => {
                eprintln!("  EXHAUSTED: {} (status={:?}, retriable={})", e.message, e.status, e.retriable);
            }
            Err(_) => {
                eprintln!("  TIMEOUT after 180s — chain hung");
            }
        }
        // Codex chain (SURFACE_CODEX /v1/responses): codex-5-6-sol maps to
        // each imported provider's modelCatalog via the even-distribution
        // rule. Verifies the direct-route forwarding mapping end-to-end.
        let codex_req = crate::proxy::canonical::ChatCompletionRequest {
            model: "codex-5-6-sol".into(),
            messages: req.messages.clone(),
            stream: false,
            temperature: None,
            max_tokens: Some(8),
            tools: None,
            tool_choice: None,
            extra: Default::default(),
        };
        eprintln!("  model=codex-5-6-sol (SURFACE_CODEX)");
        match tokio::time::timeout(
            std::time::Duration::from_secs(180),
            router.complete_for(crate::proxy::router::SURFACE_CODEX, "codex", &codex_req),
        ).await {
            Ok(Ok(resp)) => {
                let text = resp.choices[0].message.content.as_deref().unwrap_or("");
                let model = &resp.model;
                eprintln!("  SERVED: model={} len={} preview={:?}", model, text.len(), &text.chars().take(60).collect::<String>());
            }
            Ok(Err(e)) => {
                eprintln!("  EXHAUSTED: {} (status={:?}, retriable={})", e.message, e.status, e.retriable);
            }
            Err(_) => {
                eprintln!("  TIMEOUT after 180s — codex chain hung");
            }
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
            // cc-switch stores the key under `auth.OPENAI_API_KEY` (a
            // top-level `apiKey` is unusual). ChatGPT-login providers
            // (codex-official / default) have a null key — the empty
            // string is how the importer detects and skips them.
            let key = cfg
                .get("auth")
                .and_then(|a| a.get("OPENAI_API_KEY"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| cfg.get("apiKey").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            // A base_url containing `anthropic` is an Anthropic-compatible
            // endpoint (e.g. MiMo / Zhipu `/anthropic`) — route those
            // through the Anthropic provider, everything else OpenAI.
            let pt = if base.contains("anthropic") { "anthropic" } else { "openai" };
            Some((base, key, pt.into()))
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
