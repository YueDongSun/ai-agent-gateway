//! Catalog observations are distinct from successful model inference.
use super::*;
use crate::database::dao::proxy_providers::{get_proxy_providers_on_conn, ProxyProvider};

const SOURCE_CATALOG: &str = "provider_catalog";
const SOURCE_CATALOG_UNLISTED: &str = "provider_catalog_unlisted";
const SOURCE_MANUAL: &str = "manual";
const RETIRE_REASON_UNLISTED: &str = "catalog_unlisted";
/// Setting key overriding how many consecutive successful-but-unlisted catalog
/// fetches retire an import-owned model row. Defaults to
/// [`DEFAULT_RETIRE_AFTER_MISSES`]; values are clamped to 1..=10.
const KEY_RETIRE_AFTER_MISSES: &str = "supplier_catalog_retire_after_misses";
const DEFAULT_RETIRE_AFTER_MISSES: i64 = 2;

fn retire_after_misses(conn: &Connection) -> i64 {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [KEY_RETIRE_AFTER_MISSES],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten();
    stored
        .and_then(|raw| raw.parse::<i64>().ok())
        .unwrap_or(DEFAULT_RETIRE_AFTER_MISSES)
        .clamp(1, 10)
}

/// Sources whose rows reconciliation may disable outright once the upstream
/// catalog confirms the id is gone. Manual rows are never disabled here; they
/// only receive the stale flag so the owner keeps control.
fn retirement_managed_source(source: &str) -> bool {
    source == "cc_switch" || source == "url_discovery"
}

/// One reconciliation decision for an existing model row. `enabled`/`source`
/// are `None` when that column must stay untouched; `inventory_changed` marks
/// whether the change affects the offered catalog (and therefore projections)
/// or is only retirement bookkeeping.
struct ReconcileUpdate {
    enabled: Option<bool>,
    source: Option<&'static str>,
    retire_misses: i64,
    retired_at: Option<i64>,
    retire_reason: Option<&'static str>,
    inventory_changed: bool,
}

fn reconcile_update(
    model: &super::SupplierModelRecord,
    listed: bool,
    threshold: i64,
    now: i64,
) -> Option<ReconcileUpdate> {
    if listed {
        // Relisting clears any retirement streak and restores rows this
        // reconciliation itself retired; explicit user disables are kept.
        if model.retire_misses == 0 && model.retired_at.is_none() {
            return None;
        }
        let restore = model.retired_at.is_some()
            && model.retire_reason.as_deref() == Some(RETIRE_REASON_UNLISTED)
            && !model.enabled;
        return Some(ReconcileUpdate {
            enabled: restore.then_some(true),
            source: match model.source.as_str() {
                SOURCE_CATALOG_UNLISTED => Some(SOURCE_CATALOG),
                _ => None,
            },
            retire_misses: 0,
            retired_at: None,
            retire_reason: None,
            inventory_changed: restore,
        });
    }
    match model.source.as_str() {
        // Observed catalog rows keep their immediate delisting behavior, now
        // stamped with the shared retirement fields.
        SOURCE_CATALOG if model.enabled => Some(ReconcileUpdate {
            enabled: Some(false),
            source: Some(SOURCE_CATALOG_UNLISTED),
            retire_misses: threshold,
            retired_at: Some(now),
            retire_reason: Some(RETIRE_REASON_UNLISTED),
            inventory_changed: true,
        }),
        // Import-owned rows retire only after a bounded miss streak so a
        // flaky listing cannot disable live supply after one fetch.
        _ if retirement_managed_source(&model.source) => {
            if !model.enabled {
                return None;
            }
            let misses = model.retire_misses.saturating_add(1);
            if misses < threshold {
                return Some(ReconcileUpdate {
                    enabled: None,
                    source: None,
                    retire_misses: misses,
                    retired_at: None,
                    retire_reason: None,
                    inventory_changed: false,
                });
            }
            Some(ReconcileUpdate {
                enabled: Some(false),
                source: None,
                retire_misses: misses,
                retired_at: Some(now),
                retire_reason: Some(RETIRE_REASON_UNLISTED),
                inventory_changed: true,
            })
        }
        // Manual rows are flagged stale but never auto-disabled.
        SOURCE_MANUAL => {
            if model.retired_at.is_some() {
                return None;
            }
            let misses = model.retire_misses.saturating_add(1);
            if misses < threshold {
                return Some(ReconcileUpdate {
                    enabled: None,
                    source: None,
                    retire_misses: misses,
                    retired_at: None,
                    retire_reason: None,
                    inventory_changed: false,
                });
            }
            Some(ReconcileUpdate {
                enabled: None,
                source: None,
                retire_misses: misses,
                retired_at: Some(now),
                retire_reason: Some(RETIRE_REASON_UNLISTED),
                inventory_changed: false,
            })
        }
        _ => None,
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierDiscoveryHealth {
    pub status: String,
    pub checked_at: i64,
    pub last_success_at: Option<i64>,
    pub latency_ms: u64,
    pub consecutive_failures: u32,
    pub error: Option<String>,
    /// Listing evidence only: these IDs have not been tested by paid generation.
    pub listed_model_ids: Vec<String>,
    pub inference_status: String,
}

fn assert_current(conn: &Connection, provider: &ProxyProvider) -> Result<(), AppError> {
    if get_proxy_providers_on_conn(conn, Some(&provider.id))?.first() != Some(provider) {
        return Err(AppError::Message(
            "AAG_MODEL_DISCOVERY_PROVIDER_CONFLICT".into(),
        ));
    }
    Ok(())
}

pub(super) fn store_health(
    conn: &Connection,
    provider_id: &str,
    mut health: SupplierDiscoveryHealth,
) -> Result<(), AppError> {
    let key = format!("supplier_discovery_health:{provider_id}");
    let previous: Option<String> = conn
        .query_row("SELECT value FROM app_meta WHERE key=?1", [&key], |row| {
            row.get(0)
        })
        .optional()?;
    if health.error.is_some() {
        if let Some(previous) =
            previous.and_then(|raw| serde_json::from_str::<SupplierDiscoveryHealth>(&raw).ok())
        {
            health.last_success_at = previous.last_success_at;
            health.listed_model_ids = previous.listed_model_ids;
            health.consecutive_failures = previous.consecutive_failures.saturating_add(1);
        }
    }
    conn.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, serde_json::to_string(&health)?])?;
    Ok(())
}

