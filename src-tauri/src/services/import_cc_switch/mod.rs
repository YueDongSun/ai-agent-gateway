//! Import provider rows from a cc-switch SQLite database.
//!
//! cc-switch (https://github.com/farion1231/cc-switch) stores its
//! provider catalogue in `~/.cc-switch/cc-switch.db` — a SQLite file
//! whose `providers` table is schema-compatible with AAG's (same
//! column order and types, see
//! `~/Documents/.../cc-switch-src/src-tauri/src/database/schema.rs:27`).
//!
//! Module layout:
//! - [`cc_switch_db`]: read-only access to the external DB (rows,
//!   app_type mapping, path resolution).
//! - [`settings_extract`]: pure `settings_config` parsing (upstream
//!   coordinates, extra headers, self-loop and subscription checks).
//! - [`row_builders`]: derive AAG pool rows and failover routes from
//!   parsed cc-switch data.
//! - this module: the import orchestrator and the UI-facing report
//!   types.
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

pub(crate) mod cc_switch_db;
mod import_policy;
mod maintenance;
mod preview;
pub(crate) use maintenance::maintain_imported_suppliers;
mod row_builders;
pub(crate) mod settings_extract;
mod supplier_import;
mod reset_import;
mod screening;
pub use screening::{screen_import, ScreeningImportOutcome};
pub(crate) use screening::candidate_by_id;
pub use reset_import::{preview_reset_import, apply_reset_import, SupplierResetImportResult};

#[cfg(test)]
mod supplier_import_tests;
#[cfg(test)]
mod screening_tests;
#[cfg(test)]
mod tests;

use crate::database::Database;
use crate::error::AppError;
use crate::provider::Provider;
use serde::Serialize;
use std::collections::HashMap;

#[cfg(test)]
use cc_switch_db::{map_app_type, map_cc_switch_icon, map_cc_switch_icon_color};
use cc_switch_db::{read_cc_switch_providers, CcSwitchRow};
#[cfg(test)]
use settings_extract::extract_extra_headers_from_settings;
use settings_extract::{
    base_host_is_official_anthropic, codex_catalog_models, extract_upstream_from_settings,
    settings_env,
};

pub use cc_switch_db::cc_switch_db_path;
pub use preview::{
    SupplierImportPendingEntry, SupplierImportPlanEntry, SupplierImportPreview,
    SupplierImportProblem,
};
pub use supplier_import::SupplierImportApplyResult;
pub use supplier_import::safe_source_ref as subscription_source_ref;

/// Whether every currently valid source entry is already represented exactly.
/// This performs only reads and never returns credential or endpoint values.
pub fn source_is_materialized(db: &Database, source_path: &std::path::Path) -> Result<bool, AppError> {
    supplier_import::source_is_materialized(db, source_path)
}

/// Build the redacted, side-effect-free import preview for the UI.
pub fn preview_import<P: AsRef<std::path::Path>>(
    db: &Database,
    cc_switch_path: P,
) -> Result<SupplierImportPreview, AppError> {
    preview::build_preview(db, cc_switch_path.as_ref())
}

/// Apply a previewed plan in one transaction. See
/// [`SupplierImportApplyResult`] for the idempotent result shape.
pub fn apply_import<P: AsRef<std::path::Path>>(
    db: &Database,
    cc_switch_path: P,
    plan_id: &str,
) -> Result<SupplierImportApplyResult, AppError> {
    supplier_import::apply_import(db, cc_switch_path.as_ref(), plan_id)
}

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
    /// New rows force-added to the failover queue because their upstream
    /// was usable (auto-queue: a usable import needs no manual tuning).
    #[serde(default)]
    pub queued_providers: Vec<ImportedProvider>,
    /// Subscription rows (official `api.anthropic.com` base + empty key)
    /// imported as `anthropic_passthrough` pool rows instead of static-key
    /// upstreams.
    #[serde(default)]
    pub passthrough_providers: Vec<ImportedProvider>,
}

/// One-shot full import: all providers (any app_type) from cc-switch.
/// UI uses this via the "Import from cc-switch" button.
pub fn import_from_db<P: AsRef<std::path::Path>>(
    db: &Database,
    cc_switch_path: P,
) -> Result<ImportReport, AppError> {
    if crate::services::provider_admission::enforcement_enabled(db) {
        return Err(AppError::InvalidInput("supplier_import_screening_required".into()));
    }
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
    if crate::services::provider_admission::enforcement_enabled(db) {
        return Err(AppError::InvalidInput("supplier_import_screening_required".into()));
    }
    let all = read_cc_switch_providers(cc_switch_path.as_ref())?;
    // Rows arrive with app_type already mapped to AAG ids
    // (cc_switch_db::map_app_type runs inside read_cc_switch_providers), so
    // the auto-sync filter must match the mapped value: filtering on
    // cc-switch's native "claude" id here matches nothing and silently
    // turns auto-sync into a no-op.
    let rows: Vec<CcSwitchRow> = all
        .into_iter()
        .filter(|r| r.app_type == "claude_code")
        .collect();
    insert_aag_providers(db, rows, /*respect_sync_locked=*/ true)
}

