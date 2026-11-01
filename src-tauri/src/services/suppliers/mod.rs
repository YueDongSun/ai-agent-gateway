//! Supplier configuration service.
//!
//! Boundary validation and secret-safe UI views live here. SQL and projection
//! transactions live in `database::dao::suppliers`.

mod discovery;
mod types;

pub use discovery::discover_supplier_models;
pub(crate) use discovery::revalidate_suppliers;

pub use types::{
    PublicCatalogCache, PublicCatalogCacheInput, SupplierDiscoverModelsInput,
    SupplierEndpointAlternativeView, SupplierEndpointInput, SupplierEndpointView,
    SupplierGetResult, SupplierListView, SupplierModelInput, SupplierModelView,
    SupplierModelsUpdateInput, SupplierMutationResult, SupplierProvenance, SupplierSaveInput,
    SupplierStatus, SupplierView,
};

use crate::database::dao::suppliers::{
    self as supplier_dao, SupplierEndpointPersistInput, SupplierModelPersistInput,
    SupplierPersistInput, SupplierRecord,
};
use crate::database::Database;
use crate::error::AppError;
use reqwest::Url;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const MAX_ID_CHARS: usize = 80;
const MAX_NAME_CHARS: usize = 128;
const MAX_URL_BYTES: usize = 2048;
const MAX_API_KEY_BYTES: usize = 16 * 1024;
const MAX_ENDPOINTS: usize = 4;
const MAX_MODELS_PER_ENDPOINT: usize = 1000;
const MAX_HEADER_COUNT: usize = 32;
const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;
const MAX_HEADERS_TOTAL_BYTES: usize = 32 * 1024;
const MAX_METADATA_CHARS: usize = 128;
const MAX_CAPABILITY_LIMIT: u64 = 100_000_000;
const MAX_ICON_CHARS: usize = 64;
const MAX_ICON_COLOR_CHARS: usize = 32;
const MALFORMED_VENDOR_DETAIL: &str = "malformed vendor record";

/// Runtime endpoint record for the Rust proxy snapshot. It contains resolved
/// credentials and header values and intentionally has no serde implementation.
pub struct RuntimeSupplierEndpoint {
    pub endpoint_id: String,
    pub supplier_id: String,
    pub supplier_name: String,
    pub protocol: String,
    pub base_url: String,
    pub api_key: String,
    pub extra_headers: HashMap<String, String>,
    pub priority: i64,
    pub legacy_proxy_provider_id: Option<String>,
    pub models: Vec<RuntimeSupplierModel>,
}

pub struct RuntimeSupplierModel {
    pub model_id: String,
    pub enabled: bool,
    pub supports_tools: Option<bool>,
    pub supports_images: Option<bool>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub tier_override: Option<u8>,
}

pub struct RuntimeSupplierSnapshot {
    pub generation: u64,
    pub endpoints: Vec<RuntimeSupplierEndpoint>,
}

/// Return all secret-free supplier views and the generation read from the same
/// SQLite snapshot. A malformed legacy vendor row fails the read loudly: the
/// list must never render a world where the bad record silently vanished.
pub fn list_suppliers(db: &Database) -> Result<SupplierListView, AppError> {
    reject_malformed_legacy_vendor_rows(db)?;
    let (generation, records) = db.get_supplier_snapshot()?;
    Ok(SupplierListView {
        generation,
        suppliers: records.into_iter().map(|record| to_view(db, record)).collect::<Result<_, _>>()?,
    })
}

/// Return one secret-free supplier view plus the matching generation.
pub fn get_supplier(db: &Database, id: &str) -> Result<SupplierGetResult, AppError> {
    validate_id(id)?;
    let (generation, record) = db.get_supplier_record_snapshot(id)?;
    Ok(SupplierGetResult {
        generation,
        supplier: record.map(|record| to_view(db, record)).transpose()?,
    })
}

/// Read the persistent supplier configuration generation without loading
/// secrets or model data.
pub fn supplier_config_generation(db: &Database) -> Result<u64, AppError> {
    db.supplier_config_generation()
}

/// Persist a supplier and its compatible legacy proxy projection. `apiKey` is
/// write-only: omission keeps the current value and an explicit empty string
/// removes it.
pub fn save_supplier(
    db: &Database,
    input: SupplierSaveInput,
) -> Result<SupplierMutationResult, AppError> {
    validate_name(&input.name)?;
    validate_priority(input.priority)?;
    if let Some(base_url) = input.base_url.as_deref() {
        validate_optional_url(base_url)?;
    }
    if let Some(api_key) = input.api_key.as_deref() {
        validate_secret(api_key)?;
    }
    if let Some(source) = input.source.as_deref() {
        validate_source(source)?;
    }
    if let Some(provenance) = input.provenance.as_ref() {
        validate_provenance(provenance)?;
    }
    // Display fields: trim at the boundary so a whitespace-only icon is the
    // same explicit clear as `""`.
    let icon = input.icon.map(|value| value.trim().to_string());
    let icon_color = input.icon_color.map(|value| value.trim().to_string());
    if let Some(icon) = icon.as_deref() {
        validate_display_icon(icon)?;
    }
    if let Some(icon_color) = icon_color.as_deref() {
        validate_display_color(icon_color)?;
    }

    let (id, must_be_new) = match input.id.as_deref() {
        Some(id) => {
            validate_id(id)?;
            (id.to_string(), false)
        }
        None => (new_supplier_id()?, true),
    };
    let endpoints = input
        .endpoints
        .map(|endpoints| validate_endpoints(endpoints))
        .transpose()?;
    reject_malformed_legacy_vendor_rows(db)?;
    let provenance = input.provenance.map(serde_json::to_value).transpose()?;
    let persist = SupplierPersistInput {
        id: id.clone(),
        must_be_new,
        name: input.name,
        base_url: input.base_url,
        api_key: input.api_key,
        enabled: input.enabled,
        priority: input.priority,
        source: input.source,
        provenance,
        icon,
        icon_color,
        endpoints,
    };
    let generation = db.save_supplier_record(&persist, input.expected_generation)?;
    Ok(SupplierMutationResult {
        supplier_id: id,
        generation,
    })
}

