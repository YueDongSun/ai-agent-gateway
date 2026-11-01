//! Supplier-import preview: pure derivation of what an apply would write.
//!
//! Preview has zero configuration side effects — it reads the cc-switch
//! source (read-only) and AAG's current supplier snapshot, then derives
//! a redacted plan bound to the source fingerprint and the target
//! configuration generation through a self-certifying `plan_id`. The
//! apply side re-derives the same plan and refuses any drift.
//!
//! Nothing in this module writes, probes the network, or reads
//! credential files; official/managed auth rows become explicit
//! `pending_unsupported` associations, never usable API suppliers.

use crate::database::dao::suppliers::{self as supplier_dao, SupplierModelRecord, SupplierRecord};
use crate::database::Database;
use crate::error::AppError;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use super::cc_switch_db::{
    map_cc_switch_icon, map_cc_switch_icon_color, read_cc_switch_providers, CcSwitchRow,
    MAX_SETTINGS_CONFIG_BYTES, MAX_SOURCE_ROWS,
};
use super::settings_extract::{
    base_host_is_official_anthropic, base_host_is_official_openai, declared_wire_protocol,
    default_wire_protocol, extract_declared_headers, extract_upstream_pair, parse_codex_toml,
    settings_env, WireProtocol,
};

const PLAN_ID_PREFIX: &str = "ccsplan";
const FINGERPRINT_DOMAIN: &str = "cc-switch-source-v1";
const ENTRIES_DOMAIN: &str = "cc-switch-entries-v1";
const MAX_PREVIEW_MODELS: usize = 64;
const MAX_NAME_CHARS: usize = 128;
const MAX_URL_BYTES: usize = 2048;
const MAX_API_KEY_BYTES: usize = 16 * 1024;
const MAX_MODELS_PER_ENDPOINT: usize = 1000;
const MAX_HEADER_COUNT: usize = 32;
const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;

/// One supplier the apply would create or refresh. Secrets are never part
/// of any serializable type here; [`PlannedSupplier`] (internal) carries
/// them only in memory.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierImportPlanEntry {
    /// Deterministic supplier id (hashed from source identity). Stable
    /// across re-imports and free of source-provided text.
    pub supplier_id: String,
    pub source_app_type: String,
    pub source_id: String,
    pub name: String,
    pub protocol: String,
    /// Sanitized display URL (credentials, query, fragment removed).
    pub base_url: Option<String>,
    pub api_key_configured: bool,
    pub header_names: Vec<String>,
    /// Model ids in source order (bounded for display).
    pub model_ids: Vec<String>,
    pub model_count: usize,
    /// `create`, `refresh`, or `unchanged` against the current database.
    pub change: String,
    /// Whether a managed legacy proxy projection is part of the apply.
    pub legacy_projection_planned: bool,
    /// Endpoint URLs recorded in cc-switch's `provider_endpoints`.
    pub source_endpoint_urls: Vec<String>,
}

/// Official/managed authentication rows: only the non-sensitive
/// association is recorded, and the supplier stays unusable until
/// official-account authorization forwarding exists.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierImportPendingEntry {
    pub supplier_id: String,
    pub source_app_type: String,
    pub source_id: String,
    pub name: String,
    pub reason: String,
}

/// A source record the import cannot use, explained instead of dropped.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierImportProblem {
    pub source_app_type: Option<String>,
    pub source_id: Option<String>,
    pub code: String,
    pub detail: String,
}

/// The redacted import plan returned by `supplier_import_preview`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierImportPreview {
    /// Self-certifying plan id: binds the source fingerprint, the target
    /// configuration generation, and the derived entry digest. Opaque to
    /// callers; pass it verbatim to `supplier_import_apply`.
    pub plan_id: String,
    pub source_fingerprint: String,
    pub target_generation: u64,
    pub created_at: i64,
    /// Safe source label — never a filesystem path.
    pub source_label: String,
    pub total_rows: usize,
    pub plans: Vec<SupplierImportPlanEntry>,
    pub pending_unsupported: Vec<SupplierImportPendingEntry>,
    pub excluded_oauth_count: usize,
    pub problems: Vec<SupplierImportProblem>,
}

