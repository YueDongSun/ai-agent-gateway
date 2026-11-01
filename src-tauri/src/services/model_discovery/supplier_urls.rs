//! Discover and append validated protocols for an already configured supplier.
use super::{apply::ProviderPlan, supplier_preflight, url_candidates, ProviderReport};
use crate::database::{
    dao::{
        provider_admission::provider_configuration_identity,
        proxy_providers::ProxyProvider,
        suppliers::{
            endpoint_id_for, projection_id_for, SupplierDiscoveryHealth,
            SupplierEndpointPersistInput, SupplierModelPersistInput,
        },
    },
    Database,
};
use crate::proxy::providers::AnthropicAuthScheme;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct DiscoveryOutcome {
    pub changed: bool,
    pub entries: Vec<ProviderReport>,
}

fn search_unavailable(reason: &str) -> bool {
    matches!(
        reason,
        "openapi_http_404"
            | "openapi_http_405"
            | "openapi_document_invalid"
            | "openapi_paths_missing"
            | "openapi_unavailable"
            | "supplier_url_discovery_unsupported"
    )
}

fn entry(
    supplier_id: &str,
    provider_id: &str,
    protocol: &str,
    stage: &'static str,
    skipped: bool,
    reason: String,
) -> ProviderReport {
    supplier_preflight::record_outcome(
        supplier_id,
        provider_id,
        protocol,
        stage,
        if skipped { "skipped" } else { "failed" },
        &reason,
    );
    ProviderReport {
        provider_id: provider_id.into(),
        ok: skipped,
        error: Some(format!(
            "supplier_url_{}:{protocol}:{stage}:{reason}",
            if skipped { "skipped" } else { "failed" }
        )),
        plan: ProviderPlan::default(),
    }
}