fn reject_malformed_legacy_vendor_rows(db: &Database) -> Result<(), AppError> {
    let vendors = db.get_all_vendors().map_err(|error| match error {
        AppError::Json(_) => AppError::Message(MALFORMED_VENDOR_DETAIL.into()),
        other => other,
    })?;
    if vendors.iter().any(|vendor| {
        vendor
            .protocols
            .iter()
            .any(|protocol| !supplier_dao::is_supported_protocol(protocol))
    }) {
        return Err(AppError::Message(MALFORMED_VENDOR_DETAIL.into()));
    }
    Ok(())
}

/// Delete exactly one supplier with a generation compare-and-swap. Legacy
/// provider rows referenced by existing routes are retained; only projections
/// owned by the supplier service are removed.
pub fn delete_supplier(db: &Database, id: &str, expected_generation: u64) -> Result<u64, AppError> {
    validate_id(id)?;
    db.delete_supplier_record(id, expected_generation)
}

/// Replace the model inventory and manual capability overrides for one
/// endpoint. A successful write advances the same configuration generation as
/// supplier edits.
pub fn update_supplier_models(
    db: &Database,
    input: SupplierModelsUpdateInput,
) -> Result<SupplierMutationResult, AppError> {
    validate_id(&input.supplier_id)?;
    validate_protocol(&input.protocol)?;
    validate_models(&input.models)?;
    let models = input
        .models
        .into_iter()
        .map(model_to_persist)
        .collect::<Result<Vec<_>, _>>()?;
    let generation = db.replace_supplier_models(
        &input.supplier_id,
        &input.protocol,
        &models,
        input.expected_generation,
    )?;
    Ok(SupplierMutationResult {
        supplier_id: input.supplier_id,
        generation,
    })
}

/// Read the current resolved endpoint snapshot for proxy construction. This is
/// backend-only: returned records include API keys and secret header values.
pub fn resolved_supplier_snapshot(db: &Database) -> Result<RuntimeSupplierSnapshot, AppError> {
    let (generation, records) = db.get_supplier_snapshot()?;
    let mut endpoints = Vec::new();
    for supplier in records {
        if !supplier.enabled {
            continue;
        }
        for endpoint in supplier.endpoints {
            for alternative in &endpoint.alternatives {
                if !alternative.enabled || !is_runtime_url(&alternative.base_url) {
                    continue;
                }
                endpoints.push(RuntimeSupplierEndpoint {
                    endpoint_id: alternative.id.clone(),
                    supplier_id: supplier.id.clone(),
                    supplier_name: supplier.name.clone(),
                    protocol: endpoint.protocol.clone(),
                    base_url: alternative.base_url.clone(),
                    api_key: alternative.api_key.clone(),
                    extra_headers: alternative.extra_headers.clone(),
                    priority: supplier.priority.saturating_add(alternative.priority),
                    legacy_proxy_provider_id: alternative.legacy_proxy_provider_id.clone(),
                    models: alternative
                        .models
                        .iter()
                        .map(|model| RuntimeSupplierModel {
                            model_id: model.model_id.clone(),
                            enabled: model.enabled,
                            supports_tools: model.supports_tools,
                            supports_images: model.supports_images,
                            context_window: model.context_window,
                            max_output_tokens: model.max_output_tokens,
                            tier_override: model.tier_override,
                        })
                        .collect(),
                });
            }
            if !endpoint.enabled {
                continue;
            }
            let base_url = endpoint
                .endpoint_url
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or(&supplier.base_url);
            if !is_runtime_url(base_url) {
                continue;
            }
            let api_key = endpoint
                .api_key_override
                .as_deref()
                .unwrap_or(&supplier.api_key)
                .to_string();
            endpoints.push(RuntimeSupplierEndpoint {
                endpoint_id: endpoint.id,
                supplier_id: supplier.id.clone(),
                supplier_name: supplier.name.clone(),
                protocol: endpoint.protocol,
                base_url: base_url.to_string(),
                api_key,
                extra_headers: endpoint.extra_headers,
                priority: supplier.priority.saturating_add(endpoint.priority),
                legacy_proxy_provider_id: endpoint.proxy_provider_id,
                models: endpoint
                    .models
                    .into_iter()
                    .map(|model| RuntimeSupplierModel {
                        model_id: model.model_id,
                        enabled: model.enabled,
                        supports_tools: model.supports_tools,
                        supports_images: model.supports_images,
                        context_window: model.context_window,
                        max_output_tokens: model.max_output_tokens,
                        tier_override: model.tier_override,
                    })
                    .collect(),
            });
        }
    }
    endpoints.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.endpoint_id.cmp(&right.endpoint_id))
    });
    Ok(RuntimeSupplierSnapshot {
        generation,
        endpoints,
    })
}

/// Safe aggregate status only. `configured` means a URL and enabled flags are
/// present; it does not establish provider reachability or account validity.
pub fn supplier_status(db: &Database) -> Result<SupplierStatus, AppError> {
    let list = list_suppliers(db)?;
    let supplier_count = list.suppliers.len();
    let enabled_supplier_count = list.suppliers.iter().filter(|item| item.enabled).count();
    let endpoint_count = list.suppliers.iter().map(|item| item.endpoints.len()).sum();
    let enabled_endpoint_count = list
        .suppliers
        .iter()
        .flat_map(|item| item.endpoints.iter().map(move |endpoint| (item, endpoint)))
        .filter(|(item, endpoint)| item.enabled && endpoint.enabled)
        .count();
    let configured_endpoint_count = list
        .suppliers
        .iter()
        .flat_map(|item| item.endpoints.iter().map(move |endpoint| (item, endpoint)))
        .filter(|(item, endpoint)| {
            item.enabled
                && endpoint.enabled
                && endpoint.effective_base_url.is_some()
                && !endpoint.url_requires_review
        })
        .count();
    let legacy_projected_endpoint_count = list
        .suppliers
        .iter()
        .flat_map(|item| item.endpoints.iter().map(move |endpoint| (item, endpoint)))
        .filter(|(item, endpoint)| {
            item.enabled
                && endpoint.enabled
                && endpoint.effective_base_url.is_some()
                && !endpoint.url_requires_review
                && endpoint.legacy_projection_managed
        })
        .count();
    let pending_unsupported_count = list
        .suppliers
        .iter()
        .filter(|item| item.availability == "pending_unsupported")
        .count();
    Ok(SupplierStatus {
        generation: list.generation,
        supplier_count,
        enabled_supplier_count,
        endpoint_count,
        enabled_endpoint_count,
        configured_endpoint_count,
        legacy_projected_endpoint_count,
        pending_unsupported_count,
        official_auth_forwarding_supported: false,
    })
}