/// Internal, secret-bearing planned supplier row used by apply. Never
/// serialized; never logged.
#[derive(Clone)]
pub(crate) struct PlannedSupplier {
    pub(crate) supplier_id: String,
    pub(crate) source_app_type: String,
    pub(crate) source_id: String,
    pub(crate) name: String,
    pub(crate) protocol: WireProtocol,
    pub(crate) base_url: String,
    pub(crate) supplier_base_url: String,
    pub(crate) alternatives: Vec<PlannedAlternative>,
    pub(crate) api_key: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) models: Vec<String>,
    /// Display icon for the legacy projection rows: an AAG glyph id or
    /// literal character mapped from the source row's cc-switch icon.
    pub(crate) icon: Option<String>,
    pub(crate) icon_color: Option<String>,
}

#[derive(Clone)]
pub(crate) struct PlannedAlternative {
    pub(crate) source_app_type: String,
    pub(crate) source_id: String,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) models: Vec<String>,
}

/// Internal pending (official/managed auth) association.
pub(crate) struct PlannedPending {
    pub(crate) supplier_id: String,
    pub(crate) source_app_type: String,
    pub(crate) source_id: String,
    pub(crate) name: String,
    pub(crate) base_url: String,
    pub(crate) reason: String,
}

/// The full derivation result shared by preview and apply.
pub(crate) struct DerivedPlan {
    pub(crate) entries: Vec<PlannedSupplier>,
    pub(crate) pending: Vec<PlannedPending>,
    pub(crate) problems: Vec<SupplierImportProblem>,
    pub(crate) source_fingerprint: String,
    pub(crate) total_rows: usize,
    pub(crate) excluded_oauth_count: usize,
}

/// Build the redacted preview. Reads only; writes nothing.
pub(crate) fn build_preview(
    db: &Database,
    source_path: &std::path::Path,
) -> Result<SupplierImportPreview, AppError> {
    let rows = read_source_rows(source_path)?;
    let (generation, existing) = db.get_supplier_snapshot()?;
    let derived = derive_plan(&rows, &existing, db)?;
    let plan_id = plan_id_for(
        &derived.source_fingerprint,
        generation,
        &plan_digest(&derived.entries, &derived.pending),
    );
    let icons = super::supplier_import::proxy_provider_icon_state(db)?;
    let changes: Vec<_> = derived
        .entries
        .iter()
        .map(|entry| {
            if !existing.iter().any(|record| record.id == entry.supplier_id) {
                "create"
            } else if plan_already_applied(std::slice::from_ref(entry), &[], &existing, &icons) {
                "unchanged"
            } else {
                "refresh"
            }
        })
        .collect();
    let mut preview = to_preview(derived, plan_id, generation, chrono::Utc::now().timestamp());
    annotate_preview(&mut preview, &rows, &existing);
    for (entry, change) in preview.plans.iter_mut().zip(changes) {
        entry.change = change.into();
    }
    Ok(preview)
}

/// Read the source with stable, path-free error codes: a file that is
/// not a usable cc-switch database surfaces as
/// `supplier_import_source_invalid`, never with its path attached.
pub(crate) fn read_source_rows(
    source_path: &std::path::Path,
) -> Result<Vec<CcSwitchRow>, AppError> {
    read_cc_switch_providers(source_path).map_err(|error| match error {
        AppError::InvalidInput(code) => AppError::InvalidInput(code),
        _ => AppError::InvalidInput("supplier_import_source_invalid".into()),
    })
}

fn push_problem(
    problems: &mut Vec<SupplierImportProblem>,
    row: &CcSwitchRow,
    code: &str,
    detail: &str,
) {
    problems.push(SupplierImportProblem {
        source_app_type: Some(row.app_type.clone()),
        source_id: Some(row.id.clone()),
        code: code.to_string(),
        detail: detail.to_string(),
    });
}

