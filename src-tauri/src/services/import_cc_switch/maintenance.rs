//! Explicit application-owned maintenance. The cc-switch source is read-only;
//! native authentication homes and unrelated settings are never touched.
use super::{
    cc_switch_db::read_cc_switch_providers, import_policy, settings_extract::extract_upstream_pair,
};
use crate::{
    config::private_fs,
    database::{
        dao::suppliers::{SupplierMaintenanceResult, SupplierMergePlan},
        Database,
    },
    error::AppError,
};
use std::collections::{HashMap, HashSet};

pub(crate) fn maintain_imported_suppliers(
    db: &Database,
) -> Result<SupplierMaintenanceResult, AppError> {
    maintain_imported_suppliers_at(
        db,
        &super::cc_switch_db_path(),
        &crate::config::get_app_config_dir().join("supplier_maintenance_backups"),
    )
}

pub(super) fn maintain_imported_suppliers_at(
    db: &Database,
    source_path: &std::path::Path,
    directory: &std::path::Path,
) -> Result<SupplierMaintenanceResult, AppError> {
    let rows = read_cc_switch_providers(source_path)?;
    let expected_revision = db.supplier_maintenance_revision()?;
    let (generation, records) = db.get_supplier_snapshot()?;
    let proxies = db.get_all_proxy_providers()?;
    let mut source_credentials = HashMap::new();
    let mut provider_refs = HashSet::new();
    let mut converted_source_refs = HashSet::new();
    for row in &rows {
        let Ok(settings) = serde_json::from_str::<serde_json::Value>(&row.settings_config) else {
            continue;
        };
        let (base, key) =
            extract_upstream_pair(&row.app_type, &settings, row.meta.as_ref()).unwrap_or_default();
        if import_policy::excludes_account_auth(row, &settings, &base, &key) {
            source_credentials.insert(
                (row.app_type.clone(), row.id.clone()),
                (base.clone(), key.clone()),
            );
            // Exact source settings or an imported pool linkage prove ownership;
            // locally changed static configurations stay untouched.
            if let Some(saved) = db.get_provider_by_id(&row.id, &row.app_type)? {
                let pair =
                    extract_upstream_pair(&row.app_type, &saved.settings_config, row.meta.as_ref());
                let current_account =
                    import_policy::explicit_account_settings(&row.app_type, &saved.settings_config);
                let source_account =
                    import_policy::explicit_account_settings(&row.app_type, &settings);
                let current_static = pair.as_ref().is_some_and(|(current_base, current_key)| {
                    import_policy::static_configuration(current_base, current_key)
                });
                let coordinate_changed =
                    pair.as_ref().is_some_and(|(current_base, current_key)| {
                        current_base != &base || current_key != &key
                    });
                let managed_static = saved.sync_locked
                    && saved.id == "aag_unified_gateway"
                    && saved.category.as_deref() == Some("aag_unified_client");
                let converted = current_static
                    && !current_account
                    && ((source_account && saved.settings_config != settings)
                        || coordinate_changed
                        || managed_static);
                if converted {
                    // Conversion must replace authentication, not merely change
                    // the label, category, model selection or lock preference.
                    converted_source_refs.insert(super::supplier_import::safe_source_ref(
                        &row.app_type,
                        &row.id,
                    ));
                }
                if !converted
                    && (saved.settings_config == settings
                        || (pair.as_ref() == Some(&(base, key))
                            && proxies
                                .iter()
                                .any(|proxy| proxy.id == format!("ccs-{}", row.id))))
                {
                    provider_refs.insert((row.app_type.clone(), row.id.clone()));
                }
            }
        }
    }
    let mut proxy_ids: HashSet<String> = HashSet::new();
    // Older imports may outlive a recreated cc-switch row's ID. Follow only
    // an unowned legacy projection with its matching saved parent and an
    // exact current-source account-only coordinate; a label/prefix alone
    // cannot establish ownership, and static conversions stay untouched.
    let supplier_owned_ids: HashSet<&str> = records
        .iter()
        .flat_map(|supplier| &supplier.endpoints)
        .flat_map(|endpoint| {
            endpoint.proxy_provider_id.as_deref().into_iter().chain(
                endpoint
                    .alternatives
                    .iter()
                    .filter_map(|variant| variant.legacy_proxy_provider_id.as_deref()),
            )
        })
        .collect();
    for ((app, _), (base, key)) in &source_credentials {
        if !account_only_coordinate(base, key) {
            continue;
        }
        for proxy in &proxies {
            let Some(saved_id) = proxy.id.strip_prefix("ccs-") else {
                continue;
            };
            if supplier_owned_ids.contains(proxy.id.as_str())
                || proxy.base_url != *base
                || proxy.api_key != *key
            {
                continue;
            }
            let Some(saved) = db.get_provider_by_id(saved_id, app)? else {
                continue;
            };
            if extract_upstream_pair(app, &saved.settings_config, None)
                .is_some_and(|(saved_base, saved_key)| saved_base == *base && saved_key == *key)
            {
                proxy_ids.insert(proxy.id.clone());
                provider_refs.insert((app.clone(), saved_id.to_string()));
            }
        }
    }
    // The legacy migration's exact pending identity proves an owned account
    // association. A ccs prefix or passthrough type alone is insufficient.
    for supplier in &records {
        if supplier.source != "official_auth_pending"
            || supplier
                .provenance
                .get("sourceKind")
                .and_then(serde_json::Value::as_str)
                != Some("official_auth_pending")
        {
            continue;
        }
        for proxy in &proxies {
            if proxy.provider_type == "anthropic_passthrough"
                && proxy.api_key.is_empty()
                && supplier.id
                    == crate::database::dao::suppliers::legacy_supplier_id("passthrough", &proxy.id)
            {
                proxy_ids.insert(proxy.id.clone());
            }
        }
    }
    for (source_ref, (base, key)) in &source_credentials {
        if converted_source_refs.contains(&super::supplier_import::safe_source_ref(
            &source_ref.0,
            &source_ref.1,
        )) {
            continue;
        }
        let proxy_id = format!("ccs-{}", source_ref.1);
        if proxies
            .iter()
            .any(|proxy| proxy.id == proxy_id && proxy.base_url == *base && proxy.api_key == *key)
        {
            proxy_ids.insert(proxy_id);
        }
    }
    let source_refs: HashMap<String, &(String, String)> = source_credentials
        .iter()
        .map(|((app, id), credential)| {
            (super::supplier_import::safe_source_ref(app, id), credential)
        })
        .collect();
    let matches_source = |provenance: &serde_json::Value, base: &str, key: &str| {
        provenance
            .get("sourceRecordRef")
            .and_then(serde_json::Value::as_str)
            .filter(|reference| !converted_source_refs.contains(*reference))
            .and_then(|reference| source_refs.get(reference))
            .is_some_and(|(source_base, source_key)| base == source_base && key == source_key)
    };
    let mut oauth_supplier_ids = Vec::new();
    let mut affected_suppliers = HashSet::new();
    for supplier in &records {
        if supplier.source == "official_auth_pending"
            || supplier.source == "legacy_passthrough_pending"
        {
            affected_suppliers.insert(supplier.id.clone());
            let static_part = |provenance: &serde_json::Value, url: &str, key: &str| {
                import_policy::static_configuration(url, key)
                    && !matches_source(provenance, url, key)
            };
            let has_static =
                static_part(&supplier.provenance, &supplier.base_url, &supplier.api_key)
                    || supplier.endpoints.iter().any(|endpoint| {
                        let url = endpoint
                            .endpoint_url
                            .as_deref()
                            .filter(|url| !url.is_empty())
                            .unwrap_or(&supplier.base_url);
                        static_part(
                            &endpoint.provenance,
                            url,
                            endpoint
                                .api_key_override
                                .as_deref()
                                .unwrap_or(&supplier.api_key),
                        ) || endpoint.alternatives.iter().any(|variant| {
                            static_part(&variant.provenance, &variant.base_url, &variant.api_key)
                        })
                    });
            oauth_supplier_ids.push(if has_static {
                format!("convert:{}", supplier.id)
            } else {
                supplier.id.clone()
            });
            continue;
        }
        if supplier.source != "cc_switch" && supplier.source != "legacy_proxy_provider" {
            continue;
        }
        let bound = matches_source(&supplier.provenance, &supplier.base_url, &supplier.api_key);
        let mut parts = Vec::new();
        let mut total = 0;
        for endpoint in &supplier.endpoints {
            total += 1 + endpoint.alternatives.len();
            let key = endpoint
                .api_key_override
                .as_deref()
                .unwrap_or(&supplier.api_key);
            let linked = endpoint
                .proxy_provider_id
                .as_ref()
                .is_some_and(|id| proxy_ids.contains(id));
            let legacy_bound =
                bound && supplier.endpoints.len() == 1 && endpoint.alternatives.is_empty();
            if linked
                || legacy_bound
                || matches_source(
                    &endpoint.provenance,
                    endpoint
                        .endpoint_url
                        .as_deref()
                        .filter(|url| !url.is_empty())
                        .unwrap_or(&supplier.base_url),
                    key,
                )
            {
                parts.push(format!("endpoint:{}", endpoint.id));
            }
            for variant in &endpoint.alternatives {
                if matches_source(&variant.provenance, &variant.base_url, &variant.api_key)
                    || variant
                        .legacy_proxy_provider_id
                        .as_ref()
                        .is_some_and(|id| proxy_ids.contains(id))
                {
                    parts.push(format!("variant:{}", variant.id));
                }
            }
        }
        if (bound && total == 0) || !parts.is_empty() {
            affected_suppliers.insert(supplier.id.clone());
        }
        if (bound && total == 0) || (total > 0 && parts.len() == total) {
            oauth_supplier_ids.push(supplier.id.clone());
        } else {
            oauth_supplier_ids.extend(parts);
        }
    }
    let removed: HashSet<&str> = oauth_supplier_ids.iter().map(String::as_str).collect();
    let mut grouped: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut conflict_count = 0;
    let mut conflict_codes = HashSet::new();
    for supplier in &records {
        if removed.contains(supplier.id.as_str())
            || affected_suppliers.contains(&supplier.id)
            || supplier.api_key.is_empty()
        {
            continue;
        }
        let owned = supplier.source == "cc_switch"
            || (supplier.source == "legacy_proxy_provider"
                && supplier.endpoints.iter().any(|endpoint| {
                    endpoint
                        .proxy_provider_id
                        .as_deref()
                        .is_some_and(|id| id.starts_with("ccs-"))
                }));
        if !owned {
            continue;
        }
        if let Some(identity) = import_policy::vendor_identity(&supplier.name, &supplier.base_url) {
            grouped
                .entry((identity, supplier.api_key.clone()))
                .or_default()
                .push(supplier.id.clone());
        }
    }
    let mut groups = Vec::new();
    for ids in grouped.into_values().filter(|ids| ids.len() > 1) {
        let mut eligible = Vec::new();
        for id in ids {
            let supplier = records.iter().find(|supplier| supplier.id == id).unwrap();
            if let Some(code) = merge_record_conflict(supplier, &proxies) {
                conflict_count += 1;
                conflict_codes.insert(code.to_string());
            } else {
                eligible.push(id);
            }
        }
        if eligible.len() > 1 {
            eligible.sort();
            groups.push(SupplierMergePlan {
                keep_id: eligible.remove(0),
                remove_ids: eligible,
            });
        }
    }
    groups.sort_by(|left, right| left.keep_id.cmp(&right.keep_id));
    if !oauth_supplier_ids.is_empty()
        || !proxy_ids.is_empty()
        || !provider_refs.is_empty()
        || !groups.is_empty()
    {
        private_fs::prepare_managed_private_dir(directory)?;
    }
    let backup = directory.join(format!(
        "before_import_maintenance_{}.db",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%fZ")
    ));
    let mut result = db.maintain_cc_switch_records(
        generation,
        expected_revision,
        &oauth_supplier_ids,
        &proxy_ids.into_iter().collect::<Vec<_>>(),
        &provider_refs.into_iter().collect::<Vec<_>>(),
        &groups,
        &backup,
    )?;
    result.merge_conflict_count = conflict_count;
    result.merge_conflict_codes = conflict_codes.into_iter().collect();
    result.merge_conflict_codes.sort();
    Ok(result)
}