/// Return a public model-directory cache after revalidating its persisted JSON
/// and safe source/error labels.
pub fn get_public_catalog_cache(
    db: &Database,
    source: &str,
) -> Result<Option<PublicCatalogCache>, AppError> {
    validate_source(source)?;
    db.get_supplier_catalog_cache(source)?
        .map(|record| {
            validate_public_catalog_json(&record.raw_json, true)?;
            if let Some(code) = record.last_error.as_deref() {
                validate_error_code(code)?;
            }
            Ok(PublicCatalogCache {
                source: record.source,
                raw_json: record.raw_json,
                fetched_at: record.fetched_at,
                last_error: record.last_error,
            })
        })
        .transpose()
}

/// Save a validated public model catalog. Cache updates do not advance the
/// routing configuration generation.
pub fn save_public_catalog_cache(
    db: &Database,
    input: PublicCatalogCacheInput,
) -> Result<(), AppError> {
    validate_source(&input.source)?;
    validate_cache_time(input.fetched_at)?;
    validate_public_catalog_json(&input.raw_json, false)?;
    db.save_supplier_catalog_cache(&input.source, &input.raw_json, input.fetched_at, None)
}

/// Record a bounded safe error code while preserving the last known-good
/// document and fetched timestamp.
pub fn record_public_catalog_error(
    db: &Database,
    source: &str,
    error_code: &str,
) -> Result<(), AppError> {
    validate_source(source)?;
    validate_error_code(error_code)?;
    db.record_supplier_catalog_error(source, error_code)
}

/// Stable IPC-safe error mapping for the supplier commands.
pub(crate) fn ipc_error(error: AppError) -> String {
    let code = match &error {
        AppError::Message(detail) | AppError::InvalidInput(detail)
            if is_supplier_error_code(detail) =>
        {
            Some(detail.as_str())
        }
        _ => None,
    };
    code.map(str::to_string)
        .unwrap_or_else(|| String::from(error))
}

