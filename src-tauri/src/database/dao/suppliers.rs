//! Unified supplier persistence and compatibility projection.
//!
//! Secret-bearing records in this module deliberately do not implement
//! `Serialize` or `Debug`. UI views are constructed by
//! `services::suppliers` from these internal records.

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::Value;

mod maintenance;
mod discovery;
mod url_discovery;
pub use discovery::SupplierDiscoveryHealth;
mod origins;
pub(crate) use maintenance::{SupplierMaintenanceResult, SupplierMergePlan};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub(crate) const KEY_SUPPLIER_IMPORT_SETUP_PENDING: &str = "supplier_import_setup_pending";

pub const PROTOCOL_ANTHROPIC: &str = "anthropic";
pub const PROTOCOL_OPENAI_CHAT: &str = "openai-chat";
pub const PROTOCOL_OPENAI_RESPONSES: &str = "openai-responses";
pub const PROTOCOL_GEMINI: &str = "gemini";
pub const SUPPORTED_PROTOCOLS: [&str; 4] = [
    PROTOCOL_ANTHROPIC,
    PROTOCOL_OPENAI_CHAT,
    PROTOCOL_OPENAI_RESPONSES,
    PROTOCOL_GEMINI,
];

pub(crate) const MAX_CATALOG_JSON_BYTES: usize = 8 * 1024 * 1024;

/// A credential-bearing supplier record. Never serialize this type.
pub(crate) struct SupplierRecord {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub enabled: bool,
    pub priority: i64,
    pub source: String,
    pub provenance: Value,
    /// Display glyph id or literal character; presentation-only.
    pub icon: Option<String>,
    /// Tile background; presentation-only.
    pub icon_color: Option<String>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub endpoints: Vec<SupplierEndpointRecord>,
}

/// A credential-bearing endpoint record. Never serialize this type.
pub(crate) struct SupplierEndpointRecord {
    pub id: String,
    pub protocol: String,
    pub endpoint_url: Option<String>,
    pub api_key_override: Option<String>,
    pub enabled: bool,
    pub priority: i64,
    pub extra_headers: HashMap<String, String>,
    pub source: String,
    pub provenance: Value,
    pub proxy_provider_id: Option<String>,
    /// True only for supplier-service-owned projections (`managed = 1`); an
    /// anchor row pointing at a pre-existing legacy provider is `false`.
    pub projection_managed: bool,
    pub models: Vec<SupplierModelRecord>,
    pub alternatives: Vec<SupplierEndpointAlternativeRecord>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SupplierModelRecord {
    pub id: String,
    pub model_id: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub supports_tools: Option<bool>,
    pub supports_images: Option<bool>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub tier_override: Option<u8>,
    pub sort_index: i64,
    pub source: String,
    pub provenance: Value,
    /// Consecutive successful upstream listings that omitted this model.
    pub retire_misses: i64,
    /// Set when catalog reconciliation retired (or stale-flagged) the row.
    pub retired_at: Option<i64>,
    pub retire_reason: Option<String>,
}

/// Private persistence only. Never pass these credential-bearing snapshots to IPC.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SupplierEndpointAlternativeRecord {
    pub id: String,
    pub base_url: String,
    pub api_key: String,
    pub enabled: bool,
    pub priority: i64,
    pub extra_headers: HashMap<String, String>,
    pub source: String,
    pub provenance: Value,
    pub legacy_proxy_provider_id: Option<String>,
    pub models: Vec<SupplierModelRecord>,
}

pub(crate) struct SupplierPersistInput {
    pub id: String,
    pub must_be_new: bool,
    pub name: String,
    pub base_url: Option<String>,
    /// `None` preserves a saved key; `Some("")` intentionally clears it.
    pub api_key: Option<String>,
    pub enabled: bool,
    pub priority: i64,
    pub source: Option<String>,
    pub provenance: Option<Value>,
    /// `None` preserves the stored icon; `Some("")` returns to the monogram.
    pub icon: Option<String>,
    /// `None` preserves the stored color; `Some("")` clears it.
    pub icon_color: Option<String>,
    /// `None` preserves the endpoint set; `Some([])` removes all endpoints.
    pub endpoints: Option<Vec<SupplierEndpointPersistInput>>,
}

