//! Backed-up, transactional cc-switch account cleanup and supplier consolidation.
use super::*;
#[cfg(test)]
use crate::config::private_fs;
use crate::database::operator::create_snapshot_on_conn;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

pub(crate) struct SupplierMergePlan {
    pub keep_id: String,
    pub remove_ids: Vec<String>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SupplierMaintenanceResult {
    pub generation: u64,
    pub removed_oauth_suppliers: usize,
    pub removed_oauth_providers: usize,
    pub merged_suppliers: usize,
    pub backup_created: bool,
    pub preserved_static_suppliers: usize,
    pub merge_conflict_count: usize,
    pub merge_conflict_codes: Vec<String>,
}

impl Database {
    pub(crate) fn maintain_cc_switch_records(
        &self,
        expected_generation: u64,
        expected_revision: u64,
        oauth_supplier_ids: &[String],
        oauth_proxy_ids: &[String],
        oauth_provider_refs: &[(String, String)],
        groups: &[SupplierMergePlan],
        backup_path: &Path,
    ) -> Result<SupplierMaintenanceResult, AppError> {
        let converted_targets: Vec<String> = oauth_supplier_ids
            .iter()
            .filter_map(|id| id.strip_prefix("convert:").map(str::to_string))
            .collect();
        let endpoint_targets: HashSet<String> = oauth_supplier_ids
            .iter()
            .filter_map(|id| id.strip_prefix("endpoint:").map(str::to_string))
            .collect();
        let variant_targets: HashSet<String> = oauth_supplier_ids
            .iter()
            .filter_map(|id| id.strip_prefix("variant:").map(str::to_string))
            .collect();
        let supplier_targets: Vec<String> = oauth_supplier_ids
            .iter()
            .filter(|id| {
                !id.starts_with("endpoint:")
                    && !id.starts_with("variant:")
                    && !id.starts_with("convert:")
            })
            .cloned()
            .collect();
        let mut conn = lock_conn!(self.conn);
        if maintenance_revision_on_conn(&conn)? != expected_revision {
            return Err(AppError::Message(
                "supplier_maintenance_concurrent_change".into(),
            ));
        }
        if read_generation_on_conn(&conn)? != expected_generation {
            return Err(AppError::Message(
                "supplier_config_generation_conflict".into(),
            ));
        }
        if oauth_supplier_ids.is_empty()
            && oauth_proxy_ids.is_empty()
            && oauth_provider_refs.is_empty()
            && groups.is_empty()
        {
            return Ok(SupplierMaintenanceResult {
                generation: expected_generation,
                removed_oauth_suppliers: 0,
                removed_oauth_providers: 0,
                merged_suppliers: 0,
                backup_created: false,
                preserved_static_suppliers: 0,
                merge_conflict_count: 0,
                merge_conflict_codes: Vec::new(),
            });
        }
        let data_version: i64 = conn.pragma_query_value(None, "data_version", |row| row.get(0))?;
        create_snapshot_on_conn(&conn, backup_path)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))? != data_version
        {
            return Err(AppError::Message(
                "supplier_maintenance_concurrent_change".into(),
            ));
        }
        if maintenance_revision_on_conn(&tx)? != expected_revision {
            return Err(AppError::Message(
                "supplier_maintenance_concurrent_change".into(),
            ));
        }
        bump_generation(&tx, generation_to_sql(expected_generation)?)?;
        for id in &converted_targets {
            tx.execute("UPDATE suppliers SET source='manual',provenance_json='{\"sourceKind\":\"manual\"}' WHERE id=?1 AND source IN ('official_auth_pending','legacy_passthrough_pending')",[id])?;
        }
        let mut removed_ids: HashSet<String> = oauth_proxy_ids.iter().cloned().collect();
        for id in &supplier_targets {
            for (proxy_id, _) in projection_rows_for_supplier(&tx, id)? {
                removed_ids.insert(proxy_id);
            }
            for endpoint in load_endpoints_on_conn(&tx, id)? {
                for variant in endpoint.alternatives {
                    if let Some(id) = variant.legacy_proxy_provider_id {
                        removed_ids.insert(id);
                    }
                }
            }
        }
        let records = load_supplier_records_on_conn(&tx, None)?;
        for supplier in &records {
            for endpoint in &supplier.endpoints {
                if endpoint_targets.contains(&endpoint.id) {
                    if let Some(id) = &endpoint.proxy_provider_id {
                        removed_ids.insert(id.clone());
                    }
                }
                for variant in &endpoint.alternatives {
                    if variant_targets.contains(&variant.id) {
                        if let Some(id) = &variant.legacy_proxy_provider_id {
                            removed_ids.insert(id.clone());
                        }
                    }
                }
            }
        }
        rewrite_route_references(&tx, &HashMap::new(), &removed_ids)?;
        for supplier in records {
            if supplier_targets.contains(&supplier.id) {
                continue;
            }
            let primary_changed = supplier
                .endpoints
                .iter()
                .any(|endpoint| endpoint_targets.contains(&endpoint.id));
            for endpoint in supplier.endpoints {
                if endpoint_targets.contains(&endpoint.id)
                    || endpoint
                        .alternatives
                        .iter()
                        .any(|variant| variant_targets.contains(&variant.id))
                {
                    prune_oauth_endpoint_on_conn(
                        &tx,
                        endpoint,
                        &endpoint_targets,
                        &variant_targets,
                    )?;
                }
            }
            if primary_changed {
                if let Some(updated) = load_supplier_records_on_conn(&tx, Some(&supplier.id))?
                    .into_iter()
                    .next()
                {
                    if let Some(first) = updated.endpoints.first() {
                        let base = first
                            .endpoint_url
                            .as_deref()
                            .filter(|url| !url.is_empty())
                            .unwrap_or(&updated.base_url)
                            .to_string();
                        let key = first
                            .api_key_override
                            .as_deref()
                            .unwrap_or(&updated.api_key)
                            .to_string();
                        for valid in &updated.endpoints {
                            if valid.endpoint_url.as_deref().is_none_or(str::is_empty)
                                && updated.base_url != base
                            {
                                tx.execute(
                                    "UPDATE supplier_endpoints SET endpoint_url=?1 WHERE id=?2",
                                    params![updated.base_url, valid.id],
                                )?;
                            }
                            if valid.api_key_override.is_none() && updated.api_key != key {
                                tx.execute(
                                    "UPDATE supplier_endpoints SET api_key_override=?1 WHERE id=?2",
                                    params![updated.api_key, valid.id],
                                )?;
                            }
                        }
                        tx.execute("UPDATE suppliers SET base_url=?1,api_key=?2,provenance_json=?3 WHERE id=?4",params![base,key,serde_json::to_string(&first.provenance)?,updated.id])?;
                    }
                }
            }
        }
        for id in &supplier_targets {
            tx.execute("DELETE FROM suppliers WHERE id=?1", [id])?;
        }
        let mut removed_claude_account = false;
        for id in &removed_ids {
            removed_claude_account |= is_claude_passthrough_on_conn(&tx, id)?;
        }
        if removed_claude_account {
            tx.execute("INSERT INTO app_meta(key,value) VALUES ('claude_mirror_mode','token') ON CONFLICT(key) DO UPDATE SET value='token'",[])?;
        }
        for id in &removed_ids {
            tx.execute(
                "DELETE FROM supplier_runtime_projections WHERE proxy_provider_id=?1",
                [id],
            )?;
            tx.execute("DELETE FROM proxy_providers WHERE id=?1", [id])?;
        }
        let mut removed_providers = 0;
        for (app, id) in oauth_provider_refs {
            tx.execute(
                "DELETE FROM provider_endpoints WHERE provider_id=?1 AND app_type=?2",
                params![id, app],
            )?;
            removed_providers += tx.execute(
                "DELETE FROM providers WHERE id=?1 AND app_type=?2",
                params![id, app],
            )?;
        }
        let pin: Option<String> = tx
            .query_row(
                "SELECT value FROM app_meta WHERE key='proxy_pinned_provider'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if pin.is_some_and(|id| removed_ids.contains(&id)) {
            tx.execute("DELETE FROM app_meta WHERE key='proxy_pinned_provider'", [])?;
        }
        let mut merged_suppliers = 0;
        for group in groups {
            for remove_id in &group.remove_ids {
                merge_supplier_on_conn(&tx, &group.keep_id, remove_id)?;
                merged_suppliers += 1;
            }
        }
        if !removed_ids.is_empty() || removed_providers > 0 {
            tx.execute("DELETE FROM app_meta WHERE key IN ('model_discovery_last_report_json','model_discovery_last_run_epoch')",[])?;
        }
        tx.execute("INSERT INTO app_meta(key,value) VALUES (?1,'1') ON CONFLICT(key) DO UPDATE SET value='1'",[KEY_SUPPLIER_IMPORT_SETUP_PENDING])?;
        tx.commit()?;
        Ok(SupplierMaintenanceResult {
            generation: expected_generation + 1,
            removed_oauth_suppliers: supplier_targets.len(),
            removed_oauth_providers: removed_providers + removed_ids.len(),
            merged_suppliers,
            backup_created: true,
            preserved_static_suppliers: converted_targets.len(),
            merge_conflict_count: 0,
            merge_conflict_codes: Vec::new(),
        })
    }

    pub(crate) fn supplier_maintenance_revision(&self) -> Result<u64, AppError> {
        let conn = lock_conn!(self.conn);
        let tx = conn.unchecked_transaction()?;
        let revision = maintenance_revision_on_conn(&tx)?;
        tx.commit()?;
        Ok(revision)
    }

    pub(crate) fn supplier_variant_projection_ids(&self) -> Result<Vec<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let suppliers = load_supplier_records_on_conn(&conn, None)?;
        Ok(suppliers
            .into_iter()
            .flat_map(|supplier| supplier.endpoints)
            .flat_map(|endpoint| endpoint.alternatives)
            .filter_map(|variant| variant.legacy_proxy_provider_id)
            .collect())
    }
}

