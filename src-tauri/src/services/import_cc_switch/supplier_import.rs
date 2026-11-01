//! Supplier-import apply: one transactional write of a previewed plan.
//!
//! The plan id is self-certifying: it binds the source fingerprint, the
//! target configuration generation, and the derived entry digest. Apply
//! re-reads the read-only source, re-derives the same plan, refuses any
//! drift (`supplier_import_source_changed` / `supplier_import_plan_mismatch`
//! / `supplier_import_stale_preview`), and commits every row — suppliers,
//! endpoints, models, and legacy projections — in a single transaction.
//! A repeated apply of an already-materialized plan writes nothing and
//! reports `alreadyApplied`.
//!
//! The projection SQL mirrors `database::dao::suppliers` (which has no
//! batch entry point yet); keep the two in sync until a DAO-level batch
//! API exists.

use crate::database::dao::suppliers::{
    self as supplier_dao, SupplierEndpointAlternativeRecord, SupplierModelRecord, SupplierRecord,
};
use crate::database::Database;
use crate::error::AppError;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::preview::{
    derive_plan, derived_digest12, parse_plan_id, plan_already_applied, read_source_rows,
    DerivedPlan, PlannedSupplier,
};
use crate::services::suppliers::SupplierProvenance;

const PROJECTION_DOMAIN: &str = "supplier-projection-v1";
const SOURCE_KIND: &str = "cc_switch";

/// Exact, read-only comparison used by operator previews. Unlike UI change
/// labels this includes credentials, variants, models, and projected icons.
pub(super) fn source_is_materialized(
    db: &Database,
    source_path: &std::path::Path,
) -> Result<bool, AppError> {
    let rows = read_source_rows(source_path)?;
    let (_, existing) = db.get_supplier_snapshot()?;
    let mut derived = derive_plan(&rows, &existing, db)?;
    super::screening::filter_admitted(db, &mut derived, &existing)?;
    Ok(plan_already_applied(
        &derived.entries,
        &derived.pending,
        &existing,
        &proxy_provider_icon_state(db)?,
    ))
}

/// Result of `supplier_import_apply`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierImportApplyResult {
    pub plan_id: String,
    pub applied_generation: u64,
    pub already_applied: bool,
    pub created_count: u32,
    pub refreshed_count: u32,
    pub pending_unsupported_count: u32,
    pub problem_count: u32,
    pub excluded_oauth_count: usize,
}