fn account_only_coordinate(base: &str, key: &str) -> bool {
    key.is_empty()
        && reqwest::Url::parse(base).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str() == Some("chatgpt.com")
                && url.port().is_none()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path().trim_end_matches('/') == "/backend-api/codex"
        })
}

fn merge_record_conflict(
    supplier: &crate::database::dao::suppliers::SupplierRecord,
    proxies: &[crate::database::dao::proxy_providers::ProxyProvider],
) -> Option<&'static str> {
    use crate::database::dao::suppliers::{legacy_provider_type, priority_to_legacy_weight};
    let matches_proxy = |id: Option<&str>,
                         protocol: &str,
                         url: &str,
                         key: &str,
                         enabled: bool,
                         priority: i64,
                         models: Vec<&str>,
                         headers: &HashMap<String, String>| {
        let Some(id) = id else {
            return true;
        };
        let Some(proxy) = proxies.iter().find(|proxy| proxy.id == id) else {
            return false;
        };
        let expected_type = legacy_provider_type(protocol).unwrap_or("");
        proxy.provider_type.replace('_', "-") == expected_type
            && proxy.base_url == url
            && proxy.api_key == key
            && proxy.enabled == enabled
            && proxy.weight == priority_to_legacy_weight(supplier.priority, priority)
            && proxy.models.iter().map(String::as_str).collect::<Vec<_>>() == models
            && &proxy.extra_headers == headers
    };
    for endpoint in &supplier.endpoints {
        let key = endpoint
            .api_key_override
            .as_deref()
            .unwrap_or(&supplier.api_key);
        if key != supplier.api_key
            || endpoint
                .alternatives
                .iter()
                .any(|variant| variant.api_key != supplier.api_key)
        {
            return Some("supplier_merge_credential_divergence");
        }
        let url = endpoint
            .endpoint_url
            .as_deref()
            .filter(|url| !url.is_empty())
            .unwrap_or(&supplier.base_url);
        if !matches_proxy(
            endpoint.proxy_provider_id.as_deref(),
            &endpoint.protocol,
            url,
            key,
            supplier.enabled && endpoint.enabled,
            endpoint.priority,
            endpoint
                .models
                .iter()
                .filter(|model| model.enabled)
                .map(|model| model.model_id.as_str())
                .collect(),
            &endpoint.extra_headers,
        ) {
            return Some("supplier_merge_manual_projection_divergence");
        }
        for variant in &endpoint.alternatives {
            if !matches_proxy(
                variant.legacy_proxy_provider_id.as_deref(),
                &endpoint.protocol,
                &variant.base_url,
                &variant.api_key,
                supplier.enabled && variant.enabled,
                variant.priority,
                variant
                    .models
                    .iter()
                    .filter(|model| model.enabled)
                    .map(|model| model.model_id.as_str())
                    .collect(),
                &variant.extra_headers,
            ) {
                return Some("supplier_merge_manual_projection_divergence");
            }
        }
    }
    None
}