fn is_claude_passthrough_on_conn(conn: &Connection, id: &str) -> Result<bool, AppError> {
    Ok(conn
        .query_row(
            "SELECT type='anthropic_passthrough' FROM proxy_providers WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

fn maintenance_revision_on_conn(conn: &Connection) -> Result<u64, AppError> {
    use std::hash::{Hash, Hasher};
    let mut revision = std::collections::hash_map::DefaultHasher::new();
    for query in [
        "SELECT * FROM suppliers ORDER BY id",
        "SELECT * FROM supplier_endpoints ORDER BY id",
        "SELECT * FROM supplier_models ORDER BY id",
        "SELECT * FROM supplier_runtime_projections ORDER BY endpoint_id",
        "SELECT * FROM providers ORDER BY app_type,id",
        "SELECT * FROM provider_endpoints ORDER BY id",
        "SELECT * FROM proxy_providers ORDER BY id",
        "SELECT * FROM routes ORDER BY alias",
        "SELECT * FROM supplier_config_meta ORDER BY id",
        "SELECT key,value FROM app_meta WHERE key IN ('proxy_pinned_provider','claude_mirror_mode','codex_mirror_mode','unified_default_model','supplier_import_setup_pending') ORDER BY key",
    ] {
        query.hash(&mut revision);
        let mut statement = conn.prepare(query)?;
        let count = statement.column_count();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            for index in 0..count {
                use rusqlite::types::ValueRef;
                let value = row.get_ref(index)?;
                std::mem::discriminant(&value).hash(&mut revision);
                match value {
                    ValueRef::Null => {}
                    ValueRef::Integer(value) => value.hash(&mut revision),
                    ValueRef::Real(value) => value.to_bits().hash(&mut revision),
                    ValueRef::Text(value) | ValueRef::Blob(value) => value.hash(&mut revision),
                }
            }
        }
    }
    Ok(revision.finish())
}

fn prune_oauth_endpoint_on_conn(
    conn: &Connection,
    endpoint: SupplierEndpointRecord,
    primary_targets: &HashSet<String>,
    variant_targets: &HashSet<String>,
) -> Result<(), AppError> {
    let mut valid: Vec<_> = endpoint
        .alternatives
        .into_iter()
        .filter(|variant| !variant_targets.contains(&variant.id))
        .collect();
    if !primary_targets.contains(&endpoint.id) {
        conn.execute(
            "UPDATE supplier_endpoints SET alternatives_json=?1 WHERE id=?2",
            params![serde_json::to_string(&valid)?, endpoint.id],
        )?;
        return Ok(());
    }
    conn.execute(
        "DELETE FROM supplier_runtime_projections WHERE endpoint_id=?1",
        [&endpoint.id],
    )?;
    if valid.is_empty() {
        conn.execute("DELETE FROM supplier_endpoints WHERE id=?1", [&endpoint.id])?;
        return Ok(());
    }
    let promoted = valid.remove(0);
    conn.execute("UPDATE supplier_endpoints SET endpoint_url=?1,api_key_override=?2,enabled=?3,priority=?4,extra_headers=?5,source=?6,provenance_json=?7,alternatives_json=?8 WHERE id=?9",params![promoted.base_url,promoted.api_key,promoted.enabled,promoted.priority,serde_json::to_string(&promoted.extra_headers)?,promoted.source,serde_json::to_string(&promoted.provenance)?,serde_json::to_string(&valid)?,endpoint.id])?;
    conn.execute(
        "DELETE FROM supplier_models WHERE endpoint_id=?1",
        [&endpoint.id],
    )?;
    for model in promoted.models {
        conn.execute("INSERT INTO supplier_models(id,endpoint_id,model_id,display_name,enabled,supports_tools,supports_images,context_window,max_output_tokens,tier_override,source,provenance_json,sort_index) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",params![model_id_for(&endpoint.id,&model.model_id),endpoint.id,model.model_id,model.display_name,model.enabled,model.supports_tools,model.supports_images,model.context_window,model.max_output_tokens,model.tier_override,model.source,serde_json::to_string(&model.provenance)?,model.sort_index])?;
    }
    if let Some(proxy_id) = promoted.legacy_proxy_provider_id {
        conn.execute("INSERT INTO supplier_runtime_projections(endpoint_id,proxy_provider_id,managed) VALUES (?1,?2,1)",params![endpoint.id,proxy_id])?;
    }
    Ok(())
}

fn rewrite_route_references(
    conn: &Connection,
    replacements: &HashMap<String, String>,
    removed: &HashSet<String>,
) -> Result<(), AppError> {
    let rows: Vec<(String, String)> = {
        let mut stmt = conn.prepare("SELECT alias,hops FROM routes")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    for (alias, raw) in rows {
        let mut hops: Vec<crate::database::dao::routes::RouteHop> = serde_json::from_str(&raw)?;
        let before = serde_json::to_string(&hops)?;
        hops.retain(|hop| !removed.contains(&hop.provider));
        for hop in &mut hops {
            if let Some(id) = replacements.get(&hop.provider) {
                hop.provider = id.clone();
            }
        }
        let mut seen = HashSet::new();
        hops.retain(|hop| seen.insert((hop.provider.clone(), hop.model.clone())));
        if hops.is_empty() {
            conn.execute("DELETE FROM routes WHERE alias=?1", [&alias])?;
        } else {
            let after = serde_json::to_string(&hops)?;
            if after != before {
                conn.execute(
                    "UPDATE routes SET hops=?1 WHERE alias=?2",
                    params![after, alias],
                )?;
            }
        }
    }
    Ok(())
}

fn merge_supplier_on_conn(
    conn: &Connection,
    keep_id: &str,
    remove_id: &str,
) -> Result<(), AppError> {
    let mut keep = load_supplier_records_on_conn(conn, Some(keep_id))?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::InvalidInput("supplier_not_found".into()))?;
    let remove = load_supplier_records_on_conn(conn, Some(remove_id))?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::InvalidInput("supplier_not_found".into()))?;
    let account_matches = |supplier: &SupplierRecord| {
        supplier.endpoints.iter().all(|endpoint| {
            endpoint
                .api_key_override
                .as_deref()
                .unwrap_or(&supplier.api_key)
                == supplier.api_key
                && endpoint
                    .alternatives
                    .iter()
                    .all(|variant| variant.api_key == supplier.api_key)
        })
    };
    if !account_matches(&keep) || !account_matches(&remove) {
        return Err(AppError::Message("supplier_merge_account_conflict".into()));
    }
    if keep.api_key != remove.api_key || keep.api_key.is_empty() {
        return Err(AppError::Message("supplier_merge_account_conflict".into()));
    }
    super::origins::retain_merge_origins_on_conn(conn, keep_id, remove_id)?;
    // Enabling the merged supplier preserves each original endpoint's effective state.
    if !keep.enabled && remove.enabled {
        for endpoint in &mut keep.endpoints {
            for alternative in &mut endpoint.alternatives {
                alternative.enabled = false;
            }
            conn.execute(
                "UPDATE supplier_endpoints SET enabled=0,alternatives_json=?1 WHERE id=?2",
                params![serde_json::to_string(&endpoint.alternatives)?, endpoint.id],
            )?;
        }
        keep.enabled = true;
    }
    conn.execute(
        "UPDATE suppliers SET enabled=?1 WHERE id=?2",
        params![keep.enabled || remove.enabled, keep_id],
    )?;
    for endpoint in remove.endpoints {
        let target_id = endpoint_id_for(keep_id, &endpoint.protocol);
        let existing = load_endpoints_on_conn(conn, keep_id)?
            .into_iter()
            .find(|item| item.protocol == endpoint.protocol);
        let effective_url = endpoint
            .endpoint_url
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or(&remove.base_url)
            .to_string();
        let effective_key = endpoint
            .api_key_override
            .as_deref()
            .unwrap_or(&remove.api_key)
            .to_string();
        let priority = remove
            .priority
            .saturating_add(endpoint.priority)
            .saturating_sub(keep.priority);
        if let Some(mut target) = existing {
            // Keep every alternate URL, header set, model override and old proxy id.
            target.alternatives.push(SupplierEndpointAlternativeRecord {
                id: endpoint.id.clone(),
                base_url: effective_url,
                api_key: effective_key,
                enabled: remove.enabled && endpoint.enabled,
                priority,
                extra_headers: endpoint.extra_headers,
                source: endpoint.source,
                provenance: endpoint.provenance,
                legacy_proxy_provider_id: endpoint.proxy_provider_id.clone(),
                models: endpoint.models,
            });
            target.alternatives.extend(endpoint.alternatives);
            if target.alternatives.len() > 64 {
                return Err(AppError::InvalidInput(
                    "supplier_alternatives_too_many".into(),
                ));
            }
            conn.execute(
                "UPDATE supplier_endpoints SET alternatives_json=?1 WHERE id=?2",
                params![serde_json::to_string(&target.alternatives)?, target.id],
            )?;
            conn.execute(
                "DELETE FROM supplier_runtime_projections WHERE endpoint_id=?1",
                [&endpoint.id],
            )?;
            conn.execute("DELETE FROM supplier_endpoints WHERE id=?1", [&endpoint.id])?;
        } else {
            conn.execute("INSERT INTO supplier_endpoints (id,supplier_id,protocol,endpoint_url,api_key_override,enabled,priority,extra_headers,source,provenance_json,created_at,updated_at,alternatives_json) SELECT ?1,?2,protocol,?3,?4,?5,?6,extra_headers,source,provenance_json,created_at,updated_at,alternatives_json FROM supplier_endpoints WHERE id=?7",params![target_id,keep_id,effective_url,effective_key,remove.enabled&&endpoint.enabled,priority,endpoint.id])?;
            for model in &endpoint.models {
                conn.execute("INSERT INTO supplier_models (id,endpoint_id,model_id,display_name,enabled,supports_tools,supports_images,context_window,max_output_tokens,tier_override,source,provenance_json,sort_index,created_at,updated_at) SELECT ?1,?2,model_id,display_name,enabled,supports_tools,supports_images,context_window,max_output_tokens,tier_override,source,provenance_json,sort_index,created_at,updated_at FROM supplier_models WHERE id=?3",params![model_id_for(&target_id,&model.model_id),target_id,model.id])?;
            }
            conn.execute(
                "UPDATE supplier_runtime_projections SET endpoint_id=?1 WHERE endpoint_id=?2",
                params![target_id, endpoint.id],
            )?;
            conn.execute("DELETE FROM supplier_endpoints WHERE id=?1", [&endpoint.id])?;
        }
    }
    conn.execute("DELETE FROM suppliers WHERE id=?1", [remove_id])?;
    let updated = load_supplier_records_on_conn(conn, Some(keep_id))?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::InvalidInput("supplier_not_found".into()))?;
    for endpoint in &updated.endpoints {
        sync_projection_on_conn(conn, &updated, endpoint)?;
    }
    Ok(())
}

