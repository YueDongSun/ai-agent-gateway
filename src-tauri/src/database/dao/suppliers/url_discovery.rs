//! Additive URL discovery. Existing endpoints, variants and models are never replaced.
use super::*;
use crate::database::dao::proxy_providers::{get_proxy_providers_on_conn, ProxyProvider};

impl Database {
    pub(crate) fn supplier_discovery_ids(&self) -> Result<Vec<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut statement = conn.prepare("SELECT id FROM suppliers ORDER BY priority,id")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    /// Read ownership without decoding unrelated supplier configuration. Malformed
    /// supplier JSON can then be reported for that supplier without losing the batch.
    pub(crate) fn supplier_discovery_owner(
        &self,
        provider_id: &str,
    ) -> Result<Option<(String, String)>, AppError> {
        let conn = lock_conn!(self.conn);
        let primary = conn
            .query_row(
                "SELECT e.supplier_id,e.protocol FROM supplier_endpoints e
             JOIN supplier_runtime_projections p ON p.endpoint_id=e.id
             WHERE p.proxy_provider_id=?1",
                [provider_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if primary.is_some() {
            return Ok(primary);
        }
        let mut statement = conn.prepare("SELECT supplier_id,protocol,alternatives_json FROM supplier_endpoints WHERE alternatives_json!='[]'")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut owner = None;
        let mut malformed = false;
        for row in rows {
            let (supplier_id, protocol, raw) = row?;
            let variants = if raw.len() <= MAX_CATALOG_JSON_BYTES {
                serde_json::from_str::<Vec<Value>>(&raw).ok()
            } else {
                None
            };
            let Some(variants) = variants else {
                malformed = true;
                continue;
            };
            for variant in variants {
                if !variant.is_object() {
                    malformed = true;
                    continue;
                }
                match variant.get("legacy_proxy_provider_id") {
                    Some(Value::String(id)) if id == provider_id => {
                        if owner.is_some() {
                            return Err(AppError::Message(
                                "supplier_projection_owner_ambiguous".into(),
                            ));
                        }
                        owner = Some((supplier_id.clone(), protocol.clone()));
                    }
                    None | Some(Value::Null) | Some(Value::String(_)) => {}
                    _ => malformed = true,
                }
            }
        }
        if owner.is_none() && malformed {
            // Unknown ownership cannot safely fall through to an unbound legacy
            // connection. Known primary/alternative owners still run independently.
            return Err(AppError::Message(
                "supplier_projection_owner_invalid".into(),
            ));
        }
        Ok(owner)
    }

    /// Commit a validated new protocol, its catalog and compatibility projection
    /// together. A duplicate is a no-op; concurrent edits invalidate the candidate.
    pub(crate) fn append_discovered_supplier_endpoint(
        &self,
        supplier_id: &str,
        source: &ProxyProvider,
        endpoint: &SupplierEndpointPersistInput,
        health: SupplierDiscoveryHealth,
        expected_generation: u64,
    ) -> Result<Option<String>, AppError> {
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let supplier = load_supplier_records_on_conn(&transaction, Some(supplier_id))?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::Message("supplier_not_found".into()))?;
        if !supplier.enabled
            || get_proxy_providers_on_conn(&transaction, Some(&source.id))?.first() != Some(source)
        {
            return Err(AppError::Message(
                "AAG_MODEL_DISCOVERY_PROVIDER_CONFLICT".into(),
            ));
        }
        let source_owned = supplier.endpoints.iter().any(|current| {
            current.proxy_provider_id.as_deref() == Some(&source.id)
                || current
                    .alternatives
                    .iter()
                    .any(|variant| variant.legacy_proxy_provider_id.as_deref() == Some(&source.id))
        });
        if !source_owned {
            return Err(AppError::Message("supplier_source_changed".into()));
        }
        if supplier
            .endpoints
            .iter()
            .any(|current| current.protocol == endpoint.protocol)
        {
            return Ok(None);
        }
        // Different protocols may share the same API root while addressing
        // different request URLs. The protocol identity above is authoritative;
        // the service reuses the supplier base rather than duplicating it.
        bump_generation(&transaction, generation_to_sql(expected_generation)?)?;
        upsert_endpoint_on_conn(&transaction, supplier_id, supplier.priority, endpoint)?;
        let saved = load_supplier_records_on_conn(&transaction, Some(supplier_id))?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::Message("supplier_not_found".into()))?;
        let added = saved
            .endpoints
            .iter()
            .find(|current| current.protocol == endpoint.protocol)
            .ok_or_else(|| AppError::Message("supplier_endpoint_not_found".into()))?;
        sync_projection_on_conn(&transaction, &saved, added)?;
        let provider_id = projection_id_for(&added.id);
        discovery::store_health(&transaction, &provider_id, health)?;
        transaction.commit()?;
        Ok(Some(provider_id))
    }
}