fn is_supplier_error_code(value: &str) -> bool {
    value.starts_with("supplier_")
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn to_view(db: &Database, record: SupplierRecord) -> Result<SupplierView, AppError> {
    let provenance = provenance_from_value(record.provenance)?;
    let (base_url, mut url_requires_review) = display_url(&record.base_url);
    let mut endpoints = Vec::with_capacity(record.endpoints.len());
    for endpoint in record.endpoints {
        let endpoint_provenance = provenance_from_value(endpoint.provenance)?;
        let (endpoint_url, endpoint_url_redacted) =
            display_optional_url(endpoint.endpoint_url.as_deref());
        let effective_raw = endpoint
            .endpoint_url
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(&record.base_url);
        let (effective_base_url, effective_redacted) = display_url(effective_raw);
        let auth_mode = match endpoint.api_key_override.as_deref() {
            Some(value) if !value.is_empty() => "override",
            Some(_) => "none",
            None if !record.api_key.is_empty() => "shared",
            None => "none",
        };
        let mut header_names: Vec<String> = endpoint.extra_headers.keys().cloned().collect();
        header_names.sort_by_key(|name| name.to_ascii_lowercase());
        let alternatives = endpoint
            .alternatives
            .iter()
            .map(|alternative| {
                let (endpoint_url, url_requires_review) = display_url(&alternative.base_url);
                let mut header_names: Vec<_> = alternative.extra_headers.keys().cloned().collect();
                header_names.sort();
                SupplierEndpointAlternativeView {
                    id: alternative.id.clone(),
                    endpoint_url,
                    enabled: alternative.enabled,
                    priority: alternative.priority,
                    api_key_configured: !alternative.api_key.is_empty(),
                    header_names,
                    model_ids: alternative
                        .models
                        .iter()
                        .map(|model| model.model_id.clone())
                        .collect(),
                    source: alternative.source.clone(),
                    url_requires_review,
                }
            })
            .collect();
        let models = endpoint
            .models
            .into_iter()
            .map(|model| {
                Ok(SupplierModelView {
                    id: model.id,
                    model_id: model.model_id,
                    display_name: model.display_name,
                    enabled: model.enabled,
                    supports_tools: model.supports_tools,
                    supports_images: model.supports_images,
                    context_window: model.context_window,
                    max_output_tokens: model.max_output_tokens,
                    tier_override: model.tier_override,
                    sort_index: model.sort_index,
                    source: model.source,
                    provenance: provenance_from_value(model.provenance)?,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        url_requires_review |= endpoint_url_redacted || effective_redacted;
        let discovery_health = endpoint.proxy_provider_id.as_deref()
            .map(|id| db.supplier_discovery_health(id)).transpose()?.flatten();
        endpoints.push(SupplierEndpointView {
            id: endpoint.id,
            protocol: endpoint.protocol,
            endpoint_url,
            effective_base_url,
            enabled: endpoint.enabled,
            priority: endpoint.priority,
            auth_mode: auth_mode.to_string(),
            header_names,
            source: endpoint.source,
            provenance: endpoint_provenance,
            url_requires_review: endpoint_url_redacted || effective_redacted,
            legacy_projection_available: endpoint.proxy_provider_id.is_some(),
            legacy_projection_managed: endpoint.projection_managed,
            models,
            alternatives,
            discovery_health,
        });
    }
    let availability = if record.source == "official_auth_pending"
        || record.source == "legacy_passthrough_pending"
    {
        "pending_unsupported"
    } else if endpoints.is_empty() {
        "not_configured"
    } else if endpoints
        .iter()
        .any(|endpoint| endpoint.effective_base_url.is_some())
    {
        "configured"
    } else {
        "needs_configuration"
    }
    .to_string();
    Ok(SupplierView {
        id: record.id,
        name: record.name,
        base_url,
        enabled: record.enabled,
        priority: record.priority,
        source: record.source,
        provenance,
        api_key_configured: !record.api_key.is_empty(),
        icon: record.icon.filter(|value| !value.is_empty()),
        icon_color: record.icon_color.filter(|value| !value.is_empty()),
        availability,
        url_requires_review,
        created_at: record.created_at,
        updated_at: record.updated_at,
        endpoints,
    })
}

fn provenance_from_value(value: Value) -> Result<SupplierProvenance, AppError> {
    let provenance: SupplierProvenance = serde_json::from_value(value).map_err(AppError::Json)?;
    validate_provenance(&provenance)?;
    Ok(provenance)
}

fn display_optional_url(value: Option<&str>) -> (Option<String>, bool) {
    match value {
        Some(value) if !value.is_empty() => display_url(value),
        _ => (None, false),
    }
}

fn display_url(value: &str) -> (Option<String>, bool) {
    if value.is_empty() {
        return (None, false);
    }
    // Mirror runtime admission (`is_runtime_url`): a URL the runtime would
    // refuse is never presented as usable. The review flag still surfaces the
    // record for repair instead of silently dropping it.
    if !is_runtime_url(value) {
        return (None, true);
    }
    (Some(value.to_string()), false)
}

fn is_runtime_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn new_supplier_id() -> Result<String, AppError> {
    let mut random = [0_u8; 16];
    getrandom::getrandom(&mut random)
        .map_err(|_| AppError::Message("supplier_id_generation_failed".into()))?;
    Ok(format!("supplier_{}", hex::encode(random)))
}

fn validate_id(value: &str) -> Result<(), AppError> {
    if value.is_empty()
        || value.chars().count() > MAX_ID_CHARS
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(AppError::InvalidInput("supplier_id_invalid".into()));
    }
    Ok(())
}

fn validate_name(value: &str) -> Result<(), AppError> {
    if value.trim().is_empty()
        || value.chars().count() > MAX_NAME_CHARS
        || value.chars().any(char::is_control)
    {
        return Err(AppError::InvalidInput("supplier_name_invalid".into()));
    }
    Ok(())
}

fn validate_priority(value: i64) -> Result<(), AppError> {
    if !(-1_000_000..=1_000_000).contains(&value) {
        return Err(AppError::InvalidInput(
            "supplier_priority_out_of_range".into(),
        ));
    }
    Ok(())
}

/// Display icons are presentation-only (glyph id or a literal
/// letter/emoji); the only boundary rules are a size bound and no
/// control characters, so a stored record always renders in one tile.
fn validate_display_icon(value: &str) -> Result<(), AppError> {
    if value.chars().count() > MAX_ICON_CHARS || value.chars().any(char::is_control) {
        return Err(AppError::InvalidInput("supplier_icon_invalid".into()));
    }
    Ok(())
}

fn validate_display_color(value: &str) -> Result<(), AppError> {
    if value.chars().count() > MAX_ICON_COLOR_CHARS || value.chars().any(char::is_control) {
        return Err(AppError::InvalidInput("supplier_icon_color_invalid".into()));
    }
    Ok(())
}

fn validate_optional_url(value: &str) -> Result<(), AppError> {
    if value.is_empty() {
        return Ok(());
    }
    if value.len() > MAX_URL_BYTES || value.chars().any(char::is_control) {
        return Err(AppError::InvalidInput(
            "supplier_endpoint_url_invalid".into(),
        ));
    }
    let url = Url::parse(value)
        .map_err(|_| AppError::InvalidInput("supplier_endpoint_url_invalid".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::InvalidInput(
            "supplier_endpoint_url_invalid".into(),
        ));
    }
    Ok(())
}

fn validate_secret(value: &str) -> Result<(), AppError> {
    if value.len() > MAX_API_KEY_BYTES || value.chars().any(char::is_control) {
        return Err(AppError::InvalidInput("supplier_api_key_invalid".into()));
    }
    Ok(())
}

fn validate_source(value: &str) -> Result<(), AppError> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
    {
        return Err(AppError::InvalidInput("supplier_source_invalid".into()));
    }
    Ok(())
}

fn validate_protocol(value: &str) -> Result<(), AppError> {
    if !supplier_dao::is_supported_protocol(value) {
        return Err(AppError::InvalidInput(
            "supplier_protocol_unsupported".into(),
        ));
    }
    Ok(())
}

fn validate_provenance(value: &SupplierProvenance) -> Result<(), AppError> {
    for item in [
        value.source_kind.as_deref(),
        value.source_record_ref.as_deref(),
        value.source_app_type.as_deref(),
        value.source_endpoint_ref.as_deref(),
        value.import_batch_ref.as_deref(),
        value.content_version.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if item.is_empty()
            || item.chars().count() > MAX_METADATA_CHARS
            || !item.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'+')
            })
        {
            return Err(AppError::InvalidInput("supplier_provenance_invalid".into()));
        }
    }
    if value
        .confidence
        .is_some_and(|confidence| !confidence.is_finite() || !(0.0..=1.0).contains(&confidence))
    {
        return Err(AppError::InvalidInput("supplier_provenance_invalid".into()));
    }
    Ok(())
}

fn validate_headers(headers: &HashMap<String, String>) -> Result<(), AppError> {
    if headers.len() > MAX_HEADER_COUNT {
        return Err(AppError::InvalidInput("supplier_headers_too_many".into()));
    }
    let reserved = [
        "authorization",
        "x-api-key",
        "api-key",
        "host",
        "content-type",
        "content-length",
        "anthropic-version",
    ];
    let mut total = 0_usize;
    for (name, value) in headers {
        if name.is_empty()
            || name.len() > 128
            || !name.bytes().all(is_header_token_byte)
            || reserved.iter().any(|item| name.eq_ignore_ascii_case(item))
            || value.len() > MAX_HEADER_VALUE_BYTES
            || value
                .chars()
                .any(|character| character.is_control() && character != '\t')
        {
            return Err(AppError::InvalidInput("supplier_header_invalid".into()));
        }
        total = total.saturating_add(name.len()).saturating_add(value.len());
    }
    if total > MAX_HEADERS_TOTAL_BYTES {
        return Err(AppError::InvalidInput("supplier_headers_too_large".into()));
    }
    Ok(())
}