/// Apply a previously previewed plan. `source_path` must be the same
/// source the preview read: any change to it invalidates the plan.
pub(crate) fn apply_import(
    db: &Database,
    source_path: &std::path::Path,
    plan_id: &str,
) -> Result<SupplierImportApplyResult, AppError> {
    let binding = parse_plan_id(plan_id)?;
    let rows = read_source_rows(source_path)?;
    let (generation, existing) = db.get_supplier_snapshot()?;
    let mut derived = derive_plan(&rows, &existing, db)?;

    if derived.source_fingerprint.get(..12) != Some(binding.fingerprint12.as_str()) {
        return Err(AppError::Message("supplier_import_source_changed".into()));
    }
    if derived_digest12(&derived.entries, &derived.pending) != binding.digest12 {
        return Err(AppError::Message("supplier_import_plan_mismatch".into()));
    }
    super::screening::filter_admitted(db, &mut derived, &existing)?;

    // Content-based idempotency runs before the generation check so a
    // repeated apply of an already-materialized plan is a stable no-op
    // even though the first apply advanced the generation.
    let created_before = count_uncreated(&derived.entries, &existing);
    let proxy_provider_icons = proxy_provider_icon_state(db)?;
    if plan_already_applied(
        &derived.entries,
        &derived.pending,
        &existing,
        &proxy_provider_icons,
    ) {
        return Ok(SupplierImportApplyResult {
            plan_id: plan_id.to_string(),
            applied_generation: generation,
            already_applied: true,
            created_count: 0,
            refreshed_count: 0,
            pending_unsupported_count: derived.pending.len() as u32,
            problem_count: derived.problems.len() as u32,
            excluded_oauth_count: derived.excluded_oauth_count,
        });
    }
    if generation != binding.target_generation {
        return Err(AppError::Message("supplier_import_stale_preview".into()));
    }
    if derived.entries.is_empty() && derived.pending.is_empty() {
        // Nothing to materialize (every source row is reported as a
        // problem): a successful no-op that touches no generation.
        return Ok(SupplierImportApplyResult {
            plan_id: plan_id.to_string(),
            applied_generation: generation,
            already_applied: false,
            created_count: 0,
            refreshed_count: 0,
            pending_unsupported_count: 0,
            problem_count: derived.problems.len() as u32,
            excluded_oauth_count: derived.excluded_oauth_count,
        });
    }

    validate_connection_refresh(&derived.entries, &existing)?;
    let changed_entries: Vec<_> = derived
        .entries
        .iter()
        .filter(|entry| {
            !plan_already_applied(
                std::slice::from_ref(*entry),
                &[],
                &existing,
                &proxy_provider_icons,
            )
        })
        .cloned()
        .collect();
    apply_transaction(db, &derived, &changed_entries, &existing, generation)?;
    Ok(SupplierImportApplyResult {
        plan_id: plan_id.to_string(),
        applied_generation: generation + 1,
        already_applied: false,
        created_count: created_before,
        refreshed_count: changed_entries
            .iter()
            .map(|entry| entry.supplier_id.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len() as u32
            - created_before,
        pending_unsupported_count: derived.pending.len() as u32,
        problem_count: derived.problems.len() as u32,
        excluded_oauth_count: derived.excluded_oauth_count,
    })
}

fn count_uncreated(entries: &[PlannedSupplier], existing: &[SupplierRecord]) -> u32 {
    entries
        .iter()
        .filter(|entry| !existing.iter().any(|record| record.id == entry.supplier_id))
        .map(|entry| entry.supplier_id.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len() as u32
}

/// Current display icons on the legacy `proxy_providers` rows, keyed by
/// row id: the persisted projection state that `plan_already_applied`
/// compares the planned icons against. A plan only counts as
/// materialized when the rows its projection would write already carry
/// the planned icons.
pub(super) fn proxy_provider_icon_state(
    db: &Database,
) -> Result<std::collections::HashMap<String, (Option<String>, Option<String>)>, AppError> {
    let guard = crate::database::lock_conn!(db.conn);
    let mut stmt = guard.prepare("SELECT id, icon, icon_color FROM proxy_providers")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            (
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ),
        ))
    })?;
    let mut state = std::collections::HashMap::new();
    for row in rows {
        let (id, icons) =
            row.map_err(|e| AppError::Database(format!("icon state read failed: {e}")))?;
        state.insert(id, icons);
    }
    Ok(state)
}

/// Mirror of `database::dao::suppliers::projection_id_for` (private
/// there). Deterministic legacy proxy-provider id for one endpoint.
pub(crate) fn projection_id_for(endpoint_id: &str) -> String {
    format!(
        "supplier_{}",
        stable_digest(PROJECTION_DOMAIN, &[endpoint_id])
    )
}

fn stable_digest(domain: &str, parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain.as_bytes());
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    hex::encode(&digest.finalize()[..16])
}

pub fn safe_source_ref(app_type: &str, source_id: &str) -> String {
    let fits = |value: &str| {
        !value.is_empty()
            && value.chars().count() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'+')
            })
    };
    let candidate = format!("{app_type}:{source_id}");
    if fits(&candidate) {
        candidate
    } else {
        format!(
            "{app_type}:h{}",
            stable_digest("cc-switch-ref-v1", &[app_type, source_id])
        )
    }
}

