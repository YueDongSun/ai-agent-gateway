//! Read-only catalog discovery for supplier drafts, including saved secrets.

use super::{
    validate_headers, validate_id, validate_optional_url, validate_protocol, validate_secret,
    SupplierDiscoverModelsInput,
};
use crate::database::dao::proxy_providers::ProxyProvider;
use crate::database::Database;
use crate::error::AppError;
use crate::services::model_discovery::fetch::fetch_models;

const REVALIDATION_TIMEOUT_SECS: u64 = 20;

/// Configuration changes revalidate only their own enabled projections. The
/// existing discovery admission lock prevents overlap with the periodic worker.
/// Each projection has a fixed wall-clock bound; failed entries cannot consume
/// the remaining batch's deadline. Discovery uses read-only HTTP requests.
pub(crate) async fn revalidate_suppliers(db: &Database, ids: &[String]) -> Result<(), String> {
    let deadline = std::time::Duration::from_secs(REVALIDATION_TIMEOUT_SECS);
    let lock =
        crate::services::model_discovery::REFRESH_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = tokio::time::timeout(deadline, lock.lock())
        .await
        .map_err(|_| "supplier_discovery_timeout".to_string())?;
    let mut errors = std::collections::BTreeSet::new();
    let mut seen = std::collections::HashSet::new();
    for supplier_id in ids.iter().filter(|id| seen.insert(id.as_str())) {
        let supplier = match db.get_supplier_record_snapshot(supplier_id) {
            Ok((_, Some(supplier))) => supplier,
            Ok((_, None)) => continue,
            Err(error) => {
                crate::services::model_discovery::record_supplier_outcome(
                    supplier_id,
                    supplier_id,
                    "unknown",
                    "configuration",
                    "failed",
                    &error.external_message(),
                );
                errors.insert(error.external_message());
                continue;
            }
        };
        for endpoint in &supplier.endpoints {
            let Some(provider_id) = endpoint.proxy_provider_id.as_deref() else {
                continue;
            };
            let before_generation = db
                .supplier_config_generation()
                .map_err(|error| error.external_message());
            let before_generation = match before_generation {
                Ok(generation) => generation,
                Err(error) => {
                    errors.insert(error);
                    continue;
                }
            };
            let result = tokio::time::timeout(
                deadline,
                crate::services::model_discovery::refresh_providers(
                    db,
                    crate::services::model_discovery::Trigger::Manual,
                    Some(provider_id),
                ),
            )
            .await;
            // A timeout can follow a completed catalog/endpoint transaction.
            // Keep the existing pending-reload signal for those committed changes.
            match db.supplier_config_generation() {
                Ok(generation)
                    if generation != before_generation
                        || matches!(&result, Ok(Ok(report)) if report.has_changes) =>
                {
                    if let Err(error) = db.set_setting(
                        crate::database::dao::settings::KEY_MODEL_DISCOVERY_PENDING_RESTART,
                        "1",
                    ) {
                        errors.insert(error.external_message());
                    }
                }
                Err(error) => {
                    errors.insert(error.external_message());
                }
                _ => {}
            }
            match result {
                Ok(Ok(report)) => {
                    for provider in report.providers.iter().filter(|provider| !provider.ok) {
                        errors.insert(
                            provider
                                .error
                                .clone()
                                .unwrap_or_else(|| "supplier_discovery_failed".into()),
                        );
                    }
                }
                Ok(Err(error)) => {
                    crate::services::model_discovery::record_supplier_outcome(
                        supplier_id,
                        provider_id,
                        &endpoint.protocol,
                        "revalidation",
                        "failed",
                        &error.external_message(),
                    );
                    errors.insert(error.external_message());
                }
                Err(_) => {
                    crate::services::model_discovery::record_supplier_outcome(
                        supplier_id,
                        provider_id,
                        &endpoint.protocol,
                        "revalidation",
                        "failed",
                        "supplier_discovery_timeout",
                    );
                    errors.insert("supplier_discovery_timeout".into());
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else if errors.len() == 1 && errors.contains("supplier_discovery_timeout") {
        Err("supplier_discovery_timeout".into())
    } else {
        Err(format!(
            "supplier_discovery_partial:{}",
            errors.into_iter().collect::<Vec<_>>().join(",")
        ))
    }
}

/// Resolve a draft before issuing I/O. This neither saves the draft nor changes
/// the supplier generation, projections, model inventory, or routing state.
pub async fn discover_supplier_models(
    db: &Database,
    input: SupplierDiscoverModelsInput,
) -> Result<Vec<String>, AppError> {
    let (provider, saved_projection) = resolve_discovery_provider(db, input)?;
    if crate::services::provider_admission::enforcement_enabled(db) {
        // Drafts may only discover through their unchanged screened connection.
        // A changed/new connection first needs CC Switch import screening.
        if !saved_projection {
            return Err(AppError::InvalidInput(
                "supplier_subscription_screening_required".into(),
            ));
        }
        crate::services::provider_admission::ensure_catalog_admitted(db, &provider)
            .await
            .map_err(|_| {
                AppError::InvalidInput("supplier_subscription_screening_required".into())
            })?;
    }
    let auth_scheme = if saved_projection {
        crate::services::provider_admission::auth_scheme(db, &provider).map_err(|_| {
            AppError::InvalidInput("supplier_subscription_screening_required".into())
        })?
    } else {
        // New drafts have no saved source identity to borrow, even if their
        // coordinates happen to match an existing provider.
        crate::proxy::providers::AnthropicAuthScheme::ApiKey
    };
    let fetched = fetch_models(&provider, auth_scheme)
        .await
        .map_err(|error| {
            // FetchError displays only stable codes/status numbers, never a URL,
            // credential, response body, or transport-library diagnostic.
            AppError::Message(error.to_string().replacen("proxy_", "supplier_", 1))
        })?;
    let mut models: Vec<String> = fetched.models.into_iter().map(|model| model.id).collect();
    models.sort();
    models.dedup();
    Ok(models)
}

fn resolve_discovery_provider(
    db: &Database,
    input: SupplierDiscoverModelsInput,
) -> Result<(ProxyProvider, bool), AppError> {
    validate_protocol(&input.protocol)?;
    let existing = if let Some(id) = input.supplier_id.as_deref() {
        validate_id(id)?;
        Some(
            db.get_supplier_record_snapshot(id)?
                .1
                .ok_or_else(|| AppError::InvalidInput("supplier_not_found".into()))?,
        )
    } else {
        None
    };
    let endpoint = existing.as_ref().and_then(|supplier| {
        supplier
            .endpoints
            .iter()
            .find(|endpoint| endpoint.protocol == input.protocol)
    });
    let base_url = input
        .base_url
        .as_deref()
        .or_else(|| existing.as_ref().map(|supplier| supplier.base_url.as_str()))
        .unwrap_or_default();
    validate_optional_url(base_url)?;
    let endpoint_url = input
        .endpoint_url
        .as_deref()
        .or_else(|| endpoint.and_then(|endpoint| endpoint.endpoint_url.as_deref()));
    if let Some(url) = endpoint_url {
        validate_optional_url(url)?;
    }
    let effective_url = endpoint_url
        .filter(|url| !url.is_empty())
        .unwrap_or(base_url);
    if effective_url.is_empty() {
        return Err(AppError::InvalidInput(
            "supplier_endpoint_url_required".into(),
        ));
    }
    let proxy_config = db.get_proxy_config()?;
    if crate::proxy::self_loop::base_url_targets_self(
        effective_url,
        &proxy_config.host,
        proxy_config.port,
    ) {
        return Err(AppError::InvalidInput("supplier_endpoint_self_loop".into()));
    }
    let shared_key = input
        .api_key
        .as_deref()
        .or_else(|| existing.as_ref().map(|supplier| supplier.api_key.as_str()))
        .unwrap_or_default();
    validate_secret(shared_key)?;
    // Same precedence as supplier_save: shared-key selection deliberately
    // clears an override; an explicit empty override means anonymous auth.
    let override_key = if input.use_shared_api_key {
        None
    } else {
        input
            .api_key_override
            .as_deref()
            .or_else(|| endpoint.and_then(|endpoint| endpoint.api_key_override.as_deref()))
    };
    let api_key = override_key.unwrap_or(shared_key);
    validate_secret(api_key)?;
    let extra_headers = input.extra_headers.unwrap_or_else(|| {
        endpoint
            .map(|endpoint| endpoint.extra_headers.clone())
            .unwrap_or_default()
    });
    validate_headers(&extra_headers)?;
    let provider_type = match input.protocol.as_str() {
        "anthropic" => "anthropic",
        "gemini" => "gemini",
        "openai-responses" => "openai-responses",
        _ => "openai",
    };
    let mut provider = ProxyProvider {
        id: "supplier_discovery_draft".into(),
        name: "Supplier model discovery".into(),
        provider_type: provider_type.into(),
        base_url: effective_url.into(),
        api_key: api_key.into(),
        models: Vec::new(),
        enabled: true,
        timeout_ms: 15_000,
        weight: 1,
        circuit_breaker: Default::default(),
        model_mapping: Default::default(),
        extra_headers,
        auto_track_models: false,
        icon: None,
        icon_color: None,
    };
    if let Some(provider_id) = endpoint.and_then(|endpoint| endpoint.proxy_provider_id.as_deref()) {
        let mut saved = db.get_proxy_provider(provider_id)?.ok_or_else(|| {
            AppError::InvalidInput("supplier_subscription_screening_required".into())
        })?;
        if crate::services::provider_admission::enforcement_enabled(db)
            && (saved.provider_type != provider.provider_type
                || saved.base_url != provider.base_url
                || saved.api_key != provider.api_key
                || saved.extra_headers != provider.extra_headers)
        {
            return Err(AppError::InvalidInput(
                "supplier_subscription_screening_required".into(),
            ));
        }
        // Keep the selected projection's ID, enabled state, models and admission
        // context. Draft connection edits remain explicit and must validate
        // against that same source rather than finding a coordinate twin.
        saved.provider_type = provider.provider_type;
        saved.base_url = provider.base_url;
        saved.api_key = provider.api_key;
        saved.extra_headers = provider.extra_headers;
        provider = saved;
        return Ok((provider, true));
    }
    Ok((provider, false))
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