/// Derive the plan from source rows and current database state. Pure
/// with respect to writes; shared by preview and apply so the two can
/// never disagree.
pub(crate) fn derive_plan(
    rows: &[CcSwitchRow],
    existing: &[SupplierRecord],
    db: &Database,
) -> Result<DerivedPlan, AppError> {
    if rows.len() > MAX_SOURCE_ROWS {
        return Err(AppError::InvalidInput(
            "supplier_import_source_too_large".into(),
        ));
    }
    let fingerprint = source_fingerprint(rows);
    let listen = db
        .get_proxy_config()
        .ok()
        .map(|config| (config.host, config.port));
    let existing_by_id: HashMap<&str, &SupplierRecord> = existing
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();

    let mut entries: Vec<PlannedSupplier> = Vec::new();
    let pending: Vec<PlannedPending> = Vec::new();
    let mut excluded_oauth_count = 0;
    let mut problems: Vec<SupplierImportProblem> = Vec::new();
    for row in rows {
        if row.settings_config.len() > MAX_SETTINGS_CONFIG_BYTES {
            push_problem(
                &mut problems,
                row,
                "settings_config_too_large",
                "settings_config exceeds size limit",
            );
            continue;
        }
        if let Some(issue) = row.meta_issue {
            push_problem(&mut problems, row, issue, "meta column is not trustworthy");
            continue;
        }
        if row.endpoints_capped {
            push_problem(
                &mut problems,
                row,
                "endpoints_capped",
                "provider endpoint list exceeded the cap",
            );
        }
        let settings_value: serde_json::Value = match serde_json::from_str(&row.settings_config) {
            Ok(value) => value,
            Err(_) => {
                push_problem(
                    &mut problems,
                    row,
                    "settings_config_invalid",
                    "settings_config is not valid JSON",
                );
                continue;
            }
        };
        if let Some((host, port)) = &listen {
            if !host.is_empty()
                && crate::proxy::self_loop::settings_target_self(&settings_value, host, *port)
            {
                push_problem(
                    &mut problems,
                    row,
                    "targets_aag_proxy",
                    "settings_config targets this AAG proxy",
                );
                continue;
            }
        }
        let supplier_id = supplier_dao::legacy_supplier_id("cc_switch", &source_ref(row));
        if row.name.trim().is_empty()
            || row.name.chars().count() > MAX_NAME_CHARS
            || row.name.chars().any(char::is_control)
        {
            push_problem(
                &mut problems,
                row,
                "name_invalid",
                "provider name is empty or unsafe",
            );
            continue;
        }
        let name = row.name.clone();
        let meta = row.meta.as_ref();

        let (policy_base, policy_key) =
            extract_upstream_pair(&row.app_type, &settings_value, meta).unwrap_or_default();
        if super::import_policy::excludes_account_auth(
            row,
            &settings_value,
            &policy_base,
            &policy_key,
        ) {
            excluded_oauth_count += 1;
            continue;
        }

        let api_format = meta.and_then(|meta| meta.api_format.as_deref());
        let wire_api = if row.app_type == "codex" {
            settings_value
                .get("config")
                .and_then(serde_json::Value::as_str)
                .and_then(|toml| parse_codex_toml(toml).ok())
                .and_then(|info| info.wire_api)
        } else {
            None
        };
        let protocol = match declared_wire_protocol(api_format, wire_api.as_deref()) {
            Some(Ok(protocol)) => protocol,
            Some(Err(token)) => {
                push_problem(
                    &mut problems,
                    row,
                    "protocol_declaration_unknown",
                    &format!("unsupported protocol declaration: {token}"),
                );
                continue;
            }
            None => match default_wire_protocol(&row.app_type, &settings_value) {
                Some(protocol) => protocol,
                None => {
                    push_problem(
                        &mut problems,
                        row,
                        "app_type_unsupported",
                        "no supported app_type mapping",
                    );
                    continue;
                }
            },
        };
        let (raw_base, api_key) = match extract_upstream_pair(&row.app_type, &settings_value, meta)
        {
            Some(pair) => pair,
            None => {
                push_problem(
                    &mut problems,
                    row,
                    "upstream_unresolved",
                    "no usable upstream base URL in settings_config",
                );
                continue;
            }
        };
        let base_url = if raw_base.is_empty() {
            // Fall back to the recorded endpoint list when settings have
            // no base URL of their own.
            row.endpoint_urls
                .iter()
                .find(|url| validate_optional_url(url).is_ok())
                .cloned()
                .unwrap_or_default()
        } else {
            raw_base
        };
        if base_url.is_empty() {
            push_problem(
                &mut problems,
                row,
                "upstream_unresolved",
                "no usable upstream base URL",
            );
            continue;
        }
        if let Err(code) = validate_optional_url(&base_url) {
            push_problem(&mut problems, row, code, "upstream base URL is unusable");
            continue;
        }
        if api_key.len() > MAX_API_KEY_BYTES || api_key.chars().any(char::is_control) {
            push_problem(
                &mut problems,
                row,
                "api_key_invalid",
                "stored credential is malformed",
            );
            continue;
        }
        let is_official = row.category.as_deref() == Some("official")
            || match row.app_type.as_str() {
                "codex" => base_host_is_official_openai(&base_url),
                _ => base_host_is_official_anthropic(&base_url),
            };
        if api_key.is_empty() {
            if is_official {
                excluded_oauth_count += 1;
                continue;
            }
            push_problem(
                &mut problems,
                row,
                "missing_api_key",
                "no static API key and not an official account",
            );
            continue;
        }

        // Headers: declared only; invalid entries explained and dropped.
        let mut headers: Vec<(String, String)> = Vec::new();
        for (header_name, header_value) in extract_declared_headers(&row.app_type, &settings_value)
        {
            if headers.len() >= MAX_HEADER_COUNT {
                push_problem(
                    &mut problems,
                    row,
                    "header_dropped",
                    "header limit reached; extra headers dropped",
                );
                break;
            }
            match validate_header(&header_name, &header_value) {
                Ok(()) => headers.push((header_name, header_value)),
                Err(code) => push_problem(
                    &mut problems,
                    row,
                    "header_dropped",
                    &format!("{code}: {header_name}"),
                ),
            }
        }
        headers.sort_by(|left, right| {
            left.0
                .to_ascii_lowercase()
                .cmp(&right.0.to_ascii_lowercase())
        });

        let models = derive_models(row, &settings_value);
        if models.len() > MAX_MODELS_PER_ENDPOINT {
            push_problem(
                &mut problems,
                row,
                "models_too_many",
                "model list exceeds the endpoint cap",
            );
            continue;
        }
        if models.iter().any(|model| {
            model.trim().is_empty() || model.len() > 256 || model.chars().any(char::is_control)
        }) {
            push_problem(
                &mut problems,
                row,
                "model_invalid",
                "model id is empty or unsafe",
            );
            continue;
        }

        // Never overwrite a supplier the import does not own.
        if let Some(existing_record) = existing_by_id.get(supplier_id.as_str()) {
            if !import_owns(existing_record) {
                push_problem(
                    &mut problems,
                    row,
                    "import_target_conflict",
                    "a manually configured supplier already uses this identity",
                );
                continue;
            }
        }

        entries.push(PlannedSupplier {
            supplier_id,
            source_app_type: row.app_type.clone(),
            source_id: row.id.clone(),
            name,
            protocol,
            supplier_base_url: base_url.clone(),
            alternatives: row
                .endpoint_urls
                .iter()
                .filter(|url| *url != &base_url && validate_optional_url(url).is_ok())
                .map(|url| PlannedAlternative {
                    source_app_type: row.app_type.clone(),
                    source_id: row.id.clone(),
                    base_url: url.clone(),
                    api_key: api_key.clone(),
                    headers: headers.clone(),
                    models: models.clone(),
                })
                .collect(),
            base_url,
            api_key,
            headers,
            models,
            icon: map_cc_switch_icon(row.icon.as_deref()),
            icon_color: map_cc_switch_icon_color(row.icon_color.as_deref()),
        });
    }
    let entries = merge_plan_entries(entries, existing);
    Ok(DerivedPlan {
        entries,
        pending,
        problems,
        source_fingerprint: fingerprint,
        total_rows: rows.len(),
        excluded_oauth_count,
    })
}

