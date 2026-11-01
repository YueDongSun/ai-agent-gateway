//! Network preparation precedes the existing synchronous registration transaction.
use super::{
    preview::{self, DerivedPlan, PlannedAlternative, PlannedSupplier},
    supplier_import,
};
use crate::{
    database::{
        dao::{
            provider_admission::provider_configuration_identity,
            proxy_providers::ProxyProvider,
            suppliers::{self as supplier_dao, SupplierRecord},
        },
        Database,
    },
    error::AppError,
    services::provider_admission,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreeningImportOutcome {
    pub provider_id: String,
    pub source_record_ref: String,
    pub model: String,
    pub eligible: bool,
    pub stage: String,
    pub reason: String,
    pub latency_ms: Option<u64>,
    pub checked_at: i64,
}

fn candidate(
    db: &Database,
    entry: &PlannedSupplier,
    alternative: Option<&PlannedAlternative>,
    existing: &[SupplierRecord],
) -> Result<Option<(ProxyProvider, String)>, AppError> {
    let Some(provider_type) = entry.protocol.legacy_provider_type() else {
        return Ok(None);
    };
    let protocol = entry.protocol.supplier_protocol();
    let old_supplier = existing.iter().find(|saved| saved.id == entry.supplier_id);
    let old_endpoint = old_supplier.and_then(|saved| {
        saved
            .endpoints
            .iter()
            .find(|endpoint| endpoint.protocol == protocol)
    });
    let (endpoint_id, prior_id, base_url, api_key, headers, models, app_type, source_id, enabled) =
        match alternative {
            None => (
                supplier_dao::endpoint_id_for(&entry.supplier_id, protocol),
                old_endpoint.and_then(|endpoint| endpoint.proxy_provider_id.clone()),
                &entry.base_url,
                &entry.api_key,
                &entry.headers,
                &entry.models,
                &entry.source_app_type,
                &entry.source_id,
                old_endpoint.is_none_or(|endpoint| endpoint.enabled),
            ),
            Some(alternative) => {
                let computed = supplier_dao::endpoint_id_for(
                    &entry.supplier_id,
                    &format!(
                        "{}:{}:{}:{}",
                        protocol,
                        alternative.source_app_type,
                        alternative.source_id,
                        alternative.base_url
                    ),
                );
                let saved = old_endpoint.and_then(|endpoint| {
                    endpoint.alternatives.iter().find(|saved| {
                        saved.id == computed
                            || (saved.base_url == alternative.base_url
                                && saved.api_key == alternative.api_key
                                && saved.extra_headers
                                    == alternative.headers.iter().cloned().collect())
                    })
                });
                (
                    saved.map(|saved| saved.id.clone()).unwrap_or(computed),
                    saved.and_then(|saved| saved.legacy_proxy_provider_id.clone()),
                    &alternative.base_url,
                    &alternative.api_key,
                    &alternative.headers,
                    &alternative.models,
                    &alternative.source_app_type,
                    &alternative.source_id,
                    saved.is_none_or(|saved| saved.enabled),
                )
            }
        };
    let id = prior_id.unwrap_or_else(|| supplier_import::projection_id_for(&endpoint_id));
    let mut provider = match db.get_proxy_provider(&id)? {
        Some(provider) => provider,
        None => serde_json::from_value(
            serde_json::json!({"id":id,"name":entry.name,"type":provider_type}),
        )?,
    };
    provider.provider_type = provider_type.into();
    provider.base_url = base_url.clone();
    provider.api_key = api_key.clone();
    provider.extra_headers = headers.iter().cloned().collect();
    provider.models = models.clone();
    provider.enabled = enabled && old_supplier.is_none_or(|supplier| supplier.enabled);
    Ok(Some((
        provider,
        supplier_import::safe_source_ref(app_type, source_id),
    )))
}

fn collect_candidates(
    db: &Database,
    derived: &DerivedPlan,
    existing: &[SupplierRecord],
) -> Result<Vec<(ProxyProvider, String)>, String> {
    let mut candidates = Vec::new();
    for entry in &derived.entries {
        for alternative in std::iter::once(None).chain(entry.alternatives.iter().map(Some)) {
            if let Some(candidate) = candidate(db, entry, alternative, existing)
                .map_err(|_| "supplier_import_candidate_failed")?
            {
                candidates.push(candidate);
            }
        }
    }
    Ok(candidates)
}

/// Coalesce actual targets before any source/cache mutation or network work.
/// Different CC references may resolve to one previously materialized variant.
pub(super) fn coalesce_candidates(
    candidates: &[(ProxyProvider, String)],
    local_subscription_free_sources: &[String],
) -> Result<Vec<(ProxyProvider, String)>, String> {
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut merged: Vec<(ProxyProvider, String)> = Vec::new();
    for (provider, source_ref) in candidates {
        if let Some(&index) = positions.get(&provider.id) {
            let (prior, selected_ref) = &mut merged[index];
            if provider_configuration_identity(prior) != provider_configuration_identity(provider) {
                return Err("supplier_import_candidate_conflict".into());
            }
            for model in &provider.models {
                if !prior.models.contains(model) {
                    prior.models.push(model.clone());
                }
            }
            // An explicit exemption belongs to its exact CC reference. Select
            // that reference only after proving the actual configuration equal.
            if !local_subscription_free_sources.contains(selected_ref)
                && local_subscription_free_sources.contains(source_ref)
            {
                *selected_ref = source_ref.clone();
            }
        } else {
            positions.insert(provider.id.clone(), merged.len());
            let mut provider = provider.clone();
            let mut seen = HashSet::new();
            provider.models.retain(|model| seen.insert(model.clone()));
            merged.push((provider, source_ref.clone()));
        }
    }
    Ok(merged)
}

/// This is an explicit import action, not a startup probe or preview side effect.
/// A matching known-inactive observation is retained without any status request.
pub async fn screen_import(
    db: &Database,
    source: &Path,
    plan_id: &str,
    local_subscription_free_sources: &[String],
) -> Result<Vec<ScreeningImportOutcome>, String> {
    let preview =
        preview::build_preview(db, source).map_err(|_| "supplier_import_preview_failed")?;
    if preview.plan_id != plan_id {
        return Err("supplier_import_stale_preview".into());
    }
    let rows =
        preview::read_source_rows(source).map_err(|_| "supplier_import_source_unavailable")?;
    let (_, existing) = db
        .get_supplier_snapshot()
        .map_err(|_| "supplier_import_snapshot_failed")?;
    let derived =
        preview::derive_plan(&rows, &existing, db).map_err(|_| "supplier_import_plan_failed")?;
    let raw_candidates = collect_candidates(db, &derived, &existing)?;
    if local_subscription_free_sources.iter().any(|source_ref| {
        !raw_candidates
            .iter()
            .any(|(_, candidate_ref)| candidate_ref == source_ref)
    }) {
        return Err("supplier_import_subscription_source_unknown".into());
    }
    let candidates = coalesce_candidates(&raw_candidates, local_subscription_free_sources)?;
    // Validate every asserted reference, including discarded duplicates, before
    // binding anything. Validation must not overwrite a shared provider's cache.
    for (provider, source_ref) in &raw_candidates {
        if local_subscription_free_sources.contains(source_ref) {
            provider_admission::validate_local_source(provider, source, source_ref)?;
        }
    }
    for (provider, source_ref) in &candidates {
        provider_admission::bind_source(
            db,
            provider,
            source,
            source_ref,
            local_subscription_free_sources.contains(source_ref),
        )?;
    }
    db.set_provider_admission_enforced(true)
        .map_err(|_| "provider_admission_unavailable")?;
    let mut outcomes = Vec::new();
    let mut passed_candidates = Vec::new();
    for (provider, source_ref) in candidates {
        let model = provider.models.first().cloned().unwrap_or_default();
        let report = provider_admission::screen_bound_provider(
            db,
            &provider,
            &model,
            source,
            &source_ref,
            local_subscription_free_sources.contains(&source_ref),
            false,
        )
        .await?;
        if report.eligible {
            passed_candidates.push(provider.clone());
        }
        outcomes.push(ScreeningImportOutcome {
            provider_id: provider.id,
            source_record_ref: source_ref,
            model,
            eligible: report.eligible,
            stage: serde_json::to_value(report.stage)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            reason: serde_json::to_value(report.reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            latency_ms: report.latency_ms,
            checked_at: report.checked_at,
        });
    }
    // Long batches may outlive the first status observation. Refresh only stale
    // positive evidence, without repeating its successful inference probe.
    for provider in &passed_candidates {
        let refreshed = provider_admission::refresh_subscription(db, provider)
            .await
            .is_ok();
        if !refreshed || provider_admission::admitted_models(db, provider).is_empty() {
            if let Some(outcome) = outcomes
                .iter_mut()
                .find(|outcome| outcome.provider_id == provider.id)
            {
                outcome.eligible = false;
                outcome.stage = "subscription".into();
                outcome.reason = "subscription_evidence_expired_or_changed".into();
            }
        }
    }
    // No early success receipt outlives a slow final status-refresh pass.
    for provider in &passed_candidates {
        if provider_admission::admitted_models(db, provider).is_empty() {
            if let Some(outcome) = outcomes
                .iter_mut()
                .find(|outcome| outcome.provider_id == provider.id)
            {
                outcome.eligible = false;
                outcome.stage = "subscription".into();
                outcome.reason = "subscription_evidence_expired_or_changed".into();
            }
        }
    }
    if preview::source_fingerprint(
        &preview::read_source_rows(source).map_err(|_| "supplier_import_source_unavailable")?,
    ) != derived.source_fingerprint
    {
        return Err("supplier_import_source_changed".into());
    }
    Ok(outcomes)
}

pub(super) fn filter_admitted(
    db: &Database,
    derived: &mut DerivedPlan,
    existing: &[SupplierRecord],
) -> Result<(), AppError> {
    if !provider_admission::enforcement_enabled(db) {
        return Ok(());
    }
    coalesce_candidates(
        &collect_candidates(db, derived, existing).map_err(AppError::InvalidInput)?,
        &[],
    )
    .map_err(AppError::InvalidInput)?;
    for entry in &mut derived.entries {
        let primary = candidate(db, entry, None, existing)?;
        let primary_id = primary.as_ref().map(|(provider, _)| provider.id.as_str());
        let admitted = primary
            .as_ref()
            .map(|(provider, _)| provider_admission::admitted_models(db, provider))
            .unwrap_or_default();
        // Build alternative identities before changing the primary model inventory.
        let alternative_models: Vec<_> = entry
            .alternatives
            .iter()
            .map(|alternative| {
                candidate(db, entry, Some(alternative), existing).map(|value| {
                    value
                        .map(|(provider, _)| {
                            let admitted = provider_admission::admitted_models(db, &provider);
                            (Some(provider.id), admitted)
                        })
                        .unwrap_or_default()
                })
            })
            .collect::<Result<_, _>>()?;
        entry.models.retain(|model| admitted.contains(model));
        coalesce_admitted_alternatives(entry, primary_id, alternative_models);
    }
    derived
        .entries
        .retain(|entry| !entry.models.is_empty() || !entry.alternatives.is_empty());
    // Account-backed rows cannot become usable static registrations.
    derived.pending.clear();
    Ok(())
}

/// The apply transaction must see one current declaration per actual target,
/// otherwise a later duplicate can delete models declared by an earlier one.
pub(super) fn coalesce_admitted_alternatives(
    entry: &mut PlannedSupplier,
    primary_id: Option<&str>,
    admitted_alternatives: Vec<(Option<String>, Vec<String>)>,
) {
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut alternatives: Vec<PlannedAlternative> = Vec::new();
    for (mut alternative, (provider_id, admitted)) in std::mem::take(&mut entry.alternatives)
        .into_iter()
        .zip(admitted_alternatives)
    {
        let Some(provider_id) = provider_id else {
            continue;
        };
        alternative.models.retain(|model| admitted.contains(model));
        if alternative.models.is_empty() {
            continue;
        }
        let models = if primary_id == Some(provider_id.as_str()) {
            &mut entry.models
        } else if let Some(&index) = positions.get(&provider_id) {
            &mut alternatives[index].models
        } else {
            positions.insert(provider_id, alternatives.len());
            alternatives.push(alternative);
            continue;
        };
        for model in alternative.models {
            if !models.contains(&model) {
                models.push(model);
            }
        }
    }
    entry.alternatives = alternatives;
}

pub(crate) fn candidate_by_id(
    db: &Database,
    source: &Path,
    provider_id: &str,
) -> Result<ProxyProvider, String> {
    let rows =
        preview::read_source_rows(source).map_err(|_| "supplier_import_source_unavailable")?;
    let (_, existing) = db
        .get_supplier_snapshot()
        .map_err(|_| "supplier_import_snapshot_failed")?;
    let derived =
        preview::derive_plan(&rows, &existing, db).map_err(|_| "supplier_import_plan_failed")?;
    let candidates = coalesce_candidates(&collect_candidates(db, &derived, &existing)?, &[])?;
    for (provider, _) in candidates {
        if provider.id == provider_id {
            return Ok(provider);
        }
    }
    Err("provider_subscription_source_missing".into())
}