pub(crate) struct SupplierEndpointPersistInput {
    pub protocol: String,
    /// `None` preserves an override; an empty string returns to supplier base URL.
    pub endpoint_url: Option<String>,
    /// `None` preserves an override; `Some("")` stores explicit no-auth.
    pub api_key_override: Option<String>,
    pub use_shared_api_key: bool,
    pub enabled: Option<bool>,
    pub priority: Option<i64>,
    /// `None` preserves headers; `Some({})` clears them.
    pub extra_headers: Option<HashMap<String, String>>,
    pub source: Option<String>,
    pub provenance: Option<Value>,
    /// `None` preserves models; `Some([])` clears them.
    pub models: Option<Vec<SupplierModelPersistInput>>,
}

pub(crate) struct SupplierModelPersistInput {
    pub model_id: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub supports_tools: Option<bool>,
    pub supports_images: Option<bool>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub tier_override: Option<u8>,
    pub source: Option<String>,
    pub provenance: Option<Value>,
}

pub(crate) struct SupplierCatalogCacheRecord {
    pub source: String,
    pub raw_json: String,
    pub fetched_at: i64,
    pub last_error: Option<String>,
}

/// Return a deterministic, structured ID for an endpoint.
///
/// IDs are stable across updates, independent of display names, and do not
/// contain supplier-provided text. The protocol is part of the identity, so an
/// endpoint's credentials and model catalog cannot accidentally cross wires.
pub fn endpoint_id_for(supplier_id: &str, protocol: &str) -> String {
    format!(
        "ep_{}",
        stable_digest("supplier-endpoint-v1", &[supplier_id, protocol])
    )
}

pub(crate) fn model_id_for(endpoint_id: &str, model_id: &str) -> String {
    format!(
        "sm_{}",
        stable_digest("supplier-model-v1", &[endpoint_id, model_id])
    )
}

pub(crate) fn legacy_supplier_id(source: &str, legacy_id: &str) -> String {
    format!(
        "supplier_legacy_{}_{}",
        source,
        stable_digest("supplier-legacy-v1", &[source, legacy_id])
    )
}