impl Database {
    pub(crate) fn supplier_discovery_health(
        &self,
        provider_id: &str,
    ) -> Result<Option<SupplierDiscoveryHealth>, AppError> {
        self.get_setting(&format!("supplier_discovery_health:{provider_id}"))?
            .map(|raw| serde_json::from_str(&raw).map_err(AppError::from))
            .transpose()
    }

    pub(crate) fn record_supplier_discovery_failure(
        &self,
        provider: &ProxyProvider,
        health: SupplierDiscoveryHealth,
    ) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        assert_current(&transaction, provider)?;
        store_health(&transaction, &provider.id, health)?;
        transaction.commit()?;
        Ok(())
    }

    /// Add every exactly listed ID, including new families, without changing
    /// manually disabled models, capabilities, names, aliases, or route hops.
    /// Compare the admitted provider before writing either offers or health.
    ///
    /// Delisting now retires rows across sources: observed catalog rows flip
    /// immediately (existing behavior), import-owned rows (`cc_switch`,
    /// `url_discovery`) only after `retire_after_misses` consecutive
    /// successful-but-unlisted fetches, and manual rows are flagged stale
    /// without being disabled. A later listing restores every row this
    /// reconciliation retired; user-disabled rows stay disabled.
    pub(crate) fn reconcile_supplier_catalog(
        &self,
        provider: &ProxyProvider,
        health: SupplierDiscoveryHealth,
    ) -> Result<Option<(Vec<String>, bool)>, AppError> {
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        assert_current(&transaction, provider)?;
        let owner: Option<(String,String)> = transaction.query_row(
            "SELECT e.supplier_id,e.id FROM supplier_endpoints e JOIN supplier_runtime_projections p ON p.endpoint_id=e.id WHERE p.proxy_provider_id=?1",
            [&provider.id], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
        let Some((supplier_id, endpoint_id)) = owner else {
            return Ok(None);
        };
        let existing = load_models_on_conn(&transaction, &endpoint_id)?;
        let mut added = Vec::new();
        let mut inventory_changed = false;
        let now = chrono::Utc::now().timestamp();
        let threshold = retire_after_misses(&transaction);
        for model in &existing {
            let listed = health.listed_model_ids.contains(&model.model_id);
            let Some(update) = reconcile_update(model, listed, threshold, now) else {
                continue;
            };
            let provenance = if let Some(source) = update.source {
                serde_json::json!({"sourceKind":source,"fetchedAt":now,"confidence":1.0})
            } else {
                model.provenance.clone()
            };
            transaction.execute(
                "UPDATE supplier_models
                 SET enabled=COALESCE(?1,enabled), source=COALESCE(?2,source),
                     retire_misses=?3, retired_at=?4, retire_reason=?5,
                     provenance_json=?6, updated_at=?7
                 WHERE id=?8",
                params![
                    update.enabled,
                    update.source,
                    update.retire_misses,
                    update.retired_at,
                    update.retire_reason,
                    provenance.to_string(),
                    now,
                    model.id
                ],
            )?;
            inventory_changed |= update.inventory_changed;
        }
        for model_id in &health.listed_model_ids {
            if existing.iter().any(|model| &model.model_id == model_id) || added.contains(model_id)
            {
                continue;
            }
            if existing.len() + added.len() >= 1000 {
                return Err(AppError::InvalidInput(
                    "supplier_models_limit_exceeded".into(),
                ));
            }
            let provenance = serde_json::json!({"sourceKind":"provider_catalog", "fetchedAt":now,"confidence":1.0});
            transaction.execute("INSERT INTO supplier_models(id,endpoint_id,model_id,enabled,sort_index,source,provenance_json,created_at,updated_at) VALUES(?1,?2,?3,1,?4,'provider_catalog',?5,?6,?6)",
                params![model_id_for(&endpoint_id,model_id),endpoint_id,model_id,(existing.len()+added.len()) as i64,provenance.to_string(),now])?;
            added.push(model_id.clone());
        }
        inventory_changed |= !added.is_empty();
        if inventory_changed {
            let generation = read_generation_on_conn(&transaction)?;
            bump_generation(&transaction, generation_to_sql(generation)?)?;
            let supplier = load_supplier_records_on_conn(&transaction, Some(&supplier_id))?
                .into_iter()
                .next()
                .ok_or_else(|| AppError::Message("supplier_not_found".into()))?;
            let endpoint = supplier
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == endpoint_id)
                .ok_or_else(|| AppError::Message("supplier_endpoint_not_found".into()))?;
            sync_projection_on_conn(&transaction, &supplier, endpoint)?;
        }
        store_health(&transaction, &provider.id, health)?;
        transaction.commit()?;
        Ok(Some((added, inventory_changed)))
    }
}