pub(super) async fn discover(
    db: &Database,
    supplier_id: &str,
    source: &ProxyProvider,
    auth_scheme: AnthropicAuthScheme,
) -> DiscoveryOutcome {
    let mut outcome = DiscoveryOutcome::default();
    let snapshot = db.get_supplier_record_snapshot(supplier_id);
    let (mut generation, supplier) = match snapshot {
        Ok((generation, Some(supplier))) => (generation, supplier),
        Ok(_) => return outcome,
        Err(error) => {
            outcome.entries.push(entry(
                supplier_id,
                &source.id,
                "unknown",
                "configuration",
                false,
                error.external_message(),
            ));
            return outcome;
        }
    };
    let mut existing: BTreeSet<String> = supplier
        .endpoints
        .iter()
        .map(|endpoint| endpoint.protocol.clone())
        .collect();
    if crate::database::dao::suppliers::SUPPORTED_PROTOCOLS
        .iter()
        .all(|protocol| existing.contains(*protocol))
    {
        return outcome;
    }
    // Catalog reconciliation may have extended source.models. Only an unchanged
    // transport can authorize the same-origin credential context used below.
    let source = match db.get_proxy_provider(&source.id) {
        Ok(Some(current))
            if current.enabled
                && current.auto_track_models
                && provider_configuration_identity(&current)
                    == provider_configuration_identity(source) =>
        {
            current
        }
        _ => {
            outcome.entries.push(entry(
                supplier_id,
                &source.id,
                "unknown",
                "configuration",
                false,
                "source_changed".into(),
            ));
            return outcome;
        }
    };
    let search = match url_candidates::search(&source, auth_scheme).await {
        Ok(search) => search,
        Err(reason) => {
            let skipped = search_unavailable(&reason);
            outcome.entries.push(entry(
                supplier_id,
                &source.id,
                "unknown",
                "search",
                skipped,
                reason,
            ));
            return outcome;
        }
    };
    if search.candidates.is_empty() && search.failures.is_empty() {
        outcome.entries.push(entry(
            supplier_id,
            &source.id,
            "unknown",
            "search",
            true,
            "no_advertised_url_types".into(),
        ));
    }
    for (document_index, reason) in search.failures {
        outcome.entries.push(entry(
            supplier_id,
            &source.id,
            "unknown",
            "search",
            search_unavailable(&reason),
            format!("document_{document_index}:{reason}"),
        ));
    }
    for candidate in search.candidates {
        let provider_id = projection_id_for(&endpoint_id_for(supplier_id, candidate.protocol));
        if existing.contains(candidate.protocol) {
            outcome.entries.push(entry(
                supplier_id,
                &provider_id,
                candidate.protocol,
                "preflight",
                true,
                "url_type_already_configured".into(),
            ));
            continue;
        }
        let started = std::time::Instant::now();
        let (validated, models) = match url_candidates::validate(&source, &candidate).await {
            Ok(validated) => validated,
            Err(reason) => {
                outcome.entries.push(entry(
                    supplier_id,
                    &provider_id,
                    candidate.protocol,
                    "validation",
                    false,
                    reason,
                ));
                continue;
            }
        };
        // Recheck availability after I/O; stale or revoked source evidence cannot
        // authorize an append even if the URL answered earlier in this run.
        let Some(source_endpoint) = supplier.endpoints.iter().find(|endpoint| {
            endpoint.proxy_provider_id.as_deref() == Some(&source.id)
                || endpoint
                    .alternatives
                    .iter()
                    .any(|variant| variant.legacy_proxy_provider_id.as_deref() == Some(&source.id))
        }) else {
            break;
        };
        if let Some(excluded) =
            supplier_preflight::check(db, &supplier, source_endpoint, &source).await
        {
            outcome.entries.push(excluded);
            break;
        }
        let model_ids: Vec<String> = models
            .into_iter()
            .map(|model| model.id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let now = chrono::Utc::now().timestamp();
        let endpoint = SupplierEndpointPersistInput {
            protocol: candidate.protocol.into(),
            endpoint_url: if reqwest::Url::parse(candidate.url.trim_end_matches('/')).ok()
                == reqwest::Url::parse(supplier.base_url.trim_end_matches('/')).ok()
            {
                None
            } else {
                Some(candidate.url)
            },
            api_key_override: Some(validated.api_key),
            use_shared_api_key: false,
            enabled: Some(true),
            priority: Some(source_endpoint.priority),
            extra_headers: Some(validated.extra_headers),
            source: Some("url_discovery".into()),
            // This is route/catalog evidence, not a new source binding or receipt.
            provenance: Some(serde_json::json!({"sourceKind":"url_discovery","fetchedAt":now})),
            models: Some(
                model_ids
                    .iter()
                    .map(|model_id| SupplierModelPersistInput {
                        model_id: model_id.clone(),
                        display_name: None,
                        enabled: true,
                        supports_tools: None,
                        supports_images: None,
                        context_window: None,
                        max_output_tokens: None,
                        tier_override: None,
                        source: Some("provider_catalog".into()),
                        provenance: Some(
                            serde_json::json!({"sourceKind":"provider_catalog","fetchedAt":now}),
                        ),
                    })
                    .collect(),
            ),
        };
        let health = SupplierDiscoveryHealth {
            status: "catalog_available".into(),
            checked_at: now,
            last_success_at: Some(now),
            latency_ms: started.elapsed().as_millis() as u64,
            consecutive_failures: 0,
            error: None,
            listed_model_ids: model_ids.clone(),
            inference_status: "unverified".into(),
        };
        match db.append_discovered_supplier_endpoint(
            supplier_id,
            &source,
            &endpoint,
            health,
            generation,
        ) {
            Ok(Some(provider_id)) => {
                generation += 1;
                existing.insert(candidate.protocol.into());
                outcome.changed = true;
                supplier_preflight::record_outcome(
                    supplier_id,
                    &provider_id,
                    candidate.protocol,
                    "import",
                    "imported",
                    "validated_catalog",
                );
                outcome.entries.push(ProviderReport {
                    provider_id,
                    ok: true,
                    error: None,
                    plan: ProviderPlan {
                        added_models: model_ids,
                        ..Default::default()
                    },
                });
            }
            Ok(None) => outcome.entries.push(entry(
                supplier_id,
                &provider_id,
                candidate.protocol,
                "import",
                true,
                "url_already_configured".into(),
            )),
            Err(error) => outcome.entries.push(entry(
                supplier_id,
                &provider_id,
                candidate.protocol,
                "import",
                false,
                error.external_message(),
            )),
        }
    }
    outcome
}