pub(crate) fn projection_id_for(endpoint_id: &str) -> String {
    format!(
        "supplier_{}",
        stable_digest("supplier-projection-v1", &[endpoint_id])
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

pub(crate) fn is_supported_protocol(protocol: &str) -> bool {
    SUPPORTED_PROTOCOLS.contains(&protocol)
}

impl Database {
    /// Read generation and supplier rows from one SQLite snapshot for callers
    /// that build an immutable proxy snapshot or return an optimistic UI view.
    pub(crate) fn get_supplier_snapshot(&self) -> Result<(u64, Vec<SupplierRecord>), AppError> {
        let conn = lock_conn!(self.conn);
        let transaction = conn.unchecked_transaction()?;
        let generation = read_generation_on_conn(&transaction)?;
        let suppliers = load_supplier_records_on_conn(&transaction, None)?;
        transaction.commit()?;
        Ok((generation, suppliers))
    }

    pub(crate) fn get_supplier_record_snapshot(
        &self,
        id: &str,
    ) -> Result<(u64, Option<SupplierRecord>), AppError> {
        let conn = lock_conn!(self.conn);
        let transaction = conn.unchecked_transaction()?;
        let generation = read_generation_on_conn(&transaction)?;
        let supplier = load_supplier_records_on_conn(&transaction, Some(id))?
            .into_iter()
            .next();
        transaction.commit()?;
        Ok((generation, supplier))
    }

    pub(crate) fn supplier_config_generation(&self) -> Result<u64, AppError> {
        let conn = lock_conn!(self.conn);
        let value: i64 = conn.query_row(
            "SELECT generation FROM supplier_config_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        u64::try_from(value)
            .map_err(|_| AppError::Database("supplier generation is out of range".into()))
    }

    /// Save a validated supplier and project compatible endpoints in one
    /// optimistic transaction. Every failure rolls back both configuration and
    /// generation, so a partially updated proxy pool is never visible.
    pub(crate) fn save_supplier_record(
        &self,
        input: &SupplierPersistInput,
        expected_generation: u64,
    ) -> Result<u64, AppError> {
        let expected = generation_to_sql(expected_generation)?;
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        bump_generation(&transaction, expected)?;

        if input.must_be_new {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM suppliers WHERE id = ?1)",
                [&input.id],
                |row| row.get(0),
            )?;
            if exists {
                return Err(AppError::Message("supplier_id_conflict".into()));
            }
        }

        let provenance_json = input
            .provenance
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        // Display columns: omission preserves, an explicit empty string
        // clears to NULL so reads never surface a blank icon.
        let icon_present = input.icon.is_some();
        let icon_color_present = input.icon_color.is_some();
        let icon = input.icon.as_deref().filter(|value| !value.is_empty());
        let icon_color = input
            .icon_color
            .as_deref()
            .filter(|value| !value.is_empty());
        let now = chrono::Utc::now().timestamp();
        transaction.execute(
            "INSERT INTO suppliers (
                 id, name, base_url, api_key, enabled, priority, source,
                 provenance_json, icon, icon_color, created_at, updated_at
             ) VALUES (
                 ?1, ?2, COALESCE(?3, ''), COALESCE(?4, ''), ?5, ?6,
                 COALESCE(?7, 'manual'), COALESCE(?8, '{}'), ?9, ?10, ?11, ?11
             )
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 base_url = COALESCE(?3, suppliers.base_url),
                 api_key = COALESCE(?4, suppliers.api_key),
                 enabled = excluded.enabled,
                 priority = excluded.priority,
                 source = COALESCE(?7, suppliers.source),
                 provenance_json = COALESCE(?8, suppliers.provenance_json),
                 icon = CASE WHEN ?12 THEN ?9 ELSE suppliers.icon END,
                 icon_color = CASE WHEN ?13 THEN ?10 ELSE suppliers.icon_color END,
                 updated_at = excluded.updated_at",
            params![
                input.id,
                input.name,
                input.base_url,
                input.api_key,
                input.enabled,
                input.priority,
                input.source,
                provenance_json,
                icon,
                icon_color,
                now,
                icon_present,
                icon_color_present,
            ],
        )?;

        if let Some(endpoints) = input.endpoints.as_ref() {
            let desired: std::collections::HashSet<&str> = endpoints
                .iter()
                .map(|endpoint| endpoint.protocol.as_str())
                .collect();
            let old_endpoints = endpoint_ids_on_conn(&transaction, &input.id)?;
            for (protocol, endpoint_id) in old_endpoints {
                if !desired.contains(protocol.as_str()) {
                    delete_endpoint_on_conn(&transaction, &endpoint_id)?;
                }
            }
            for endpoint in endpoints {
                upsert_endpoint_on_conn(&transaction, &input.id, input.priority, endpoint)?;
            }
        }

        let saved = load_supplier_records_on_conn(&transaction, Some(&input.id))?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::Database("saved supplier disappeared".into()))?;
        for endpoint in &saved.endpoints {
            sync_projection_on_conn(&transaction, &saved, endpoint)?;
        }

        transaction.commit()?;
        expected_generation
            .checked_add(1)
            .ok_or_else(|| AppError::Database("supplier generation overflow".into()))
    }

    pub(crate) fn delete_supplier_record(
        &self,
        id: &str,
        expected_generation: u64,
    ) -> Result<u64, AppError> {
        let expected = generation_to_sql(expected_generation)?;
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        bump_generation(&transaction, expected)?;

        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM suppliers WHERE id = ?1)",
            [id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(AppError::InvalidInput("supplier_not_found".into()));
        }
        let projections = projection_rows_for_supplier(&transaction, id)?;
        for (provider_id, managed) in projections {
            if managed {
                if provider_is_referenced_by_routes_on_conn(&transaction, &provider_id)? {
                    return Err(AppError::Message("supplier_in_use".into()));
                }
                transaction.execute("DELETE FROM proxy_providers WHERE id = ?1", [&provider_id])?;
            }
        }
        for endpoint in load_endpoints_on_conn(&transaction, id)? {
            for alternative in endpoint.alternatives {
                if let Some(provider_id) = alternative.legacy_proxy_provider_id {
                    if provider_is_referenced_by_routes_on_conn(&transaction, &provider_id)? {
                        return Err(AppError::Message("supplier_in_use".into()));
                    }
                    transaction
                        .execute("DELETE FROM proxy_providers WHERE id=?1", [provider_id])?;
                }
            }
        }
        transaction.execute("DELETE FROM suppliers WHERE id = ?1", [id])?;
        transaction.commit()?;
        expected_generation
            .checked_add(1)
            .ok_or_else(|| AppError::Database("supplier generation overflow".into()))
    }

    pub(crate) fn replace_supplier_models(
        &self,
        supplier_id: &str,
        protocol: &str,
        models: &[SupplierModelPersistInput],
        expected_generation: u64,
    ) -> Result<u64, AppError> {
        let expected = generation_to_sql(expected_generation)?;
        let endpoint_id = endpoint_id_for(supplier_id, protocol);
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        bump_generation(&transaction, expected)?;

        let exists: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM supplier_endpoints WHERE id = ?1 AND supplier_id = ?2
             )",
            params![endpoint_id, supplier_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(AppError::InvalidInput("supplier_endpoint_not_found".into()));
        }
        replace_models_on_conn(&transaction, &endpoint_id, models)?;

        let supplier = load_supplier_records_on_conn(&transaction, Some(supplier_id))?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::Database("supplier disappeared during model update".into()))?;
        let endpoint = supplier
            .endpoints
            .iter()
            .find(|item| item.id == endpoint_id)
            .ok_or_else(|| AppError::Database("endpoint disappeared during model update".into()))?;
        sync_projection_on_conn(&transaction, &supplier, endpoint)?;

        transaction.commit()?;
        expected_generation
            .checked_add(1)
            .ok_or_else(|| AppError::Database("supplier generation overflow".into()))
    }

    pub(crate) fn get_supplier_catalog_cache(
        &self,
        source: &str,
    ) -> Result<Option<SupplierCatalogCacheRecord>, AppError> {
        let conn = lock_conn!(self.conn);
        conn.query_row(
            "SELECT source, raw_json, fetched_at, last_error
             FROM supplier_catalog_cache WHERE source = ?1",
            [source],
            |row| {
                Ok(SupplierCatalogCacheRecord {
                    source: row.get(0)?,
                    raw_json: row.get(1)?,
                    fetched_at: row.get(2)?,
                    last_error: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(AppError::from)
    }

    /// Store a caller-validated, bounded public catalog document. This cache is
    /// intentionally excluded from the routing generation.
    pub(crate) fn save_supplier_catalog_cache(
        &self,
        source: &str,
        raw_json: &str,
        fetched_at: i64,
        last_error: Option<&str>,
    ) -> Result<(), AppError> {
        if raw_json.len() > MAX_CATALOG_JSON_BYTES {
            return Err(AppError::InvalidInput("supplier_catalog_too_large".into()));
        }
        let _: Value = serde_json::from_str(raw_json)?;
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT INTO supplier_catalog_cache (source, raw_json, fetched_at, last_error)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(source) DO UPDATE SET
                 raw_json = excluded.raw_json,
                 fetched_at = excluded.fetched_at,
                 last_error = excluded.last_error",
            params![source, raw_json, fetched_at, last_error],
        )?;
        Ok(())
    }

    /// Mark a failed refresh without discarding the last known-good document.
    /// A missing cache row is represented by JSON `null` and fetched_at=0.
    pub(crate) fn record_supplier_catalog_error(
        &self,
        source: &str,
        error_code: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT INTO supplier_catalog_cache (source, raw_json, fetched_at, last_error)
             VALUES (?1, 'null', 0, ?2)
             ON CONFLICT(source) DO UPDATE SET last_error = excluded.last_error",
            params![source, error_code],
        )?;
        Ok(())
    }
}

fn read_generation_on_conn(conn: &Connection) -> Result<u64, AppError> {
    let value: i64 = conn.query_row(
        "SELECT generation FROM supplier_config_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(value)
        .map_err(|_| AppError::Database("supplier generation is out of range".into()))
}

fn generation_to_sql(generation: u64) -> Result<i64, AppError> {
    if generation >= i64::MAX as u64 {
        return Err(AppError::InvalidInput(
            "supplier_generation_out_of_range".into(),
        ));
    }
    i64::try_from(generation)
        .map_err(|_| AppError::InvalidInput("supplier_generation_out_of_range".into()))
}

fn bump_generation(transaction: &Transaction<'_>, expected: i64) -> Result<(), AppError> {
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

fn endpoint_ids_on_conn(
    conn: &Connection,
    supplier_id: &str,
) -> Result<Vec<(String, String)>, AppError> {
    let mut statement =
        conn.prepare("SELECT protocol, id FROM supplier_endpoints WHERE supplier_id = ?1")?;
    let rows = statement.query_map([supplier_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut endpoints = Vec::new();
    for row in rows {
        endpoints.push(row?);
    }
    Ok(endpoints)
}

fn delete_endpoint_on_conn(conn: &Connection, endpoint_id: &str) -> Result<(), AppError> {
    let raw: String = conn.query_row(
        "SELECT alternatives_json FROM supplier_endpoints WHERE id=?1",
        [endpoint_id],
        |row| row.get(0),
    )?;
    let alternatives: Vec<SupplierEndpointAlternativeRecord> = serde_json::from_str(&raw)?;
    for alternative in alternatives {
        if let Some(provider_id) = alternative.legacy_proxy_provider_id {
            if provider_is_referenced_by_routes_on_conn(conn, &provider_id)? {
                return Err(AppError::Message("supplier_in_use".into()));
            }
            conn.execute("DELETE FROM proxy_providers WHERE id=?1", [provider_id])?;
        }
    }
    let mapping: Option<(String, bool)> = conn
        .query_row(
            "SELECT proxy_provider_id, managed
             FROM supplier_runtime_projections WHERE endpoint_id = ?1",
            [endpoint_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((provider_id, managed)) = mapping {
        if managed {
            if provider_is_referenced_by_routes_on_conn(conn, &provider_id)? {
                return Err(AppError::Message("supplier_in_use".into()));
            }
            conn.execute("DELETE FROM proxy_providers WHERE id = ?1", [&provider_id])?;
        }
        conn.execute(
            "DELETE FROM supplier_runtime_projections WHERE endpoint_id = ?1",
            [endpoint_id],
        )?;
    }
    conn.execute(
        "DELETE FROM supplier_endpoints WHERE id = ?1",
        [endpoint_id],
    )?;
    Ok(())
}

fn upsert_endpoint_on_conn(
    conn: &Connection,
    supplier_id: &str,
    supplier_priority: i64,
    input: &SupplierEndpointPersistInput,
) -> Result<(), AppError> {
    let endpoint_id = endpoint_id_for(supplier_id, &input.protocol);
    let existing: Option<(
        Option<String>,
        Option<String>,
        bool,
        i64,
        String,
        String,
        String,
    )> = conn
        .query_row(
            "SELECT endpoint_url, api_key_override, enabled, priority, extra_headers,
                    source, provenance_json
             FROM supplier_endpoints WHERE id = ?1",
            [&endpoint_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;

    let endpoint_url = match input.endpoint_url.as_deref() {
        Some("") => None,
        Some(value) => Some(value.to_string()),
        None => existing.as_ref().and_then(|row| row.0.clone()),
    };
    let api_key_override = if input.use_shared_api_key {
        None
    } else if let Some(value) = input.api_key_override.as_ref() {
        Some(value.clone())
    } else {
        existing.as_ref().and_then(|row| row.1.clone())
    };
    let enabled = input
        .enabled
        .or_else(|| existing.as_ref().map(|row| row.2))
        .unwrap_or(true);
    let priority = input
        .priority
        .or_else(|| existing.as_ref().map(|row| row.3))
        .unwrap_or(supplier_priority);
    let extra_headers = match input.extra_headers.as_ref() {
        Some(headers) => serde_json::to_string(headers)?,
        None => existing
            .as_ref()
            .map(|row| row.4.clone())
            .unwrap_or_else(|| "{}".into()),
    };
    let provenance = input
        .provenance
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?
        .or_else(|| existing.as_ref().map(|row| row.6.clone()))
        .unwrap_or_else(|| "{}".into());
    let source = input
        .source
        .as_deref()
        .or_else(|| existing.as_ref().map(|row| row.5.as_str()))
        .unwrap_or("manual");
    let now = chrono::Utc::now().timestamp();

    conn.execute(
        "INSERT INTO supplier_endpoints (
             id, supplier_id, protocol, endpoint_url, api_key_override, enabled,
             priority, extra_headers, source, provenance_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
         ON CONFLICT(id) DO UPDATE SET
             endpoint_url = excluded.endpoint_url,
             api_key_override = excluded.api_key_override,
             enabled = excluded.enabled,
             priority = excluded.priority,
             extra_headers = excluded.extra_headers,
             source = excluded.source,
             provenance_json = excluded.provenance_json,
             updated_at = excluded.updated_at",
        params![
            endpoint_id,
            supplier_id,
            input.protocol,
            endpoint_url,
            api_key_override,
            enabled,
            priority,
            extra_headers,
            source,
            provenance,
            now,
        ],
    )?;

    if let Some(models) = input.models.as_ref() {
        replace_models_on_conn(conn, &endpoint_id, models)?;
    }
    Ok(())
}

fn replace_models_on_conn(
    conn: &Connection,
    endpoint_id: &str,
    models: &[SupplierModelPersistInput],
) -> Result<(), AppError> {
    conn.execute(
        "DELETE FROM supplier_models WHERE endpoint_id = ?1",
        [endpoint_id],
    )?;
    let now = chrono::Utc::now().timestamp();
    for (sort_index, model) in models.iter().enumerate() {
        let provenance = model
            .provenance
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?
            .unwrap_or_else(|| "{}".into());
        conn.execute(
            "INSERT INTO supplier_models (
                 id, endpoint_id, model_id, display_name, enabled, supports_tools,
                 supports_images, context_window, max_output_tokens, tier_override,
                 source, provenance_json, sort_index, created_at, updated_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 COALESCE(?11, 'manual'), ?12, ?13, ?14, ?14
             )",
            params![
                model_id_for(endpoint_id, &model.model_id),
                endpoint_id,
                model.model_id,
                model.display_name,
                model.enabled,
                model.supports_tools,
                model.supports_images,
                model.context_window,
                model.max_output_tokens,
                model.tier_override,
                model.source,
                provenance,
                sort_index as i64,
                now,
            ],
        )?;
    }
    Ok(())
}

fn load_supplier_records_on_conn(
    conn: &Connection,
    supplier_id: Option<&str>,
) -> Result<Vec<SupplierRecord>, AppError> {
    let mut statement = conn.prepare(
        "SELECT id, name, base_url, api_key, enabled, priority, source,
                provenance_json, created_at, updated_at, icon, icon_color
         FROM suppliers
         WHERE (?1 IS NULL OR id = ?1)
         ORDER BY priority ASC, name COLLATE NOCASE ASC, id ASC",
    )?;
    let rows = statement.query_map(params![supplier_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, bool>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, Option<i64>>(9)?,
            row.get::<_, Option<String>>(10)?,
            row.get::<_, Option<String>>(11)?,
        ))
    })?;
    let mut raw_suppliers = Vec::new();
    for row in rows {
        raw_suppliers.push(row?);
    }
    drop(statement);

    let mut suppliers = Vec::with_capacity(raw_suppliers.len());
    for (
        id,
        name,
        base_url,
        api_key,
        enabled,
        priority,
        source,
        provenance_json,
        created_at,
        updated_at,
        icon,
        icon_color,
    ) in raw_suppliers
    {
        let provenance: Value = serde_json::from_str(&provenance_json)?;
        let endpoints = load_endpoints_on_conn(conn, &id)?;
        suppliers.push(SupplierRecord {
            id,
            name,
            base_url,
            api_key,
            enabled,
            priority,
            source,
            provenance,
            icon,
            icon_color,
            created_at,
            updated_at,
            endpoints,
        });
    }
    Ok(suppliers)
}

fn load_endpoints_on_conn(
    conn: &Connection,
    supplier_id: &str,
) -> Result<Vec<SupplierEndpointRecord>, AppError> {
    let mut statement = conn.prepare(
        "SELECT e.id, e.protocol, e.endpoint_url, e.api_key_override, e.enabled,
                e.priority, e.extra_headers, e.source, e.provenance_json,
                p.proxy_provider_id, p.managed, e.alternatives_json
         FROM supplier_endpoints e
         LEFT JOIN supplier_runtime_projections p ON p.endpoint_id = e.id
         WHERE e.supplier_id = ?1
         ORDER BY e.priority ASC, e.protocol ASC",
    )?;
    let rows = statement.query_map([supplier_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, bool>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, Option<bool>>(10)?.unwrap_or(false),
            row.get::<_, String>(11)?,
        ))
    })?;
    let mut raw_endpoints = Vec::new();
    for row in rows {
        raw_endpoints.push(row?);
    }
    drop(statement);

    let mut endpoints = Vec::with_capacity(raw_endpoints.len());
    for (
        id,
        protocol,
        endpoint_url,
        api_key_override,
        enabled,
        priority,
        headers_json,
        source,
        provenance_json,
        proxy_provider_id,
        projection_managed,
        alternatives_json,
    ) in raw_endpoints
    {
        let extra_headers = serde_json::from_str(&headers_json)?;
        let provenance = serde_json::from_str(&provenance_json)?;
        let models = load_models_on_conn(conn, &id)?;
        if alternatives_json.len() > 4 * 1024 * 1024 {
            return Err(AppError::InvalidInput(
                "supplier_alternatives_too_large".into(),
            ));
        }
        let alternatives: Vec<SupplierEndpointAlternativeRecord> =
            serde_json::from_str(&alternatives_json)?;
        if alternatives.len() > 64 {
            return Err(AppError::InvalidInput(
                "supplier_alternatives_too_many".into(),
            ));
        }
        endpoints.push(SupplierEndpointRecord {
            id,
            protocol,
            endpoint_url,
            api_key_override,
            enabled,
            priority,
            extra_headers,
            source,
            provenance,
            proxy_provider_id,
            projection_managed,
            models,
            alternatives,
        });
    }
    Ok(endpoints)
}

fn load_models_on_conn(
    conn: &Connection,
    endpoint_id: &str,
) -> Result<Vec<SupplierModelRecord>, AppError> {
    let mut statement = conn.prepare(
        "SELECT id, model_id, display_name, enabled, supports_tools, supports_images,
                context_window, max_output_tokens, tier_override, source, provenance_json,
                sort_index, retire_misses, retired_at, retire_reason
         FROM supplier_models
         WHERE endpoint_id = ?1
         ORDER BY sort_index ASC, model_id COLLATE NOCASE, id",
    )?;
    let rows = statement.query_map([endpoint_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<u64>>(6)?,
            row.get::<_, Option<u64>>(7)?,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, i64>(11)?,
            row.get::<_, i64>(12)?,
            row.get::<_, Option<i64>>(13)?,
            row.get::<_, Option<String>>(14)?,
        ))
    })?;
    let mut models = Vec::new();
    for row in rows {
        let (
            id,
            model_id,
            display_name,
            enabled,
            supports_tools,
            supports_images,
            context_window,
            max_output_tokens,
            tier_override,
            source,
            provenance_json,
            sort_index,
            retire_misses,
            retired_at,
            retire_reason,
        ) = row?;
        let provenance = serde_json::from_str(&provenance_json)?;
        let tier_override = tier_override
            .map(|value| {
                u8::try_from(value)
                    .map_err(|_| AppError::Database("supplier tier is out of range".into()))
            })
            .transpose()?;
        models.push(SupplierModelRecord {
            id,
            model_id,
            display_name,
            enabled,
            supports_tools: supports_tools.map(|value| value != 0),
            supports_images: supports_images.map(|value| value != 0),
            context_window,
            max_output_tokens,
            tier_override,
            sort_index,
            source,
            provenance,
            retire_misses,
            retired_at,
            retire_reason,
        });
    }
    Ok(models)
}

fn projection_rows_for_supplier(
    conn: &Connection,
    supplier_id: &str,
) -> Result<Vec<(String, bool)>, AppError> {
    let mut statement = conn.prepare(
        "SELECT p.proxy_provider_id, p.managed
         FROM supplier_runtime_projections p
         JOIN supplier_endpoints e ON e.id = p.endpoint_id
         WHERE e.supplier_id = ?1",
    )?;
    let rows = statement.query_map([supplier_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut projections = Vec::new();
    for row in rows {
        projections.push(row?);
    }
    Ok(projections)
}

fn provider_is_referenced_by_routes_on_conn(
    conn: &Connection,
    provider_id: &str,
) -> Result<bool, AppError> {
    let routes = {
        let mut statement = conn.prepare("SELECT hops FROM routes ORDER BY alias")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut routes = Vec::new();
        for row in rows {
            routes.push(row?);
        }
        routes
    };
    for hops_json in routes {
        let hops: Vec<crate::database::dao::routes::RouteHop> = serde_json::from_str(&hops_json)?;
        if hops.iter().any(|hop| hop.provider == provider_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sync_projection_on_conn(
    conn: &Connection,
    supplier: &SupplierRecord,
    endpoint: &SupplierEndpointRecord,
) -> Result<(), AppError> {
    maintenance::sync_variant_projections_on_conn(conn, supplier, endpoint)?;
    let Some(provider_type) = legacy_provider_type(&endpoint.protocol) else {
        // Unknown protocol: keep the supplier record intact and let the
        // runtime integration consume its resolved endpoint directly.
        return Ok(());
    };
    let existing_mapping: Option<(String, bool)> = conn
        .query_row(
            "SELECT proxy_provider_id, managed FROM supplier_runtime_projections
             WHERE endpoint_id = ?1",
            [&endpoint.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let base_url = endpoint
        .endpoint_url
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(&supplier.base_url);
    let api_key = endpoint
        .api_key_override
        .as_deref()
        .unwrap_or(&supplier.api_key);
    let models: Vec<&str> = endpoint
        .models
        .iter()
        .filter(|model| model.enabled)
        .map(|model| model.model_id.as_str())
        .collect();
    let models_json = serde_json::to_string(&models)?;
    let headers_json = serde_json::to_string(&endpoint.extra_headers)?;
    let enabled = supplier.enabled && endpoint.enabled;
    let name = format!("{} ({})", supplier.name, endpoint.protocol);
    let weight = priority_to_legacy_weight(supplier.priority, endpoint.priority);
    let now = chrono::Utc::now().timestamp();

    if let Some((provider_id, managed)) = existing_mapping {
        conn.execute("DELETE FROM app_meta WHERE key = ?1", [format!("supplier_discovery_health:{provider_id}")])?;
        // Managed projection rows belong to the supplier service, so the
        // supplier's display icons replace whatever the row carried. An
        // anchor row over a pre-existing legacy provider keeps its own
        // icons: the supplier never owned them.
        let changed = conn.execute(
            "UPDATE proxy_providers SET
                 name = ?1, type = ?2, api_key = ?3, base_url = ?4,
                 models = ?5, enabled = ?6, weight = ?7, extra_headers = ?8,
                 icon = CASE WHEN ?9 THEN ?10 ELSE icon END,
                 icon_color = CASE WHEN ?9 THEN ?11 ELSE icon_color END
             WHERE id = ?12",
            params![
                name,
                provider_type,
                api_key,
                base_url,
                models_json,
                enabled,
                weight,
                headers_json,
                managed,
                supplier.icon.as_deref(),
                supplier.icon_color.as_deref(),
                provider_id,
            ],
        )?;
        if changed != 1 {
            return Err(AppError::Database(
                "supplier projection points to a missing proxy provider".into(),
            ));
        }
        return Ok(());
    }

    let proxy_provider_id = projection_id_for(&endpoint.id);
    let collision: bool = conn.query_row(
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
    conn.execute(
        "INSERT INTO proxy_providers (
             id, name, type, api_key, base_url, models, enabled, timeout_ms, weight,
             circuit_breaker, model_mapping, extra_headers, auto_track_models,
             icon, icon_color, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 60000, ?8, ?9, '{}', ?10, 1, ?11, ?12, ?13)",
        params![
            proxy_provider_id,
            name,
            provider_type,
            api_key,
            base_url,
            models_json,
            enabled,
            weight,
            breaker_json,
            headers_json,
            supplier.icon.as_deref(),
            supplier.icon_color.as_deref(),
            now,
        ],
    )?;
    conn.execute(
        "INSERT INTO supplier_runtime_projections (endpoint_id, proxy_provider_id, managed)
         VALUES (?1, ?2, 1)",
        params![endpoint.id, proxy_provider_id],
    )?;
    Ok(())
}

pub(crate) fn legacy_provider_type(protocol: &str) -> Option<&'static str> {
    match protocol {
        PROTOCOL_ANTHROPIC => Some("anthropic"),
        PROTOCOL_OPENAI_CHAT => Some("openai"),
        PROTOCOL_GEMINI => Some("gemini"),
        // The proxy factory now hosts a native Responses upstream adapter;
        // projecting through it keeps Responses traffic on its own wire.
        PROTOCOL_OPENAI_RESPONSES => Some("openai-responses"),
        _ => None,
    }
}

pub(crate) fn priority_to_legacy_weight(supplier_priority: i64, endpoint_priority: i64) -> u32 {
    // The legacy proxy sorts larger weights first; supplier priority sorts
    // smaller values first. Backfill uses the same base to retain an old row's
    // weight when its supplier is edited, while keeping new priorities ordered.
    let priority = supplier_priority
        .saturating_add(endpoint_priority)
        .clamp(-2_000_000, 2_000_000);
    (201_i64 - priority).clamp(1, 2_000_001) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_endpoint_ids_are_stable_and_protocol_scoped() {
        let anthropic = endpoint_id_for("supplier_abc", PROTOCOL_ANTHROPIC);
        assert_eq!(
            anthropic,
            endpoint_id_for("supplier_abc", PROTOCOL_ANTHROPIC)
        );
        assert_ne!(
            anthropic,
            endpoint_id_for("supplier_abc", PROTOCOL_OPENAI_CHAT)
        );
        assert!(anthropic.starts_with("ep_"));
        assert_eq!(anthropic.len(), 35);
    }

    #[test]
    fn legacy_supplier_ids_do_not_expose_source_record_ids() {
        let id = legacy_supplier_id("vendor", "provider-with-private-looking-name");
        assert!(id.starts_with("supplier_legacy_vendor_"));
        assert!(!id.contains("private-looking-name"));
        assert_eq!(
            id,
            legacy_supplier_id("vendor", "provider-with-private-looking-name")
        );
    }

    #[test]
    fn legacy_priority_conversion_is_bounded_and_reversed() {
        assert!(priority_to_legacy_weight(1, 1) > priority_to_legacy_weight(2, 1));
        assert!(priority_to_legacy_weight(i64::MIN, i64::MAX) <= 2_000_001);
    }
}