pub(super) fn sync_variant_projections_on_conn(
    conn: &Connection,
    supplier: &SupplierRecord,
    endpoint: &SupplierEndpointRecord,
) -> Result<(), AppError> {
    let Some(provider_type) = legacy_provider_type(&endpoint.protocol) else {
        return Ok(());
    };
    let mut variants = endpoint.alternatives.clone();
    let mut created = false;
    for variant in &mut variants {
        let new_id = projection_id_for(&variant.id);
        let id = variant
            .legacy_proxy_provider_id
            .clone()
            .unwrap_or_else(|| new_id.clone());
        if variant.legacy_proxy_provider_id.is_none() {
            if conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM proxy_providers WHERE id=?1)",
                [&id],
                |row| row.get::<_, bool>(0),
            )? {
                return Err(AppError::Message("supplier_projection_id_conflict".into()));
            }
            conn.execute("INSERT INTO proxy_providers(id,name,type,api_key,base_url,models,enabled,timeout_ms,weight,circuit_breaker,model_mapping,extra_headers,auto_track_models,created_at) VALUES (?1,?2,?3,?4,?5,'[]',0,60000,1,?6,'{}','{}',1,?7)",params![id,format!("{} ({})",supplier.name,endpoint.protocol),provider_type,variant.api_key,variant.base_url,serde_json::to_string(&crate::database::dao::proxy_providers::CircuitBreakerConfig::default())?,chrono::Utc::now().timestamp()])?;
            variant.legacy_proxy_provider_id = Some(id.clone());
            created = true;
        }
        let models: Vec<_> = variant
            .models
            .iter()
            .filter(|model| model.enabled)
            .map(|model| model.model_id.as_str())
            .collect();
        let changed=conn.execute("UPDATE proxy_providers SET name=?1,type=?2,api_key=?3,base_url=?4,models=?5,enabled=?6,weight=?7,extra_headers=?8 WHERE id=?9",params![format!("{} ({})",supplier.name,endpoint.protocol),provider_type,variant.api_key,variant.base_url,serde_json::to_string(&models)?,supplier.enabled&&variant.enabled,priority_to_legacy_weight(supplier.priority,variant.priority),serde_json::to_string(&variant.extra_headers)?,id])?;
        if changed != 1 {
            return Err(AppError::Database(
                "supplier variant projection is missing".into(),
            ));
        }
    }
    if created {
        conn.execute(
            "UPDATE supplier_endpoints SET alternatives_json=?1 WHERE id=?2",
            params![serde_json::to_string(&variants)?, endpoint.id],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::suppliers::{
        list_suppliers, resolved_supplier_snapshot, save_supplier, SupplierSaveInput,
    };
    use serde_json::json;

    fn supplier(db: &Database, id: &str, url: &str, key: &str, generation: u64) -> u64 {
        let input:SupplierSaveInput=serde_json::from_value(json!({"id":id,"expectedGeneration":generation,"name":"GLM","baseUrl":url,"apiKey":key,"source":"cc_switch","endpoints":[{"protocol":"anthropic","models":[{"modelId":format!("model-{id}"),"tierOverride":2}]}]})).unwrap();
        save_supplier(db, input).unwrap().generation
    }

    fn backup_root() -> private_fs::PrivateTempDir {
        private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap()).unwrap()
    }

    #[test]
    fn consolidation_preserves_variant_models_headers_routes_and_normal_edits() {
        let db = Database::memory().unwrap();
        let generation = supplier(
            &db,
            "first",
            "https://open.bigmodel.cn/anthropic",
            "fixture_key",
            0,
        );
        let generation = supplier(
            &db,
            "second",
            "https://open.bigmodel.cn/other",
            "fixture_key",
            generation,
        );
        db.conn.lock().unwrap().execute("UPDATE suppliers SET name='GLM imported',icon='moon',created_at=111,updated_at=222 WHERE id='second'",[]).unwrap();
        let source = db
            .get_supplier_record_snapshot("second")
            .unwrap()
            .1
            .unwrap();
        let old_proxy = source.endpoints[0].proxy_provider_id.clone().unwrap();
        db.save_route(&crate::database::dao::routes::Route {
            alias: "retained-route".into(),
            description: String::new(),
            hops: vec![crate::database::dao::routes::RouteHop {
                provider: old_proxy.clone(),
                model: "model-second".into(),
            }],
        })
        .unwrap();
        let root = backup_root();
        let backup = root.path().join("before.db");
        let result = db
            .maintain_cc_switch_records(
                generation,
                db.supplier_maintenance_revision().unwrap(),
                &[],
                &[],
                &[],
                &[SupplierMergePlan {
                    keep_id: "first".into(),
                    remove_ids: vec!["second".into()],
                }],
                &backup,
            )
            .unwrap();
        assert!(result.backup_created);
        assert_eq!(result.merged_suppliers, 1);
        let list = list_suppliers(&db).unwrap();
        assert_eq!(list.suppliers.len(), 1);
        let endpoint = &list.suppliers[0].endpoints[0];
        assert_eq!(endpoint.alternatives.len(), 1);
        assert_eq!(
            endpoint.alternatives[0].endpoint_url.as_deref(),
            Some("https://open.bigmodel.cn/other")
        );
        let runtime = resolved_supplier_snapshot(&db).unwrap();
        assert_eq!(runtime.endpoints.len(), 2);
        assert_eq!(
            runtime
                .endpoints
                .iter()
                .find(|e| e.base_url.ends_with("other"))
                .unwrap()
                .models[0]
                .tier_override,
            Some(2)
        );
        assert!(db.get_proxy_provider(&old_proxy).unwrap().is_some());
        assert_eq!(db.get_all_routes().unwrap()[0].hops[0].provider, old_proxy);
        let model_update=crate::services::suppliers::update_supplier_models(&db,serde_json::from_value(json!({"supplierId":"first","protocol":"anthropic","expectedGeneration":result.generation,"models":[{"modelId":"changed-primary"}]})).unwrap()).unwrap();
        assert_eq!(
            db.get_supplier_record_snapshot("first")
                .unwrap()
                .1
                .unwrap()
                .endpoints[0]
                .alternatives[0]
                .models[0]
                .tier_override,
            Some(2)
        );
        let raw_origins: String = db
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT import_origins_json FROM suppliers WHERE id='first'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let origins: serde_json::Value = serde_json::from_str(&raw_origins).unwrap();
        let origin = origins
            .as_array()
            .unwrap()
            .iter()
            .find(|origin| origin["supplier"][0]["id"] == "second")
            .unwrap();
        assert_eq!(origin["supplier"][0]["name"], "GLM imported");
        assert_eq!(origin["supplier"][0]["icon"], "moon");
        assert_eq!(origin["supplier"][0]["created_at"], 111);
        let input:SupplierSaveInput=serde_json::from_value(json!({"id":"first","expectedGeneration":model_update.generation,"name":"Renamed","enabled":false})).unwrap();
        let after = save_supplier(&db, input).unwrap();
        assert!(
            db.get_supplier_record_snapshot("first")
                .unwrap()
                .1
                .unwrap()
                .endpoints[0]
                .alternatives
                .len()
                == 1
        );
        assert!(!db.get_proxy_provider(&old_proxy).unwrap().unwrap().enabled);
        assert!(resolved_supplier_snapshot(&db)
            .unwrap()
            .endpoints
            .is_empty());
        assert_eq!(after.generation, model_update.generation + 1);
        let backup =
            Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        assert_eq!(
            backup
                .query_row("SELECT COUNT(*) FROM suppliers", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn oauth_cleanup_preserves_valid_suppliers_and_prunes_only_matching_route_hops() {
        let db = Database::memory().unwrap();
        let generation = supplier(
            &db,
            "valid",
            "https://valid.example.invalid",
            "fixture_valid",
            0,
        );
        let conn = db.conn.lock().unwrap();
        conn.execute("INSERT INTO suppliers(id,name,base_url,api_key,enabled,priority,source,provenance_json) VALUES ('oauth','Removed account','','',0,100,'official_auth_pending','{}')",[]).unwrap();
        conn.execute("INSERT INTO providers(id,app_type,name,settings_config,is_current) VALUES ('login','codex','Removed account','{}',1)",[]).unwrap();
        drop(conn);
        let root = backup_root();
        let result = db
            .maintain_cc_switch_records(
                generation,
                db.supplier_maintenance_revision().unwrap(),
                &["oauth".into()],
                &[],
                &[("codex".into(), "login".into())],
                &[],
                &root.path().join("before.db"),
            )
            .unwrap();
        assert_eq!(result.removed_oauth_suppliers, 1);
        assert_eq!(result.removed_oauth_providers, 1);
        assert_eq!(list_suppliers(&db).unwrap().suppliers[0].id, "valid");
        assert!(db.get_current_provider("codex").unwrap().is_none());
        assert!(db.get_setting("claude_mirror_mode").unwrap().is_none());
        let no_op = db
            .maintain_cc_switch_records(
                result.generation,
                db.supplier_maintenance_revision().unwrap(),
                &[],
                &[],
                &[],
                &[],
                &root.path().join("unused.db"),
            )
            .unwrap();
        assert!(!no_op.backup_created);
        assert!(!root.path().join("unused.db").exists());
    }

    #[test]
    fn failed_merge_rolls_back_data_and_generation_with_recovery_intact() {
        let db = Database::memory().unwrap();
        let generation = supplier(
            &db,
            "first",
            "https://first.example.invalid",
            "fixture_same",
            0,
        );
        let generation = supplier(
            &db,
            "second",
            "https://second.example.invalid",
            "fixture_same",
            generation,
        );
        db.conn.lock().unwrap().execute_batch("CREATE TRIGGER refuse_merge BEFORE DELETE ON suppliers BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        let root = backup_root();
        let backup = root.path().join("recovery.db");
        assert!(db
            .maintain_cc_switch_records(
                generation,
                db.supplier_maintenance_revision().unwrap(),
                &[],
                &[],
                &[],
                &[SupplierMergePlan {
                    keep_id: "first".into(),
                    remove_ids: vec!["second".into()]
                }],
                &backup
            )
            .is_err());
        assert_eq!(db.get_supplier_snapshot().unwrap().1.len(), 2);
        assert_eq!(db.supplier_config_generation().unwrap(), generation);
        assert!(backup.exists());
        let conn = db.conn.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            0
        );
    }

    #[test]
    fn distinct_accounts_never_merge_even_if_requested_by_a_stale_plan() {
        let db = Database::memory().unwrap();
        let generation = supplier(
            &db,
            "first",
            "https://open.bigmodel.cn/one",
            "fixture_one",
            0,
        );
        let generation = supplier(
            &db,
            "second",
            "https://open.bigmodel.cn/two",
            "fixture_two",
            generation,
        );
        let root = backup_root();
        assert!(db
            .maintain_cc_switch_records(
                generation,
                db.supplier_maintenance_revision().unwrap(),
                &[],
                &[],
                &[],
                &[SupplierMergePlan {
                    keep_id: "first".into(),
                    remove_ids: vec!["second".into()]
                }],
                &root.path().join("before.db")
            )
            .is_err());
        assert_eq!(db.get_supplier_snapshot().unwrap().1.len(), 2);
        assert_eq!(db.supplier_config_generation().unwrap(), generation);
    }

    #[test]
    fn stale_proxy_or_provider_plan_cannot_remove_a_valid_replacement() {
        let db = Database::memory().unwrap();
        let initial=serde_json::from_value(serde_json::json!({"id":"ccs-account","name":"Old account","type":"anthropic_passthrough","baseURL":"https://old.example.invalid","models":[]})).unwrap();
        db.save_proxy_provider(&initial).unwrap();
        let revision = db.supplier_maintenance_revision().unwrap();
        let replacement=serde_json::from_value(serde_json::json!({"id":"ccs-account","name":"Valid API","type":"openai","apiKey":"fixture_valid","baseURL":"https://valid.example.invalid","models":["model"]})).unwrap();
        db.save_proxy_provider(&replacement).unwrap();
        let root = backup_root();
        assert!(db
            .maintain_cc_switch_records(
                0,
                revision,
                &[],
                &["ccs-account".into()],
                &[],
                &[],
                &root.path().join("stale.db")
            )
            .is_err());
        assert_eq!(
            db.get_proxy_provider("ccs-account")
                .unwrap()
                .unwrap()
                .api_key,
            "fixture_valid"
        );
        assert!(!root.path().join("stale.db").exists());
        let old = crate::provider::Provider::new(
            "source".into(),
            "Old".into(),
            serde_json::json!({"auth":{"refresh_token":"fixture_refresh"}}),
        );
        db.save_provider("codex", &old).unwrap();
        let revision = db.supplier_maintenance_revision().unwrap();
        let valid = crate::provider::Provider::new(
            "source".into(),
            "New static API".into(),
            serde_json::json!({"auth":{"OPENAI_API_KEY":"fixture_valid"}}),
        );
        db.upsert_provider("codex", &valid).unwrap();
        assert!(db
            .maintain_cc_switch_records(
                0,
                revision,
                &[],
                &[],
                &[("codex".into(), "source".into())],
                &[],
                &root.path().join("stale-provider.db")
            )
            .is_err());
        assert_eq!(
            db.get_provider_by_id("source", "codex")
                .unwrap()
                .unwrap()
                .name,
            "New static API"
        );
    }

    #[test]
    fn setup_pending_survives_database_reopen_and_no_change_maintenance() {
        let root = backup_root();
        let db_path = root.path().join("state/aag.db");
        let db = Database::init_at_path(&db_path).unwrap();
        db.conn.lock().unwrap().execute("INSERT INTO suppliers(id,name,base_url,api_key,enabled,priority,source,provenance_json) VALUES ('oauth','Account','','',0,100,'official_auth_pending','{}')",[]).unwrap();
        db.maintain_cc_switch_records(
            0,
            db.supplier_maintenance_revision().unwrap(),
            &["oauth".into()],
            &[],
            &[],
            &[],
            &root.path().join("before.db"),
        )
        .unwrap();
        assert_eq!(
            db.get_setting(KEY_SUPPLIER_IMPORT_SETUP_PENDING)
                .unwrap()
                .as_deref(),
            Some("1")
        );
        drop(db);
        let reopened = Database::init_at_path(&db_path).unwrap();
        let no_change = reopened
            .maintain_cc_switch_records(
                1,
                reopened.supplier_maintenance_revision().unwrap(),
                &[],
                &[],
                &[],
                &[],
                &root.path().join("unused.db"),
            )
            .unwrap();
        assert!(!no_change.backup_created);
        assert_eq!(
            reopened
                .get_setting(KEY_SUPPLIER_IMPORT_SETUP_PENDING)
                .unwrap()
                .as_deref(),
            Some("1")
        );
    }
}
