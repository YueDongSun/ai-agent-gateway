//! IPC and service DTOs for supplier management.
//!
//! Credential values are accepted only on write requests and are never part of
//! the serializable read views. Runtime-only resolved records live in
//! `services::suppliers::mod` and intentionally do not implement serde traits.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

fn default_true() -> bool {
    true
}

fn default_priority() -> i64 {
    100
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupplierProvenance {
    /// Stable source category, such as `manual`, `cc_switch`, or
    /// `models_dev`; never an authorization token.
    pub source_kind: Option<String>,
    /// Redacted or hashed source-row identity, not the source credential.
    pub source_record_ref: Option<String>,
    pub source_app_type: Option<String>,
    pub source_endpoint_ref: Option<String>,
    pub import_batch_ref: Option<String>,
    pub fetched_at: Option<i64>,
    pub content_version: Option<String>,
    pub confidence: Option<f64>,
}

/// Tier values are an explicit ordinal contract: 0 is the lowest tier, 3 the
/// highest. `None` means no manual override / pending evidence.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupplierModelInput {
    pub model_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub supports_tools: Option<bool>,
    #[serde(default)]
    pub supports_images: Option<bool>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub tier_override: Option<u8>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub provenance: Option<SupplierProvenance>,
}

/// An endpoint is addressed by protocol; its persisted endpoint ID is
/// deterministically derived from the stable supplier ID and this protocol.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupplierEndpointInput {
    pub protocol: String,
    /// `None` preserves an existing override. An empty string clears the
    /// override and uses the supplier's default base URL.
    #[serde(default)]
    pub endpoint_url: Option<String>,
    /// `None` preserves an existing override. An empty string deliberately
    /// disables endpoint-specific authentication; set `useSharedApiKey` to
    /// return to the supplier key.
    #[serde(default)]
    pub api_key_override: Option<String>,
    #[serde(default)]
    pub use_shared_api_key: bool,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub priority: Option<i64>,
    /// Header values are write-only. Read views return only sorted names.
    #[serde(default)]
    pub extra_headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub provenance: Option<SupplierProvenance>,
    /// `None` preserves the current model list; an empty list clears it.
    #[serde(default)]
    pub models: Option<Vec<SupplierModelInput>>,
}

/// Supplier save is an optimistic-generation patch. In particular, omitted
/// `api_key` retains the stored key while `apiKey: ""` clears it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupplierSaveInput {
    #[serde(default)]
    pub id: Option<String>,
    pub expected_generation: u64,
    pub name: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_priority")]
    pub priority: i64,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub provenance: Option<SupplierProvenance>,
    /// Display glyph id or a literal character (letter/emoji). Omitted
    /// preserves the stored icon; `""` returns to the name monogram.
    #[serde(default)]
    pub icon: Option<String>,
    /// Tile background. Omitted preserves the stored color; `""` clears it.
    #[serde(default)]
    pub icon_color: Option<String>,
    /// Omitted preserves endpoints; an explicit empty array removes them.
    #[serde(default)]
    pub endpoints: Option<Vec<SupplierEndpointInput>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupplierModelsUpdateInput {
    pub supplier_id: String,
    pub protocol: String,
    pub expected_generation: u64,
    pub models: Vec<SupplierModelInput>,
}

/// Read-only model probe using an unsaved endpoint draft. Omitted fields use
/// stored values when `supplierId` is present; explicit empty strings clear
/// draft values without changing the saved supplier.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupplierDiscoverModelsInput {
    pub supplier_id: Option<String>,
    pub protocol: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub endpoint_url: Option<String>,
    pub api_key_override: Option<String>,
    #[serde(default)]
    pub use_shared_api_key: bool,
    pub extra_headers: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierListView {
    pub generation: u64,
    pub suppliers: Vec<SupplierView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierGetResult {
    pub generation: u64,
    pub supplier: Option<SupplierView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierMutationResult {
    pub supplier_id: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierView {
    pub id: String,
    pub name: String,
    /// Safe for display: URL credentials, query strings and fragments are
    /// removed. Invalid stored URLs become `None` and set `url_requires_review`.
    pub base_url: Option<String>,
    pub enabled: bool,
    pub priority: i64,
    pub source: String,
    pub provenance: SupplierProvenance,
    pub api_key_configured: bool,
    /// Display glyph id or literal character; `None` renders a monogram.
    pub icon: Option<String>,
    /// Tile background; `None` uses the deterministic monogram hue.
    pub icon_color: Option<String>,
    pub availability: String,
    pub url_requires_review: bool,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub endpoints: Vec<SupplierEndpointView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierEndpointView {
    pub id: String,
    pub protocol: String,
    /// The per-endpoint override (if present), sanitized for UI display.
    pub endpoint_url: Option<String>,
    pub effective_base_url: Option<String>,
    pub enabled: bool,
    pub priority: i64,
    /// One of `shared`, `override`, or `none`; never includes the credential.
    pub auth_mode: String,
    pub header_names: Vec<String>,
    pub source: String,
    pub provenance: SupplierProvenance,
    pub url_requires_review: bool,
    pub legacy_projection_available: bool,
    /// True only when the legacy proxy projection is owned by the supplier
    /// service (`managed`); anchor rows over pre-existing legacy providers
    /// report `false`.
    pub legacy_projection_managed: bool,
    pub models: Vec<SupplierModelView>,
    pub alternatives: Vec<SupplierEndpointAlternativeView>,
    /// Catalog reachability never establishes successful model generation.
    pub discovery_health: Option<crate::database::dao::suppliers::SupplierDiscoveryHealth>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierEndpointAlternativeView {
    pub id: String,
    pub endpoint_url: Option<String>,
    pub enabled: bool,
    pub priority: i64,
    pub api_key_configured: bool,
    pub header_names: Vec<String>,
    pub model_ids: Vec<String>,
    pub source: String,
    pub url_requires_review: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierModelView {
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
    pub provenance: SupplierProvenance,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierStatus {
    pub generation: u64,
    pub supplier_count: usize,
    pub enabled_supplier_count: usize,
    pub endpoint_count: usize,
    pub enabled_endpoint_count: usize,
    /// Enabled endpoints with a safe, usable base URL; this is configuration
    /// state only and does not claim that an upstream is reachable.
    pub configured_endpoint_count: usize,
    /// Enabled configured endpoints that have a managed legacy proxy projection.
    pub legacy_projected_endpoint_count: usize,
    pub pending_unsupported_count: usize,
    /// Always `false` until official-account authorization forwarding is
    /// implemented and separately authorized.
    pub official_auth_forwarding_supported: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicCatalogCache {
    pub source: String,
    /// A bounded, valid public JSON document; it is not an upstream response
    /// body or credential store.
    pub raw_json: String,
    pub fetched_at: i64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicCatalogCacheInput {
    pub source: String,
    pub raw_json: String,
    pub fetched_at: i64,
}