fn merge_plan_entries(
    entries: Vec<PlannedSupplier>,
    existing: &[SupplierRecord],
) -> Vec<PlannedSupplier> {
    let mut groups: Vec<Vec<PlannedSupplier>> = Vec::new();
    let mut group_keys: Vec<(String, String)> = Vec::new();
    for entry in entries {
        let identity = super::import_policy::vendor_identity(&entry.name, &entry.base_url)
            .unwrap_or_else(|| entry.supplier_id.clone());
        let key = (identity, entry.api_key.clone());
        if let Some(index) = group_keys.iter().position(|candidate| candidate == &key) {
            groups[index].push(entry);
        } else {
            group_keys.push(key);
            groups.push(vec![entry]);
        }
    }
    let mut merged: Vec<PlannedSupplier> = Vec::new();
    for (group, (identity, credential)) in groups.into_iter().zip(group_keys) {
        let mut ids: Vec<String> = existing
            .iter()
            .filter(|saved| {
                import_owns(saved)
                    && saved.api_key == credential
                    && super::import_policy::vendor_identity(&saved.name, &saved.base_url)
                        .as_deref()
                        == Some(identity.as_str())
            })
            .map(|saved| saved.id.clone())
            .collect();
        if ids.is_empty() {
            ids = group
                .iter()
                .map(|entry| entry.supplier_id.clone())
                .collect();
        }
        ids.sort();
        let supplier_id = ids[0].clone();
        let supplier_base_url = existing
            .iter()
            .find(|saved| saved.id == supplier_id)
            .map(|saved| saved.base_url.clone())
            .unwrap_or_else(|| group[0].base_url.clone());
        let name = group
            .iter()
            .min_by_key(|entry| entry.name.len())
            .unwrap()
            .name
            .clone();
        for mut entry in group {
            entry.supplier_id = supplier_id.clone();
            entry.supplier_base_url = supplier_base_url.clone();
            entry.name = name.clone();
            if let Some(primary) = merged
                .iter_mut()
                .find(|saved| saved.supplier_id == supplier_id && saved.protocol == entry.protocol)
            {
                if primary.base_url == entry.base_url && primary.headers == entry.headers {
                    for model in entry.models {
                        if !primary.models.contains(&model) {
                            primary.models.push(model);
                        }
                    }
                } else {
                    primary.alternatives.push(PlannedAlternative {
                        source_app_type: entry.source_app_type,
                        source_id: entry.source_id,
                        base_url: entry.base_url,
                        api_key: entry.api_key,
                        headers: entry.headers,
                        models: entry.models,
                    });
                }
                primary.alternatives.extend(entry.alternatives);
            } else {
                merged.push(entry);
            }
        }
    }
    merged
}