fn provenance_value(
    app_type: &str,
    source_id: &str,
    endpoint_ref: Option<&str>,
    import_batch_ref: &str,
) -> Result<serde_json::Value, AppError> {
    let provenance = SupplierProvenance {
        source_kind: Some(SOURCE_KIND.to_string()),
        source_record_ref: Some(safe_source_ref(app_type, source_id)),
        source_app_type: Some(app_type.to_string()),
        source_endpoint_ref: endpoint_ref.map(str::to_string),
        import_batch_ref: Some(import_batch_ref.to_string()),
        fetched_at: None,
        content_version: None,
        confidence: None,
    };
    serde_json::to_value(provenance).map_err(AppError::Json)
}

fn pending_provenance_value(
    app_type: &str,
    source_id: &str,
    import_batch_ref: &str,
) -> Result<serde_json::Value, AppError> {
    let provenance = SupplierProvenance {
        source_kind: Some("official_auth_pending".to_string()),
        source_record_ref: Some(safe_source_ref(app_type, source_id)),
        source_app_type: Some(app_type.to_string()),
        source_endpoint_ref: None,
        import_batch_ref: Some(import_batch_ref.to_string()),
        fetched_at: None,
        content_version: None,
        confidence: None,
    };
    serde_json::to_value(provenance).map_err(AppError::Json)
}

/// A source edit to authentication/transport is a separate revalidation
/// operation. Refuse before the transaction instead of carrying old discovery
/// claims into a new connection. The caller retains its pre-import snapshot.
fn validate_connection_refresh(
    entries: &[PlannedSupplier],
    existing: &[SupplierRecord],
) -> Result<(), AppError> {
    for entry in entries {
        let Some(record) = existing
            .iter()
            .find(|record| record.id == entry.supplier_id)
        else {
            continue;
        };
        let headers: std::collections::HashMap<_, _> = entry.headers.iter().cloned().collect();
        let changed = record.api_key != entry.api_key
            || record.base_url != entry.supplier_base_url
            || record
                .endpoints
                .iter()
                .find(|endpoint| endpoint.protocol == entry.protocol.supplier_protocol())
                .is_some_and(|endpoint| {
                    endpoint
                        .endpoint_url
                        .as_deref()
                        .filter(|url| !url.is_empty())
                        .unwrap_or(&record.base_url)
                        != entry.base_url
                        || endpoint
                            .api_key_override
                            .as_deref()
                            .unwrap_or(&record.api_key)
                            != entry.api_key
                        || endpoint.extra_headers != headers
                        || entry.alternatives.iter().any(|planned| {
                            endpoint.alternatives.iter().any(|saved| {
                                saved.base_url == planned.base_url
                                    && (saved.api_key != planned.api_key
                                        || saved.extra_headers
                                            != planned.headers.iter().cloned().collect())
                            })
                        })
                });
        if changed {
            return Err(AppError::Message(
                "supplier_import_endpoint_change_requires_review".into(),
            ));
        }
    }
    Ok(())
}

fn preserved_variant_models(
    existing: &[SupplierModelRecord],
    planned: &[String],
    endpoint_id: &str,
) -> Vec<SupplierModelRecord> {
    let mut models: Vec<_> = existing
        .iter()
        .filter(|model| {
            !super::preview::import_owns_model(model) || planned.contains(&model.model_id)
        })
        .cloned()
        .collect();
    for (index, id) in planned.iter().enumerate() {
        if let Some(model) = models.iter_mut().find(|model| model.model_id == *id) {
            if super::preview::import_owns_model(model) {
                model.sort_index = index as i64;
            }
        } else {
            models.push(SupplierModelRecord {
                id: supplier_dao::model_id_for(endpoint_id, id),
                model_id: id.clone(),
                display_name: None,
                enabled: true,
                supports_tools: None,
                supports_images: None,
                context_window: None,
                max_output_tokens: None,
                tier_override: None,
                sort_index: index as i64,
                source: SOURCE_KIND.into(),
                provenance: serde_json::json!({}),
                retire_misses: 0,
                retired_at: None,
                retire_reason: None,
            });
        }
    }
    models.sort_by(|left, right| {
        left.sort_index
            .cmp(&right.sort_index)
            .then_with(|| left.model_id.cmp(&right.model_id))
    });
    models
}

