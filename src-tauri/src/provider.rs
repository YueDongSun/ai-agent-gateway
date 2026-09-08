//! Provider data model.
//!
//! Mirrors cc-switch's `Provider` / `ProviderMeta` (`src-tauri/src/provider.rs:11`),
//! trimmed to the fields AAG needs in Phase 1. `settings_config` is a JSON blob
//! whose shape depends on `app_type` - the adapter layer interprets it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// A provider configuration row. One provider per `(id, app_type)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    #[serde(rename = "settingsConfig")]
    pub settings_config: Value,
    #[serde(skip_serializing_if = "Option::is_none", rename = "websiteUrl")]
    pub website_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "createdAt")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "sortIndex")]
    pub sort_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "iconColor")]
    pub icon_color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<ProviderMeta>,
    /// Whether this provider is in the failover queue for its app.
    #[serde(default, rename = "inFailoverQueue")]
    pub in_failover_queue: bool,
    /// When true, the auto-sync path (cc-switch -> AAG) skips this row
    /// instead of overwriting it. Lets users keep manual edits while
    /// still auto-pulling new upstream entries.
    #[serde(default, rename = "syncLocked")]
    pub sync_locked: bool,
}

impl Provider {
    #[allow(dead_code)]
    pub fn new(id: String, name: String, settings_config: Value) -> Self {
        Self {
            id,
            name,
            settings_config,
            website_url: None,
            category: None,
            created_at: Some(chrono::Utc::now().timestamp()),
            sort_index: None,
            notes: None,
            icon: None,
            icon_color: None,
            meta: None,
            in_failover_queue: true,
            sync_locked: false,
        }
    }
}

/// Provider metadata - stored alongside the provider but not written to live
/// agent config files. Extends as features land (custom endpoints, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderMeta {
    /// Custom endpoint URLs keyed by URL (Phase 5+ will populate this).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub custom_endpoints: HashMap<String, CustomEndpoint>,
}

/// A custom endpoint entry (mirrors cc-switch's `settings::CustomEndpoint`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomEndpoint {
    pub url: String,
    pub added_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<i64>,
}