/// True when an existing supplier row was created by this importer.
pub(crate) fn import_owns(record: &SupplierRecord) -> bool {
    record.source == "cc_switch" || record.source == "official_auth_pending"
}

fn source_ref(row: &CcSwitchRow) -> String {
    format!("{}:{}", row.app_type, row.id)
}

fn derive_models(row: &CcSwitchRow, settings_value: &serde_json::Value) -> Vec<String> {
    let mut models: Vec<String> = Vec::new();
    let mut push = |model: String| {
        if !models.contains(&model) {
            models.push(model);
        }
    };
    match row.app_type.as_str() {
        "claude_code" | "claude" | "claude_desktop" => {
            for model in super::row_builders::tier_models(settings_env(settings_value)) {
                if let Some(model) = model {
                    push(model);
                }
            }
        }
        "codex" => {
            for model in super::settings_extract::codex_catalog_models(settings_value) {
                push(model);
            }
        }
        "gemini" => {
            let env = settings_env(settings_value);
            if let Some(model) = env
                .get("GEMINI_MODEL")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|model| !model.is_empty())
            {
                push(model.to_string());
            }
            if let Some(mapping) = settings_value
                .get("modelMapping")
                .and_then(serde_json::Value::as_object)
            {
                for value in mapping.values() {
                    if let Some(model) = value.as_str().map(str::trim).filter(|m| !m.is_empty()) {
                        push(model.to_string());
                    }
                }
            }
        }
        _ => {}
    }
    models
}