/// Eagerly extract a Claude row's [haiku, sonnet, opus, fable] tier models
/// from its env block (borrowed, not cloned) so the route builder works
/// from the tiers alone and the import never retains the settings tree.
fn route_tiers(settings_value: &serde_json::Value) -> [Option<String>; 4] {
    row_builders::tier_models(settings_env(settings_value))
}

/// The import orchestrator: classify each cc-switch row, insert new
/// provider + pool rows, collect route inputs in queue order, then let
/// [`row_builders::build_import_routes`] derive the failover chains.
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
    // proxy listen address and drop it before insertion. The coarse
    // settings-tree rule lives with the strict URL rule in
    // `proxy::self_loop` so the two cannot drift.
    let aag_listen = db.get_proxy_config().ok().map(|c| (c.host, c.port));

    let mut imported: Vec<ImportedProvider> = Vec::new();
    let mut skipped_existing: Vec<ImportedProvider> = Vec::new();
    let mut skipped_no_key: Vec<ImportedProvider> = Vec::new();
    // New rows force-added to the failover queue (usable upstream found).
    let mut queued_providers: Vec<ImportedProvider> = Vec::new();
    // Subscription rows imported as `anthropic_passthrough` pool rows.
    let mut passthrough_providers: Vec<ImportedProvider> = Vec::new();
    // Successfully-imported Claude providers with their eagerly-extracted
    // [haiku, sonnet, opus, fable] tier models, so the post-loop route
    // builder never retains the full settings tree. Other app_types don't
    // contribute ANTHROPIC_* mapping, so they're not tracked.
    let mut imported_claude: Vec<(String, [Option<String>; 4], bool)> = Vec::new();
    // Successfully-imported Codex providers + their upstream model list
    // (from `modelCatalog.models[].model`), for the codex route builder.
    let mut imported_codex: Vec<(String, Vec<String>)> = Vec::new();
    // Imported gemini providers with a GEMINI_MODEL (alias, upstream,
    // modelMapping), for the gemini route builder.
    let mut imported_gemini: Vec<(String, String, HashMap<String, String>)> = Vec::new();

    for row in rows {
        let settings_value: serde_json::Value = serde_json::from_str(&row.settings_config)
            .map_err(|e| {
                AppError::Database(format!(
                    "cc-switch provider {}/{}: invalid settings_config JSON: {e}",
                    row.id, row.app_type
                ))
            })?;
        if let Some((listen_host, listen_port)) = &aag_listen {
            if !listen_host.is_empty()
                && crate::proxy::self_loop::settings_target_self(
                    &settings_value,
                    listen_host,
                    *listen_port,
                )
            {
                log::warn!(
                    "import_cc_switch: skipping {}/{} ({}) — settings_config targets AAG itself at {}:{}",
                    row.id,
                    row.app_type,
                    row.name,
                    listen_host,
                    listen_port
                );
                continue;
            }
        }
        // Classify the upstream once per row: the codex no-key filter and
        // the new-row classification below share this extraction.
        let upstream =
            extract_upstream_from_settings(&row.app_type, &settings_value, row.meta.as_ref());
        let (policy_base, policy_key) = upstream
            .as_ref()
            .map(|(base, key, _)| (base.as_str(), key.as_str()))
            .unwrap_or(("", ""));
        if import_policy::excludes_account_auth(&row, &settings_value, policy_base, policy_key) {
            skipped_no_key.push(ImportedProvider {
                id: row.id,
                app_type: row.app_type,
                name: String::new(),
            });
            continue;
        }
        // Codex providers without an API key (ChatGPT-login `codex-official`,
        // `default`, etc.) can't be used for forwarding — skip them entirely.
        if row.app_type == "codex" {
            let key_ok = upstream.as_ref().is_some_and(|(_, key, _)| !key.is_empty());
            if !key_ok {
                log::info!(
                    "import_cc_switch: skipping codex provider {} ({}) — no API key",
                    row.id,
                    row.name
                );
                skipped_no_key.push(ImportedProvider {
                    id: row.id,
                    app_type: row.app_type,
                    name: row.name,
                });
                continue;
            }
        }
        let existing = db.get_provider_by_id(&row.id, &row.app_type)?;
        let mut collect_for_routes = true;
        // The new-row arm collects its own route inputs below (with the
        // effective queue bit); the shared collection block then only
        // serves existing/locked rows.
        let mut collected_in_new_arm = false;
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
                // Subscription rows (official api.anthropic.com base +
                // empty key) become passthrough pool rows; everything else
                // with a usable upstream (the hoisted `upstream` above) is
                // force-added to the failover queue — an import with a
                // working upstream needs no manual queue tuning.
                let is_subscription = row.app_type == "claude_code"
                    && upstream.as_ref().is_some_and(|(base, key, _)| {
                        key.is_empty() && base_host_is_official_anthropic(base)
                    });
                let in_queue = if is_subscription {
                    // Passthrough rows ride the anthropic_passthrough pool
                    // path — they are not static failover hops.
                    false
                } else {
                    row.in_failover_queue || upstream.is_some()
                };
                let provider = Provider {
                    id: row.id.clone(),
                    name: row.name.clone(),
                    settings_config: settings_value.clone(),
                    website_url: row.website_url.clone(),
                    category: row.category.clone(),
                    created_at: row.created_at,
                    sort_index: row.sort_index,
                    notes: row.notes.clone(),
                    icon: cc_switch_db::map_cc_switch_icon(row.icon.as_deref()),
                    icon_color: cc_switch_db::map_cc_switch_icon_color(row.icon_color.as_deref()),
                    meta: Some(crate::provider::ProviderMeta {
                        custom_endpoints: Default::default(),
                    }),
                    in_failover_queue: in_queue,
                    sync_locked: false,
                };
                db.save_provider(&row.app_type, &provider)?;
                if !row.in_failover_queue && in_queue {
                    queued_providers.push(ImportedProvider {
                        id: row.id.clone(),
                        app_type: row.app_type.clone(),
                        name: row.name.clone(),
                    });
                }
                // cc-switch's `is_current` bit becomes AAG's CURRENT marker so
                // the chain head in the UI matches what cc-switch had selected.
                if row.is_current {
                    db.set_current_provider(&row.id, &row.app_type)?;
                }

                // Also create a corresponding proxy_provider so the imported
                // entry appears in the upstream pool (Claude page). This lets
                // users configure model mappings and failover directly.
                let pp_id = format!("ccs-{}", row.id);
                let existing_pp = db.get_proxy_provider(&pp_id)?.is_some();
                if !existing_pp {
                    // The pool-row builder derives models + model_mapping
                    // from the row's own model metadata so the imported
                    // upstream is usable out of the box.
                    if let Some(pp) = row_builders::pool_row_for_import(
                        &pp_id,
                        &row,
                        &settings_value,
                        upstream.as_ref(),
                        is_subscription,
                    ) {
                        db.save_proxy_provider(&pp).map_err(|error| {
                            AppError::Database(format!(
                                "cc-switch provider {}/{}: pool row write failed: {error}",
                                row.id, row.app_type
                            ))
                        })?;
                    }
                    if is_subscription {
                        passthrough_providers.push(ImportedProvider {
                            id: row.id.clone(),
                            app_type: row.app_type.clone(),
                            name: row.name.clone(),
                        });
                    }
                }

                imported.push(ImportedProvider {
                    id: row.id.clone(),
                    app_type: row.app_type.clone(),
                    name: row.name.clone(),
                });
                // Route collection uses the effective queue bit for new
                // rows so force-queued imports contribute hops too.
                if row.app_type == "claude_code" {
                    imported_claude.push((row.id.clone(), route_tiers(&settings_value), in_queue));
                }
                if row.app_type == "codex" {
                    let models = codex_catalog_models(&settings_value);
                    if !models.is_empty() {
                        imported_codex.push((row.id.clone(), models));
                    }
                }
                if row.app_type == "gemini" {
                    // Borrow the env block; cloning it per row only to read
                    // GEMINI_MODEL wasted an allocation per gemini import.
                    let env = settings_env(&settings_value);
                    let gemini_model = env
                        .get("GEMINI_MODEL")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if !gemini_model.is_empty() {
                        let model_mapping: HashMap<String, String> = settings_value
                            .get("modelMapping")
                            .and_then(|m| m.as_object())
                            .map(|m| {
                                m.iter()
                                    .filter_map(|(k, v)| {
                                        v.as_str().map(|s| (k.clone(), s.to_string()))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        imported_gemini.push((row.id.clone(), gemini_model, model_mapping));
                    }
                }
                collected_in_new_arm = true;
            }
        }
        // Collect route inputs regardless of whether the provider was newly
        // imported or already present, so a re-import fills in hop models for
        // providers that predate the route builder. Kept in cc-switch queue
        // order (the SELECT ORDER BY's current+queue+sort_index). Locked rows
        // (respect_sync_locked + sync_locked) are excluded.
        if !collected_in_new_arm && collect_for_routes && row.app_type == "claude_code" {
            imported_claude.push((
                row.id.clone(),
                route_tiers(&settings_value),
                row.in_failover_queue,
            ));
        }
        if !collected_in_new_arm && collect_for_routes && row.app_type == "codex" {
            // Codex providers' upstream model list from
            // `modelCatalog.models[].model` for the codex route builder.
            let models = codex_catalog_models(&settings_value);
            if !models.is_empty() {
                imported_codex.push((row.id.clone(), models));
            }
        }
    }

    let imported_routes = row_builders::build_import_routes(
        db,
        &imported_claude,
        &imported_codex,
        &imported_gemini,
        &passthrough_providers,
    )?;

    Ok(ImportReport {
        source_db,
        imported,
        skipped_existing,
        skipped_no_key,
        total_in_source,
        imported_routes,
        queued_providers,
        passthrough_providers,
    })
}