fn apply_transaction(
    db: &Database,
    derived: &DerivedPlan,
    changed_entries: &[PlannedSupplier],
    existing: &[SupplierRecord],
    expected_generation: u64,
) -> Result<(), AppError> {
    let mut guard = crate::database::lock_conn!(db.conn);
    let transaction = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
    bump_generation(&transaction, expected_generation)?;
    apply_entries_on_transaction(&transaction, derived, changed_entries, existing)?;
    transaction.commit()?;
    Ok(())
}

/// Shared insertion body for ordinary patch imports and an explicitly reset
/// inventory. The caller owns the transaction and generation transition.
pub(super) fn apply_entries_on_transaction(
    transaction: &Transaction<'_>,
    derived: &DerivedPlan,
    changed_entries: &[PlannedSupplier],
    existing: &[SupplierRecord],
) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    let batch_ref = &derived.source_fingerprint[..12];

    for entry in changed_entries {
        assert_import_owns(&transaction, &entry.supplier_id)?;
        let protocol = entry.protocol.supplier_protocol();
        let old_supplier = existing
            .iter()
            .find(|record| record.id == entry.supplier_id);
        let old_endpoint = old_supplier.and_then(|record| {
            record
                .endpoints
                .iter()
                .find(|endpoint| endpoint.protocol == protocol)
        });
        let supplier_enabled = old_supplier.is_none_or(|record| record.enabled);
        let supplier_priority = old_supplier.map(|record| record.priority).unwrap_or(100);
        let provenance_json = serde_json::to_string(&provenance_value(
            &entry.source_app_type,
            &entry.source_id,
            Some(protocol),
            batch_ref,
        )?)?;
        transaction.execute(
            "INSERT INTO suppliers (
                 id, name, base_url, api_key, enabled, priority, source,
                 provenance_json, icon, icon_color, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 1, 100, 'cc_switch', ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 base_url = excluded.base_url,
                 api_key = excluded.api_key,
                 source = 'cc_switch',
                 provenance_json = excluded.provenance_json,
                 icon = excluded.icon,
                 icon_color = excluded.icon_color,
                 updated_at = excluded.updated_at",
            params![
                entry.supplier_id,
                entry.name,
                entry.supplier_base_url,
                entry.api_key,
                provenance_json,
                entry.icon,
                entry.icon_color,
                now
            ],
        )?;

        let endpoint_id = supplier_dao::endpoint_id_for(&entry.supplier_id, protocol);
        let headers_json = serde_json::to_string(
            &entry
                .headers
                .iter()
                .cloned()
                .collect::<std::collections::HashMap<_, _>>(),
        )?;
        let endpoint_provenance_json = serde_json::to_string(&provenance_value(
            &entry.source_app_type,
            &entry.source_id,
            Some(protocol),
            batch_ref,
        )?)?;
        transaction.execute(
            "INSERT INTO supplier_endpoints (
                 id, supplier_id, protocol, endpoint_url, api_key_override, enabled,
                 priority, extra_headers, source, provenance_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?7, ?8, 1, 0, ?4, 'cc_switch', ?5, ?6, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 endpoint_url = excluded.endpoint_url,
                 api_key_override = excluded.api_key_override,
                 extra_headers = excluded.extra_headers,
                 source = 'cc_switch',
                 provenance_json = excluded.provenance_json,
                 updated_at = excluded.updated_at",
            params![
                endpoint_id,
                entry.supplier_id,
                protocol,
                headers_json,
                endpoint_provenance_json,
                now,
                entry.base_url,
                entry.api_key
            ],
        )?;

        let old_models = old_endpoint
            .map(|endpoint| endpoint.models.as_slice())
            .unwrap_or_default();
        for old in old_models.iter().filter(|model| {
            super::preview::import_owns_model(model) && !entry.models.contains(&model.model_id)
        }) {
            transaction.execute("DELETE FROM supplier_models WHERE id=?1", [&old.id])?;
        }
        for (sort_index, model) in entry.models.iter().enumerate() {
            if let Some(old) = old_models.iter().find(|old| old.model_id == *model) {
                if super::preview::import_owns_model(old) {
                    transaction.execute(
                        "UPDATE supplier_models SET sort_index=?1 WHERE id=?2",
                        params![sort_index as i64, old.id],
                    )?;
                }
                continue;
            }
            let model_provenance_json = serde_json::to_string(&provenance_value(
                &entry.source_app_type,
                &entry.source_id,
                Some(protocol),
                batch_ref,
            )?)?;
            transaction.execute(
                "INSERT INTO supplier_models (
                     id, endpoint_id, model_id, display_name, enabled, supports_tools,
                     supports_images, context_window, max_output_tokens, tier_override,
                     source, provenance_json, sort_index, created_at, updated_at
                 ) VALUES (
                     ?1, ?2, ?3, NULL, 1, NULL, NULL, NULL, NULL, NULL,
                     'cc_switch', ?4, ?5, ?6, ?6
                 )",
                params![
                    supplier_dao::model_id_for(&endpoint_id, model),
                    endpoint_id,
                    model,
                    model_provenance_json,
                    sort_index as i64,
                    now
                ],
            )?;
        }
        let mut projected_entry = entry.clone();
        projected_entry.models = {
            let mut statement = transaction.prepare("SELECT model_id FROM supplier_models WHERE endpoint_id=?1 AND enabled=1 ORDER BY sort_index,model_id")?;
            let rows = statement.query_map([&endpoint_id], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        sync_projection(
            &transaction,
            &projected_entry,
            &endpoint_id,
            None,
            true,
            supplier_enabled && old_endpoint.is_none_or(|endpoint| endpoint.enabled),
            supplier_dao::priority_to_legacy_weight(
                supplier_priority,
                old_endpoint.map(|endpoint| endpoint.priority).unwrap_or(0),
            ),
        )?;
        let raw: String = transaction.query_row(
            "SELECT alternatives_json FROM supplier_endpoints WHERE id=?1",
            [&endpoint_id],
            |row| row.get(0),
        )?;
        let mut alternatives: Vec<SupplierEndpointAlternativeRecord> = serde_json::from_str(&raw)?;
        for alternative in &entry.alternatives {
            let computed_id = supplier_dao::endpoint_id_for(
                &entry.supplier_id,
                &format!(
                    "{}:{}:{}:{}",
                    protocol,
                    alternative.source_app_type,
                    alternative.source_id,
                    alternative.base_url
                ),
            );
            let old = alternatives
                .iter()
                .find(|saved| {
                    saved.id == computed_id
                        || (saved.base_url == alternative.base_url
                            && saved.api_key == alternative.api_key
                            && saved.extra_headers == alternative.headers.iter().cloned().collect())
                })
                .cloned();
            let id = old
                .as_ref()
                .map(|saved| saved.id.clone())
                .unwrap_or(computed_id);
            let prior = alternatives
                .iter()
                .find(|saved| saved.id == id)
                .and_then(|saved| saved.legacy_proxy_provider_id.clone());
            let projection = projection_id_for(&id);
            let projected = prior
                .clone()
                .or_else(|| entry.protocol.legacy_provider_type().map(|_| projection));
            let mut variant_entry = entry.clone();
            variant_entry.base_url = alternative.base_url.clone();
            variant_entry.api_key = alternative.api_key.clone();
            variant_entry.headers = alternative.headers.clone();
            let models = preserved_variant_models(
                old.as_ref()
                    .map(|saved| saved.models.as_slice())
                    .unwrap_or_default(),
                &alternative.models,
                &id,
            );
            variant_entry.models = models
                .iter()
                .filter(|model| model.enabled)
                .map(|model| model.model_id.clone())
                .collect();
            sync_projection(
                &transaction,
                &variant_entry,
                &id,
                prior.as_deref(),
                false,
                supplier_enabled && old.as_ref().is_none_or(|saved| saved.enabled),
                supplier_dao::priority_to_legacy_weight(
                    supplier_priority,
                    old.as_ref().map(|saved| saved.priority).unwrap_or(0),
                ),
            )?;
            let variant = SupplierEndpointAlternativeRecord {
                id: id.clone(),
                base_url: alternative.base_url.clone(),
                api_key: alternative.api_key.clone(),
                enabled: old.as_ref().is_none_or(|saved| saved.enabled),
                priority: old.as_ref().map(|saved| saved.priority).unwrap_or(0),
                extra_headers: alternative.headers.iter().cloned().collect(),
                source: old
                    .as_ref()
                    .map(|saved| saved.source.clone())
                    .unwrap_or_else(|| "cc_switch".into()),
                provenance: old
                    .as_ref()
                    .map(|saved| Ok(saved.provenance.clone()))
                    .unwrap_or_else(|| {
                        provenance_value(
                            &alternative.source_app_type,
                            &alternative.source_id,
                            Some(protocol),
                            batch_ref,
                        )
                    })?,
                legacy_proxy_provider_id: projected,
                models,
            };
            if let Some(index) = alternatives.iter().position(|saved| saved.id == id) {
                alternatives[index] = variant;
            } else {
                alternatives.push(variant);
            }
        }
        if alternatives.len() > 64 {
            return Err(AppError::InvalidInput(
                "supplier_alternatives_too_many".into(),
            ));
        }
        transaction.execute(
            "UPDATE supplier_endpoints SET alternatives_json=?1 WHERE id=?2",
            params![serde_json::to_string(&alternatives)?, endpoint_id],
        )?;
    }

    for pending in &derived.pending {
        assert_import_owns(&transaction, &pending.supplier_id)?;
        let provenance_json = serde_json::to_string(&pending_provenance_value(
            &pending.source_app_type,
            &pending.source_id,
            batch_ref,
        )?)?;
        transaction.execute(
            "INSERT INTO suppliers (
                 id, name, base_url, api_key, enabled, priority, source,
                 provenance_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, '', 0, 100, 'official_auth_pending', ?4, ?5, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 base_url = excluded.base_url,
                 api_key = '',
                 enabled = 0,
                 priority = excluded.priority,
                 source = 'official_auth_pending',
                 provenance_json = excluded.provenance_json,
                 updated_at = excluded.updated_at",
            params![
                pending.supplier_id,
                pending.name,
                pending.base_url,
                provenance_json,
                now
            ],
        )?;
    }

    Ok(())
}