fn is_header_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn validate_models(models: &[SupplierModelInput]) -> Result<(), AppError> {
    if models.len() > MAX_MODELS_PER_ENDPOINT {
        return Err(AppError::InvalidInput("supplier_models_too_many".into()));
    }
    let mut ids = HashSet::with_capacity(models.len());
    for model in models {
        if model.model_id.trim().is_empty()
            || model.model_id.len() > 256
            || model.model_id.chars().any(char::is_control)
            || !ids.insert(model.model_id.as_str())
        {
            return Err(AppError::InvalidInput("supplier_model_id_invalid".into()));
        }
        if model.display_name.as_deref().is_some_and(|value| {
            value.chars().count() > MAX_NAME_CHARS || value.chars().any(char::is_control)
        }) {
            return Err(AppError::InvalidInput("supplier_model_name_invalid".into()));
        }
        if model
            .context_window
            .is_some_and(|value| value == 0 || value > MAX_CAPABILITY_LIMIT)
            || model
                .max_output_tokens
                .is_some_and(|value| value == 0 || value > MAX_CAPABILITY_LIMIT)
            || model.tier_override.is_some_and(|tier| tier > 3)
        {
            return Err(AppError::InvalidInput(
                "supplier_model_capability_invalid".into(),
            ));
        }
        if let Some(source) = model.source.as_deref() {
            validate_source(source)?;
        }
        if let Some(provenance) = model.provenance.as_ref() {
            validate_provenance(provenance)?;
        }
    }
    Ok(())
}

fn validate_endpoints(
    endpoints: Vec<SupplierEndpointInput>,
) -> Result<Vec<SupplierEndpointPersistInput>, AppError> {
    if endpoints.len() > MAX_ENDPOINTS {
        return Err(AppError::InvalidInput("supplier_endpoints_too_many".into()));
    }
    let mut protocols = HashSet::with_capacity(endpoints.len());
    let mut validated = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        validate_protocol(&endpoint.protocol)?;
        if !protocols.insert(endpoint.protocol.clone()) {
            return Err(AppError::InvalidInput("supplier_endpoint_duplicate".into()));
        }
        if let Some(url) = endpoint.endpoint_url.as_deref() {
            validate_optional_url(url)?;
        }
        if endpoint.use_shared_api_key && endpoint.api_key_override.is_some() {
            return Err(AppError::InvalidInput(
                "supplier_endpoint_auth_ambiguous".into(),
            ));
        }
        if let Some(key) = endpoint.api_key_override.as_deref() {
            validate_secret(key)?;
        }
        if let Some(priority) = endpoint.priority {
            validate_priority(priority)?;
        }
        if let Some(headers) = endpoint.extra_headers.as_ref() {
            validate_headers(headers)?;
        }
        if let Some(source) = endpoint.source.as_deref() {
            validate_source(source)?;
        }
        if let Some(provenance) = endpoint.provenance.as_ref() {
            validate_provenance(provenance)?;
        }
        if let Some(models) = endpoint.models.as_ref() {
            validate_models(models)?;
        }
        let models = endpoint
            .models
            .map(|models| models.into_iter().map(model_to_persist).collect())
            .transpose()?;
        validated.push(SupplierEndpointPersistInput {
            protocol: endpoint.protocol,
            endpoint_url: endpoint.endpoint_url,
            api_key_override: endpoint.api_key_override,
            use_shared_api_key: endpoint.use_shared_api_key,
            enabled: endpoint.enabled,
            priority: endpoint.priority,
            extra_headers: endpoint.extra_headers,
            source: endpoint.source,
            provenance: endpoint.provenance.map(serde_json::to_value).transpose()?,
            models,
        });
    }
    Ok(validated)
}

fn model_to_persist(model: SupplierModelInput) -> Result<SupplierModelPersistInput, AppError> {
    if let Some(source) = model.source.as_deref() {
        validate_source(source)?;
    }
    if let Some(provenance) = model.provenance.as_ref() {
        validate_provenance(provenance)?;
    }
    Ok(SupplierModelPersistInput {
        model_id: model.model_id,
        display_name: model.display_name,
        enabled: model.enabled,
        supports_tools: model.supports_tools,
        supports_images: model.supports_images,
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        tier_override: model.tier_override,
        source: model.source,
        provenance: model.provenance.map(serde_json::to_value).transpose()?,
    })
}

fn validate_cache_time(value: i64) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    if value <= 0 || value > now.saturating_add(86_400) {
        return Err(AppError::InvalidInput(
            "supplier_catalog_timestamp_invalid".into(),
        ));
    }
    Ok(())
}

fn validate_error_code(value: &str) -> Result<(), AppError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(AppError::InvalidInput(
            "supplier_catalog_error_invalid".into(),
        ));
    }
    Ok(())
}

fn validate_public_catalog_json(raw_json: &str, allow_empty_marker: bool) -> Result<(), AppError> {
    if raw_json.len() > supplier_dao::MAX_CATALOG_JSON_BYTES {
        return Err(AppError::InvalidInput("supplier_catalog_too_large".into()));
    }
    let value: Value = serde_json::from_str(raw_json)?;
    if allow_empty_marker && value.is_null() {
        return Ok(());
    }
    if !value.is_object() && !value.is_array() {
        return Err(AppError::InvalidInput(
            "supplier_catalog_shape_invalid".into(),
        ));
    }
    if contains_sensitive_catalog_field(&value) {
        return Err(AppError::InvalidInput(
            "supplier_catalog_sensitive_field".into(),
        ));
    }
    Ok(())
}