/// Fingerprint of the raw source rows. Binds the plan to the exact source
/// bytes so any cc-switch edit between preview and apply is detected.
pub(super) fn source_fingerprint(rows: &[CcSwitchRow]) -> String {
    let mut digest = Sha256::new();
    digest.update(FINGERPRINT_DOMAIN.as_bytes());
    digest.update((rows.len() as u64).to_be_bytes());
    for row in rows {
        for part in [
            row.id.as_str(),
            row.app_type.as_str(),
            row.name.as_str(),
            row.settings_config.as_str(),
            row.meta_json.as_str(),
            row.icon.as_deref().unwrap_or(""),
            row.icon_color.as_deref().unwrap_or(""),
        ] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
        digest.update([row.is_current as u8, row.in_failover_queue as u8]);
        digest.update(
            row.sort_index
                .map(|index| index as u64)
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(row.created_at.unwrap_or(i64::MIN).to_be_bytes());
        digest.update((row.endpoint_urls.len() as u64).to_be_bytes());
        for url in &row.endpoint_urls {
            digest.update((url.len() as u64).to_be_bytes());
            digest.update(url.as_bytes());
        }
    }
    hex::encode(&digest.finalize()[..16])
}

/// Digest of the planned entries and pending associations: binds what
/// apply would write.
fn plan_digest(entries: &[PlannedSupplier], pending: &[PlannedPending]) -> String {
    let mut digest = Sha256::new();
    digest.update(ENTRIES_DOMAIN.as_bytes());
    digest.update((entries.len() as u64).to_be_bytes());
    for entry in entries {
        for part in [
            entry.supplier_id.as_str(),
            entry.source_app_type.as_str(),
            entry.source_id.as_str(),
            entry.name.as_str(),
            entry.protocol.supplier_protocol(),
            entry.base_url.as_str(),
            entry.supplier_base_url.as_str(),
        ] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
        digest.update([!entry.api_key.is_empty() as u8]);
        digest.update((entry.headers.len() as u64).to_be_bytes());
        for (name, value) in &entry.headers {
            digest.update((name.len() as u64).to_be_bytes());
            digest.update(name.as_bytes());
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value.as_bytes());
        }
        for alternative in &entry.alternatives {
            for value in [
                &alternative.source_app_type,
                &alternative.source_id,
                &alternative.base_url,
                &alternative.api_key,
            ] {
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            for (key, value) in &alternative.headers {
                digest.update(key.as_bytes());
                digest.update(value.as_bytes());
            }
            for model in &alternative.models {
                digest.update(model.as_bytes());
            }
        }
        digest.update((entry.models.len() as u64).to_be_bytes());
        for model in &entry.models {
            digest.update((model.len() as u64).to_be_bytes());
            digest.update(model.as_bytes());
        }
        // Display icons are plan content too: an icon edit in the source
        // must change the derived plan, not silently keep an old binding.
        for part in [
            entry.icon.as_deref().unwrap_or(""),
            entry.icon_color.as_deref().unwrap_or(""),
        ] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
    }
    digest.update((pending.len() as u64).to_be_bytes());
    for association in pending {
        for part in [
            association.supplier_id.as_str(),
            association.source_app_type.as_str(),
            association.source_id.as_str(),
            association.name.as_str(),
            association.base_url.as_str(),
            association.reason.as_str(),
        ] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
    }
    hex::encode(&digest.finalize()[..16])
}

/// Self-certifying plan id: `ccsplan_<fingerprint12>_<generation>_<digest12>`.
pub(crate) fn plan_id_for(fingerprint: &str, generation: u64, digest: &str) -> String {
    format!(
        "{PLAN_ID_PREFIX}_{}_{}_{}",
        &fingerprint[..12],
        generation,
        &digest[..12]
    )
}

/// Parsed plan binding.
pub(crate) struct PlanBinding {
    pub(crate) fingerprint12: String,
    pub(crate) target_generation: u64,
    pub(crate) digest12: String,
}

/// Parse a plan id produced by [`plan_id_for`]. Malformed values are an
/// error — a plan id is never recomputed from caller-supplied guesses.
pub(crate) fn parse_plan_id(plan_id: &str) -> Result<PlanBinding, AppError> {
    let invalid = || AppError::InvalidInput("supplier_import_plan_invalid".into());
    let parts: Vec<&str> = plan_id.split('_').collect();
    if parts.len() != 4 || parts[0] != PLAN_ID_PREFIX {
        return Err(invalid());
    }
    let fingerprint12 = parts[1];
    let digest12 = parts[3];
    let hex12 =
        |value: &str| value.len() == 12 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !hex12(fingerprint12) || !hex12(digest12) {
        return Err(invalid());
    }
    let target_generation = parts[2].parse::<u64>().map_err(|_| invalid())?;
    Ok(PlanBinding {
        fingerprint12: fingerprint12.to_ascii_lowercase(),
        target_generation,
        digest12: digest12.to_ascii_lowercase(),
    })
}

/// The derived entries digest as the plan id carries it.
pub(crate) fn derived_digest12(entries: &[PlannedSupplier], pending: &[PlannedPending]) -> String {
    plan_digest(entries, pending)[..12].to_string()
}

/// Compare the derived plan against the persisted state: the plan is
/// already applied when every entry and pending association exists with
/// identical content — including the display icons on both the supplier
/// row and its legacy projection (`proxy_provider_icons` maps
/// `proxy_providers.id` → (icon, icon_color)), so an icon-only source
/// edit re-applies instead of hiding behind supplier-level idempotency.
/// This is what makes a repeated apply idempotent without any ledger.
pub(crate) fn plan_already_applied(
    entries: &[PlannedSupplier],
    pending: &[PlannedPending],
    existing: &[SupplierRecord],
    proxy_provider_icons: &HashMap<String, (Option<String>, Option<String>)>,
) -> bool {
    if entries.is_empty() && pending.is_empty() {
        return false;
    }
    let by_id: HashMap<&str, &SupplierRecord> = existing
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    let entries_applied = entries.iter().all(|entry| {
        let Some(record) = by_id.get(entry.supplier_id.as_str()) else {
            return false;
        };
        if !import_owns(record) {
            return false;
        }
        if record.name != entry.name
            || record.base_url != entry.supplier_base_url
            || record.api_key != entry.api_key
            || record.icon.as_deref() != entry.icon.as_deref()
            || record.icon_color.as_deref() != entry.icon_color.as_deref()
        {
            return false;
        }
        let Some(endpoint) = record
            .endpoints
            .iter()
            .find(|endpoint| endpoint.protocol == entry.protocol.supplier_protocol())
        else {
            return false;
        };
        if endpoint.endpoint_url.as_deref().unwrap_or(&record.base_url) != entry.base_url {
            return false;
        }
        if endpoint
            .api_key_override
            .as_deref()
            .unwrap_or(&record.api_key)
            != entry.api_key
        {
            return false;
        }
        if entry.alternatives.iter().any(|planned| {
            !endpoint.alternatives.iter().any(|saved| {
                saved.base_url == planned.base_url
                    && saved.api_key == planned.api_key
                    && saved.extra_headers
                        == planned.headers.iter().cloned().collect::<HashMap<_, _>>()
                    && model_inventory_matches(&saved.models, &planned.models)
            })
        }) {
            return false;
        }
        if endpoint.extra_headers.len() != entry.headers.len() {
            return false;
        }
        for (name, value) in &entry.headers {
            match endpoint.extra_headers.get(name) {
                Some(existing_value) if existing_value == value => {}
                _ => return false,
            }
        }
        if !model_inventory_matches(&endpoint.models, &entry.models) {
            return false;
        }
        // Legacy projection rows are part of what apply writes: one must
        // exist and already carry the planned display icons. A missing
        // projection (or stale icons) means apply would change state.
        if entry.protocol.legacy_provider_type().is_some() {
            let Some(proxy_provider_id) = endpoint.proxy_provider_id.as_deref() else {
                return false;
            };
            match proxy_provider_icons.get(proxy_provider_id) {
                Some((icon, icon_color))
                    if icon.as_deref() == entry.icon.as_deref()
                        && icon_color.as_deref() == entry.icon_color.as_deref() => {}
                _ => return false,
            }
        }
        true
    });
    if !entries_applied {
        return false;
    }
    pending.iter().all(|association| {
        by_id
            .get(association.supplier_id.as_str())
            .is_some_and(|record| {
                record.source == "official_auth_pending"
                    && record.name == association.name
                    && record.base_url == association.base_url
            })
    })
}

/// Import owns unmodified source declarations; edits and discovery retain
/// their own fields and membership even when another source row changes.
pub(super) fn import_owns_model(model: &SupplierModelRecord) -> bool {
    model.source == "cc_switch"
        && model.enabled
        && model.display_name.is_none()
        && model.supports_tools.is_none()
        && model.supports_images.is_none()
        && model.context_window.is_none()
        && model.max_output_tokens.is_none()
        && model.tier_override.is_none()
}

pub(super) fn model_inventory_matches(saved: &[SupplierModelRecord], planned: &[String]) -> bool {
    if !planned
        .iter()
        .all(|id| saved.iter().any(|model| &model.model_id == id))
    {
        return false;
    }
    let actual: Vec<_> = saved
        .iter()
        .filter(|model| import_owns_model(model))
        .map(|model| model.model_id.as_str())
        .collect();
    let expected: Vec<_> = planned
        .iter()
        .filter(|id| {
            saved
                .iter()
                .any(|model| &model.model_id == *id && import_owns_model(model))
        })
        .map(String::as_str)
        .collect();
    actual == expected
}

pub(super) fn to_preview(
    derived: DerivedPlan,
    plan_id: String,
    generation: u64,
    created_at: i64,
) -> SupplierImportPreview {
    let DerivedPlan {
        entries,
        pending,
        problems,
        source_fingerprint,
        total_rows,
        excluded_oauth_count,
    } = derived;
    let plans = entries
        .into_iter()
        .map(|entry| {
            let mut complete_models = entry.models.clone();
            for alternative in &entry.alternatives {
                for model in &alternative.models {
                    if !complete_models.contains(model) {
                        complete_models.push(model.clone());
                    }
                }
            }
            let model_ids: Vec<String> = complete_models
                .iter()
                .take(MAX_PREVIEW_MODELS)
                .cloned()
                .collect();
            let mut header_names: Vec<String> =
                entry.headers.iter().map(|(name, _)| name.clone()).collect();
            header_names.sort_by_key(|name| name.to_ascii_lowercase());
            SupplierImportPlanEntry {
                supplier_id: entry.supplier_id,
                source_app_type: entry.source_app_type,
                source_id: entry.source_id,
                name: entry.name,
                protocol: entry.protocol.supplier_protocol().to_string(),
                base_url: display_url(&entry.base_url),
                api_key_configured: !entry.api_key.is_empty(),
                header_names,
                model_ids,
                model_count: complete_models.len(),
                // `change` and `source_endpoint_urls` are filled by
                // `annotate_preview` against live state.
                change: "create".to_string(),
                legacy_projection_planned: entry.protocol.legacy_provider_type().is_some(),
                source_endpoint_urls: entry
                    .alternatives
                    .iter()
                    .filter_map(|alternative| display_url(&alternative.base_url))
                    .collect(),
            }
        })
        .collect();
    SupplierImportPreview {
        plan_id,
        source_fingerprint,
        target_generation: generation,
        created_at,
        source_label: "cc-switch".to_string(),
        total_rows,
        plans,
        pending_unsupported: pending
            .into_iter()
            .map(|entry| SupplierImportPendingEntry {
                supplier_id: entry.supplier_id,
                source_app_type: entry.source_app_type,
                source_id: entry.source_id,
                name: entry.name,
                reason: entry.reason,
            })
            .collect(),
        problems,
        excluded_oauth_count,
    }
}

/// Redacted display URL: credentials, query, and fragment removed. A URL that
/// needs no redaction keeps its exact input serialization so previews
/// round-trip byte-identical against stored values.
pub(crate) fn display_url(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let mut url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    if url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
    {
        return Some(value.to_string());
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

fn validate_optional_url(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("base_url_invalid");
    }
    if value.len() > MAX_URL_BYTES || value.chars().any(char::is_control) {
        return Err("base_url_invalid");
    }
    let url = reqwest::Url::parse(value).map_err(|_| "base_url_invalid")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("base_url_invalid");
    }
    Ok(())
}

fn validate_header(name: &str, value: &str) -> Result<(), &'static str> {
    let reserved = [
        "authorization",
        "x-api-key",
        "api-key",
        "host",
        "content-type",
        "content-length",
        "anthropic-version",
    ];
    if name.is_empty()
        || name.len() > 128
        || !name.bytes().all(is_header_token_byte)
        || reserved.iter().any(|item| name.eq_ignore_ascii_case(item))
    {
        return Err("header_reserved_or_invalid");
    }
    if value.len() > MAX_HEADER_VALUE_BYTES
        || value
            .chars()
            .any(|character| character.is_control() && character != '\t')
    {
        return Err("header_value_invalid");
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

/// Fill `change` and `source_endpoint_urls` on a preview from live state.
/// Kept separate so [`derive_plan`] stays a pure mapping.
pub(crate) fn annotate_preview(
    preview: &mut SupplierImportPreview,
    rows: &[CcSwitchRow],
    existing: &[SupplierRecord],
) {
    let existing_by_id: HashMap<&str, &SupplierRecord> = existing
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    for entry in &mut preview.plans {
        entry.change = match existing_by_id.get(entry.supplier_id.as_str()) {
            None => "create".to_string(),
            Some(record) => {
                if record.source == "cc_switch"
                    && record.name == entry.name
                    && Some(record.base_url.as_str()) == entry.base_url.as_deref()
                    && record
                        .endpoints
                        .iter()
                        .any(|endpoint| endpoint.protocol == entry.protocol)
                {
                    "unchanged".to_string()
                } else {
                    "refresh".to_string()
                }
            }
        };
        let recorded_urls: Vec<String> = rows
            .iter()
            .find(|row| row.app_type == entry.source_app_type && row.id == entry.source_id)
            .map(|row| {
                row.endpoint_urls
                    .iter()
                    .filter_map(|url| display_url(url))
                    .collect()
            })
            .unwrap_or_default();
        for url in recorded_urls {
            if !entry.source_endpoint_urls.contains(&url) {
                entry.source_endpoint_urls.push(url);
            }
        }
    }
}