/// A planned row may only create or refresh rows this importer owns.
/// A colliding manual supplier fails the whole transaction — nothing is
/// overwritten and nothing is partially applied.
fn assert_import_owns(transaction: &Transaction<'_>, supplier_id: &str) -> Result<(), AppError> {
    let existing_source: Option<String> = transaction
        .query_row(
            "SELECT source FROM suppliers WHERE id = ?1",
            [supplier_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(source) = existing_source {
        if source != "cc_switch" && source != "official_auth_pending" {
            return Err(AppError::Message("supplier_import_conflict".into()));
        }
    }
    Ok(())
}

fn bump_generation(
    transaction: &Transaction<'_>,
    expected_generation: u64,
) -> Result<(), AppError> {
    if expected_generation >= i64::MAX as u64 {
        return Err(AppError::InvalidInput(
            "supplier_generation_out_of_range".into(),
        ));
    }
    let expected = expected_generation as i64;
    let updated = transaction.execute(
        "UPDATE supplier_config_meta
         SET generation = generation + 1
         WHERE id = 1 AND generation = ?1",
        [expected],
    )?;
    if updated != 1 {
        return Err(AppError::Message(
            "supplier_config_generation_conflict".into(),
        ));
    }
    Ok(())
}

/// Mirror of `database::dao::suppliers::sync_projection_on_conn`: project
/// the endpoint into a legacy `proxy_providers` row so the running proxy
/// sees the imported supplier. Responses-native endpoints have no legacy
/// type and are deliberately not projected. The import writes the display
/// icons mapped from the cc-switch source row onto both the supplier and
/// its managed projection — managed rows belong to the import, so a
/// refresh replaces them.
fn sync_projection(
    transaction: &Transaction<'_>,
    entry: &PlannedSupplier,
    endpoint_id: &str,
    existing_variant_id: Option<&str>,
    write_mapping: bool,
    enabled: bool,
    weight: u32,
) -> Result<(), AppError> {
    let Some(provider_type) = entry.protocol.legacy_provider_type() else {
        return Ok(());
    };
    let existing_mapping: Option<(String, bool)> = if let Some(id) = existing_variant_id {
        Some((id.to_string(), true))
    } else {
        transaction
            .query_row(
                "SELECT proxy_provider_id, managed FROM supplier_runtime_projections
             WHERE endpoint_id = ?1",
                [endpoint_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
    };
    let models_json = serde_json::to_string(&entry.models)?;
    let headers: std::collections::HashMap<&str, &str> = entry
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let headers_json = serde_json::to_string(&headers)?;
    let name = format!("{} ({})", entry.name, entry.protocol.supplier_protocol());
    let now = chrono::Utc::now().timestamp();

    if let Some((provider_id, _managed)) = existing_mapping {
        let changed = transaction.execute(
            "UPDATE proxy_providers SET
                 name = ?1, type = ?2, api_key = ?3, base_url = ?4,
                 models = ?5, enabled = ?11, weight = ?6, extra_headers = ?7,
                 icon = ?8, icon_color = ?9
             WHERE id = ?10",
            params![
                name,
                provider_type,
                entry.api_key,
                entry.base_url,
                models_json,
                weight,
                headers_json,
                entry.icon,
                entry.icon_color,
                provider_id,
                enabled,
            ],
        )?;
        if changed != 1 {
            return Err(AppError::Database(
                "supplier projection points to a missing proxy provider".into(),
            ));
        }
        return Ok(());
    }

    let proxy_provider_id = projection_id_for(endpoint_id);
    let collision: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM proxy_providers WHERE id = ?1)",
        [&proxy_provider_id],
        |row| row.get(0),
    )?;
    if collision {
        return Err(AppError::Message("supplier_projection_id_conflict".into()));
    }
    let breaker_json = serde_json::to_string(
        &crate::database::dao::proxy_providers::CircuitBreakerConfig::default(),
    )?;
    transaction.execute(
        "INSERT INTO proxy_providers (
             id, name, type, api_key, base_url, models, enabled, timeout_ms, weight,
             circuit_breaker, model_mapping, extra_headers, auto_track_models,
             icon, icon_color, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?13, 60000, ?7, ?8, '{}', ?9, 1, ?10, ?11, ?12)",
        params![
            proxy_provider_id,
            name,
            provider_type,
            entry.api_key,
            entry.base_url,
            models_json,
            weight,
            breaker_json,
            headers_json,
            entry.icon,
            entry.icon_color,
            now,
            enabled,
        ],
    )?;
    if write_mapping {
        transaction.execute(
            "INSERT INTO supplier_runtime_projections (endpoint_id, proxy_provider_id, managed)
         VALUES (?1, ?2, 1)",
            params![endpoint_id, proxy_provider_id],
        )?;
    }
    Ok(())
}