fn contains_sensitive_catalog_field(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized = key
                .bytes()
                .filter(|byte| byte.is_ascii_alphanumeric())
                .map(|byte| byte.to_ascii_lowercase() as char)
                .collect::<String>();
            matches!(
                normalized.as_str(),
                "apikey"
                    | "accesstoken"
                    | "refreshtoken"
                    | "authtoken"
                    | "authorization"
                    | "clientsecret"
                    | "secret"
                    | "password"
                    | "credentials"
            ) || contains_sensitive_catalog_field(value)
        }),
        Value::Array(values) => values.iter().any(contains_sensitive_catalog_field),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn db() -> Database {
        Database::memory().expect("supplier test database")
    }

    fn save_input(id: Option<&str>, generation: u64, api_key: Option<&str>) -> SupplierSaveInput {
        SupplierSaveInput {
            id: id.map(str::to_string),
            expected_generation: generation,
            name: "Acme Relay".into(),
            base_url: Some("https://api.example.invalid/v1".into()),
            api_key: api_key.map(str::to_string),
            enabled: true,
            priority: 100,
            source: None,
            provenance: None,
            icon: None,
            icon_color: None,
            endpoints: Some(vec![endpoint_input()]),
        }
    }

    fn endpoint_input() -> SupplierEndpointInput {
        SupplierEndpointInput {
            protocol: "openai-chat".into(),
            endpoint_url: None,
            api_key_override: None,
            use_shared_api_key: false,
            enabled: Some(true),
            priority: Some(100),
            extra_headers: Some(HashMap::from([(
                "x-provider-mode".into(),
                "private-header-value".into(),
            )])),
            source: None,
            provenance: None,
            models: Some(vec![SupplierModelInput {
                model_id: "acme-code".into(),
                display_name: Some("Acme Code".into()),
                enabled: true,
                supports_tools: Some(true),
                supports_images: Some(false),
                context_window: Some(64_000),
                max_output_tokens: Some(8_192),
                tier_override: Some(2),
                source: Some("manual".into()),
                provenance: None,
            }]),
        }
    }

    #[test]
    fn malformed_stored_provenance_is_reported_instead_of_silently_defaulted() {
        let db = db();
        let created = save_supplier(&db, save_input(None, 0, Some("sk-provenance"))).unwrap();
        {
            let conn = db.conn.lock().expect("database lock");
            conn.execute(
                "UPDATE suppliers SET provenance_json = '{\"sourceKind\": \"bad value\"}'
                 WHERE id = ?1",
                [&created.supplier_id],
            )
            .expect("corrupt provenance fixture");
        }

        let error = list_suppliers(&db).unwrap_err();
        assert!(error.to_string().contains("supplier_provenance_invalid"));
    }

    #[test]
    fn save_rejects_malformed_legacy_vendors_json_instead_of_trusting_defaults() {
        let db = db();
        {
            let conn = db.conn.lock().expect("database lock");
            conn.execute(
                "INSERT INTO vendors (id, name, base_url, api_key, protocols, endpoints, created_at)
                 VALUES ('malformed', 'Broken Vendor', '', '', 'not-json', '{}', 100)",
                [],
            )
            .expect("malformed vendor fixture");
        }

        let save = SupplierSaveInput {
            id: None,
            expected_generation: 0,
            name: "Broken Vendor".into(),
            base_url: None,
            api_key: Some("sk-new-key".into()),
            enabled: true,
            priority: 100,
            source: None,
            provenance: None,
            icon: None,
            icon_color: None,
            endpoints: Some(vec![endpoint_input()]),
        };
        let error = save_supplier(&db, save).unwrap_err();
        let detail = error.to_string();
        assert!(
            detail.contains(MALFORMED_VENDOR_DETAIL),
            "unexpected save error: {detail}"
        );
        assert_eq!(supplier_config_generation(&db).unwrap(), 0);
        assert!(list_suppliers(&db).is_err());
        assert!(db.get_all_vendors().is_err());
        let conn = db.conn.lock().expect("database lock");
        let vendor_raw = conn
            .query_row(
                "SELECT protocols FROM vendors WHERE id = 'malformed'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("the bad row must be preserved verbatim");
        assert_eq!(vendor_raw, "not-json");
        assert!(
            serde_json::from_str::<serde_json::Value>(&vendor_raw).is_err(),
            "the stored payload must really be unparseable JSON"
        );
        let vendors: i64 = conn
            .query_row("SELECT COUNT(*) FROM vendors", [], |row| row.get(0))
            .expect("count vendors");
        assert_eq!(vendors, 1);
    }

    #[test]
    fn list_supplier_views_preserve_legacy_model_order_from_sort_index() {
        let db = db();
        let supplier_id = supplier_dao::legacy_supplier_id("proxy_provider", "order-legacy");
        let endpoint_id =
            supplier_dao::endpoint_id_for(&supplier_id, supplier_dao::PROTOCOL_ANTHROPIC);
        {
            let conn = db.conn.lock().expect("database lock");
            conn.execute(
                "INSERT INTO suppliers (
                     id, name, base_url, api_key, enabled, priority, source, provenance_json,
                     created_at, updated_at
                 ) VALUES (?1, 'Ordered Legacy', 'https://legacy.example/v1', 'sk-order', 1, 100,
                           'legacy_proxy_provider', '{}', 100, 100)",
                [&supplier_id],
            )
            .expect("insert supplier");
            conn.execute(
                "INSERT INTO supplier_endpoints (
                     id, supplier_id, protocol, endpoint_url, api_key_override, enabled, priority,
                     extra_headers, source, provenance_json, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, NULL, NULL, 1, 0, '{}', 'legacy_proxy_provider', '{}', 100, 100)",
                rusqlite::params![
                    endpoint_id,
                    supplier_id,
                    supplier_dao::PROTOCOL_ANTHROPIC
                ],
            )
            .expect("insert endpoint");
            // Migrated rows keep an unmanaged anchor to their pre-existing
            // legacy provider (managed = 0), matching the v20→v21 backfill.
            conn.execute(
                "INSERT INTO supplier_runtime_projections (
                     endpoint_id, proxy_provider_id, managed
                 ) VALUES (?1, ?2, 0)",
                rusqlite::params![endpoint_id, "order-legacy"],
            )
            .expect("insert legacy projection anchor");
            for (index, model) in ["zeta", "alpha", "beta"].into_iter().enumerate() {
                let model_id = supplier_dao::model_id_for(&endpoint_id, model);
                conn.execute(
                    "INSERT INTO supplier_models (
                         id, endpoint_id, model_id, display_name, enabled, source,
                         provenance_json, sort_index, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, NULL, 1, 'legacy_proxy_provider', '{}', ?4, 100, 100)",
                    rusqlite::params![model_id, endpoint_id, model, index as i64],
                )
                .expect("insert model");
            }
        }

        let list = list_suppliers(&db).expect("list suppliers");
        let endpoint = list
            .suppliers
            .into_iter()
            .find(|supplier| supplier.id == supplier_id)
            .and_then(|supplier| supplier.endpoints.into_iter().next())
            .expect("supplier endpoint");
        let ordered: Vec<String> = endpoint
            .models
            .into_iter()
            .map(|model| model.model_id)
            .collect();
        assert_eq!(ordered, vec!["zeta", "alpha", "beta"]);
        assert!(endpoint.legacy_projection_available);
        let status = supplier_status(&db).expect("supplier status");
        assert_eq!(status.configured_endpoint_count, 1);
        assert_eq!(status.legacy_projected_endpoint_count, 0);
    }

    #[test]
    fn runtime_snapshot_never_returns_invalid_urls_or_passthrough_markers() {
        let db = db();
        let passthrough_id = supplier_dao::legacy_supplier_id("passthrough", "pending-official");
        let invalid_id = {
            let mut input = save_input(None, 0, Some("sk-invalid-url"));
            input.name = "Invalid URL".into();
            save_supplier(&db, input)
                .expect("create invalid-url supplier")
                .supplier_id
        };
        {
            let conn = db.conn.lock().expect("database lock");
            conn.execute(
                "INSERT INTO suppliers (
                     id, name, base_url, api_key, enabled, priority, source, provenance_json,
                     created_at, updated_at
                 ) VALUES (?1, 'Pending Official', '', '', 0, 100, 'official_auth_pending',
                           '{\"sourceKind\":\"official_auth_pending\"}', 100, 100)",
                [&passthrough_id],
            )
            .expect("insert pending official supplier");
            conn.execute(
                "UPDATE suppliers SET base_url = 'https://user:pass@api.example.invalid/v1'
                 WHERE id = ?1",
                [&invalid_id],
            )
            .expect("corrupt stored url fixture");
        }

        let snapshot = resolved_supplier_snapshot(&db).expect("resolved supplier snapshot");
        assert!(!snapshot
            .endpoints
            .iter()
            .any(|endpoint| endpoint.supplier_id == passthrough_id));
        assert!(!snapshot
            .endpoints
            .iter()
            .any(|endpoint| endpoint.supplier_id == invalid_id));

        let status = supplier_status(&db).expect("supplier status");
        assert_eq!(status.pending_unsupported_count, 1);
        let listed = list_suppliers(&db).expect("listed suppliers");
        assert!(listed
            .suppliers
            .iter()
            .any(|supplier| supplier.id == passthrough_id
                && supplier.availability == "pending_unsupported"));
        let invalid = listed
            .suppliers
            .iter()
            .find(|supplier| supplier.id == invalid_id)
            .expect("invalid supplier view");
        assert_eq!(invalid.availability, "needs_configuration");
        assert!(invalid.url_requires_review);
        assert!(invalid.endpoints[0].effective_base_url.is_none());
        assert!(invalid.endpoints[0].url_requires_review);
    }

    #[test]
    fn save_preserves_omitted_key_and_empty_key_clears_it() {
        let db = db();
        let created = save_supplier(&db, save_input(None, 0, Some("sk-private-first"))).unwrap();
        let after_create = get_supplier(&db, &created.supplier_id).unwrap();
        assert_eq!(after_create.generation, created.generation);
        assert!(after_create.supplier.unwrap().api_key_configured);

        let mut update = save_input(Some(&created.supplier_id), created.generation, None);
        update.name = "Acme Relay Renamed".into();
        let preserved = save_supplier(&db, update).unwrap();
        let runtime = resolved_supplier_snapshot(&db).unwrap();
        assert_eq!(runtime.endpoints[0].api_key, "sk-private-first");

        let cleared = save_supplier(
            &db,
            save_input(Some(&created.supplier_id), preserved.generation, Some("")),
        )
        .unwrap();
        let view = get_supplier(&db, &created.supplier_id)
            .unwrap()
            .supplier
            .unwrap();
        assert!(!view.api_key_configured);
        assert!(resolved_supplier_snapshot(&db).unwrap().endpoints[0]
            .api_key
            .is_empty());
        assert_eq!(cleared.generation, preserved.generation + 1);
    }

    #[test]
    fn supplier_icons_round_trip_and_mirror_onto_the_projection() {
        let db = db();
        let projection_icons = |db: &Database| -> (Option<String>, Option<String>) {
            let conn = db.conn.lock().expect("database lock");
            conn.query_row(
                "SELECT icon, icon_color FROM proxy_providers WHERE id LIKE 'supplier_%'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("projection row")
        };
        let view_icons = |db: &Database, id: &str| -> (Option<String>, Option<String>) {
            let supplier = get_supplier(db, id).unwrap().supplier.unwrap();
            (supplier.icon, supplier.icon_color)
        };

        let mut create = save_input(None, 0, Some("sk-icons"));
        create.icon = Some("whale".into());
        create.icon_color = Some("#1E88E5".into());
        let created = save_supplier(&db, create).unwrap();
        assert_eq!(
            view_icons(&db, &created.supplier_id),
            (Some("whale".into()), Some("#1E88E5".into()))
        );
        assert_eq!(
            projection_icons(&db),
            (Some("whale".into()), Some("#1E88E5".into()))
        );

        // Omission preserves the stored display fields...
        let mut patch = save_input(Some(&created.supplier_id), created.generation, None);
        patch.icon = None;
        patch.icon_color = None;
        let preserved = save_supplier(&db, patch).unwrap();
        assert_eq!(
            view_icons(&db, &created.supplier_id),
            (Some("whale".into()), Some("#1E88E5".into()))
        );

        // ...while an explicit empty string returns to the monogram and
        // clears the mirrored projection icons with it.
        let mut clear = save_input(Some(&created.supplier_id), preserved.generation, None);
        clear.icon = Some("".into());
        clear.icon_color = Some("".into());
        save_supplier(&db, clear).unwrap();
        assert_eq!(view_icons(&db, &created.supplier_id), (None, None));
        assert_eq!(projection_icons(&db), (None, None));

        // A control character is a boundary error, never a stored record.
        let mut bad = save_input(None, 0, None);
        bad.icon = Some("bad\nicon".into());
        let error = save_supplier(&db, bad).unwrap_err();
        assert!(
            error.to_string().contains("supplier_icon_invalid"),
            "unexpected save error: {error}"
        );
    }

    #[test]
    fn same_name_suppliers_keep_independent_credentials() {
        let db = db();
        let first = save_supplier(&db, save_input(None, 0, Some("sk-first"))).unwrap();
        let second =
            save_supplier(&db, save_input(None, first.generation, Some("sk-second"))).unwrap();
        let list = list_suppliers(&db).unwrap();
        assert_eq!(list.suppliers.len(), 2);
        assert_ne!(first.supplier_id, second.supplier_id);
        let runtime = resolved_supplier_snapshot(&db).unwrap();
        assert_eq!(runtime.endpoints.len(), 2);
        assert!(runtime
            .endpoints
            .iter()
            .any(|endpoint| endpoint.api_key == "sk-first"));
        assert!(runtime
            .endpoints
            .iter()
            .any(|endpoint| endpoint.api_key == "sk-second"));
    }

    #[test]
    fn views_never_serialize_keys_or_header_values() {
        let db = db();
        let created = save_supplier(&db, save_input(None, 0, Some("sk-view-secret-742"))).unwrap();
        let view = get_supplier(&db, &created.supplier_id)
            .unwrap()
            .supplier
            .unwrap();
        let serialized = serde_json::to_string(&view).unwrap();
        assert!(!serialized.contains("sk-view-secret-742"));
        assert!(!serialized.contains("private-header-value"));
        assert!(serialized.contains("x-provider-mode"));
        assert!(serialized.contains("apiKeyConfigured"));
        assert!(!serialized.contains("apiKey\""));
    }

    #[test]
    fn generation_conflicts_and_catalog_cache_do_not_change_it() {
        let db = db();
        let created = save_supplier(&db, save_input(None, 0, Some("sk-one"))).unwrap();
        let stale =
            save_supplier(&db, save_input(Some(&created.supplier_id), 0, None)).unwrap_err();
        assert!(stale
            .to_string()
            .contains("supplier_config_generation_conflict"));

        save_public_catalog_cache(
            &db,
            PublicCatalogCacheInput {
                source: "models_dev".into(),
                raw_json: json!({"model": {"tool_call": true}}).to_string(),
                fetched_at: chrono::Utc::now().timestamp(),
            },
        )
        .unwrap();
        let cached = get_public_catalog_cache(&db, "models_dev")
            .unwrap()
            .unwrap();
        assert!(cached.raw_json.contains("tool_call"));
        assert_eq!(supplier_status(&db).unwrap().generation, created.generation);

        record_public_catalog_error(&db, "models_dev", "timeout").unwrap();
        let after_error = get_public_catalog_cache(&db, "models_dev")
            .unwrap()
            .unwrap();
        assert_eq!(after_error.last_error.as_deref(), Some("timeout"));
        assert!(after_error.raw_json.contains("tool_call"));
    }

    #[test]
    fn input_validation_rejects_secret_bearing_catalog_and_unsafe_header() {
        let db = db();
        // Synthetic fixture secrets: sentinel values proving the validator
        // rejects secret-bearing input, never real credentials.
        let secret_catalog = PublicCatalogCacheInput {
            source: "models_dev".into(),
            raw_json: json!({"items": [{"apiKey": "do-not-store"}]}).to_string(), // fixture sentinel value
            fetched_at: chrono::Utc::now().timestamp(),
        };
        assert!(save_public_catalog_cache(&db, secret_catalog).is_err());
        let mut invalid = save_input(None, 0, Some("sk-one"));
        invalid.endpoints.as_mut().unwrap()[0]
            .extra_headers
            .as_mut()
            .unwrap()
            .insert("authorization".into(), "Bearer secret".into());
        assert!(save_supplier(&db, invalid).is_err());
    }

    #[test]
    fn unsafe_url_cannot_be_saved_or_exposed() {
        let db = db();
        let mut input = save_input(None, 0, Some("sk-url"));
        input.base_url =
            Some("https://user:password@api.example.invalid/v1?token=secret#frag".into());
        assert!(save_supplier(&db, input).is_err());
    }

    #[test]
    fn new_supplier_projection_does_not_mutate_legacy_rows_or_routes() {
        use crate::database::dao::proxy_providers::{CircuitBreakerConfig, ProxyProvider};
        use crate::database::dao::routes::{Route, RouteHop};

        let db = db();
        let legacy = ProxyProvider {
            id: "legacy-stays".into(),
            name: "Legacy provider".into(),
            provider_type: "openai".into(),
            api_key: "sk-legacy-private".into(),
            base_url: "https://legacy.example.invalid/v1".into(),
            models: vec!["legacy-model".into()],
            enabled: true,
            timeout_ms: 60_000,
            weight: 7,
            circuit_breaker: CircuitBreakerConfig::default(),
            model_mapping: HashMap::from([("alias".into(), "legacy-model".into())]),
            extra_headers: HashMap::new(),
            auto_track_models: true,
            icon: None,
            icon_color: None,
        };
        db.save_proxy_provider(&legacy).unwrap();
        db.save_route(&Route {
            alias: "legacy-model".into(),
            description: "legacy route".into(),
            hops: vec![RouteHop {
                provider: legacy.id.clone(),
                model: "legacy-model".into(),
            }],
        })
        .unwrap();

        let created = save_supplier(&db, save_input(None, 0, Some("sk-new"))).unwrap();
        assert_eq!(db.get_proxy_provider(&legacy.id).unwrap().unwrap(), legacy);
        let projected_id = resolved_supplier_snapshot(&db).unwrap().endpoints[0]
            .legacy_proxy_provider_id
            .clone()
            .unwrap();
        assert_ne!(projected_id, legacy.id);
        assert!(db.get_proxy_provider(&projected_id).unwrap().is_some());
        assert_eq!(
            db.get_route("legacy-model").unwrap().unwrap().hops[0].provider,
            legacy.id
        );

        delete_supplier(&db, &created.supplier_id, created.generation).unwrap();
        assert_eq!(
            db.get_proxy_provider("legacy-stays").unwrap().unwrap(),
            legacy
        );
        assert!(db.get_proxy_provider(&projected_id).unwrap().is_none());
    }

    #[test]
    fn deleting_supplier_is_rejected_while_its_projection_is_routed() {
        use crate::database::dao::routes::{Route, RouteHop};

        let db = db();
        let created = save_supplier(&db, save_input(None, 0, Some("sk-routed"))).unwrap();
        let projected_id = resolved_supplier_snapshot(&db).unwrap().endpoints[0]
            .legacy_proxy_provider_id
            .clone()
            .unwrap();
        db.save_route(&Route {
            alias: "routed-model".into(),
            description: String::new(),
            hops: vec![RouteHop {
                provider: projected_id,
                model: "acme-code".into(),
            }],
        })
        .unwrap();

        let error = delete_supplier(&db, &created.supplier_id, created.generation).unwrap_err();
        assert!(error.to_string().contains("supplier_in_use"));
        assert_eq!(supplier_config_generation(&db).unwrap(), created.generation);
        assert!(get_supplier(&db, &created.supplier_id)
            .unwrap()
            .supplier
            .is_some());

        db.delete_route("routed-model").unwrap();
        assert_eq!(
            delete_supplier(&db, &created.supplier_id, created.generation).unwrap(),
            created.generation + 1
        );
    }
}
