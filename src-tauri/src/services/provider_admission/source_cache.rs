//! Private CC Switch source bindings and status-only freshness updates.
use super::{admission_evidence, policy_revision};
use crate::{
    database::{dao::proxy_providers::ProxyProvider, Database},
    services::{
        provider_screening::{ScreeningConfig, SubscriptionEvidence, SubscriptionState},
        subscription_source::{self, SourcePlan, SourceReason, SubscriptionObservation},
    },
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stored_source {
    #[serde(default)]
    display_name: String,
    source_path: PathBuf,
    source_record_ref: String,
    source_binding: String,
    local_subscription_free: bool,
    observation: Option<SubscriptionObservation>,
}

fn read(
    db: &Database,
    provider: &ProxyProvider,
) -> Result<Option<(String, Stored_source)>, String> {
    let raw = db
        .provider_subscription_value(&provider.id)
        .map_err(|_| "provider_subscription_unavailable")?;
    raw.map(|raw| {
        serde_json::from_str(&raw)
            .map(|value| (raw, value))
            .map_err(|_| "provider_subscription_invalid".into())
    })
    .transpose()
}

fn write(
    db: &Database,
    provider: &ProxyProvider,
    expected: Option<&str>,
    source: &Stored_source,
) -> Result<(), String> {
    let raw = serde_json::to_string(source).map_err(|_| "provider_subscription_invalid")?;
    if !db
        .compare_exchange_provider_subscription(&provider.id, expected, &raw)
        .map_err(|_| "provider_subscription_unavailable")?
    {
        return Err("provider_subscription_conflict".into());
    }
    Ok(())
}

fn local_endpoint(provider: &ProxyProvider) -> bool {
    reqwest::Url::parse(&provider.base_url)
        .ok()
        .is_some_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.host_str().is_some_and(|host| {
                    host.eq_ignore_ascii_case("localhost")
                        || host
                            .trim_matches(['[', ']'])
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|address| address.is_loopback())
                })
        })
}

fn validate_local_plan(provider: &ProxyProvider, plan: &SourcePlan) -> Result<(), String> {
    if !local_endpoint(provider)
        || matches!(
            plan.view().reason,
            SourceReason::SourceBindingMismatch
                | SourceReason::SourceAccountUnsupported
                | SourceReason::InvalidSourceMetadata
        )
    {
        return Err("provider_subscription_local_binding_invalid".into());
    }
    Ok(())
}

/// Validate an exact local assertion without writing a source/cache record.
pub(crate) fn validate_local_source(
    provider: &ProxyProvider,
    path: &Path,
    source_ref: &str,
) -> Result<(), String> {
    let plan = subscription_source::prepare_cc_switch_source(path, source_ref, provider)?;
    validate_local_plan(provider, &plan)
}

/// Explicit owner assertion, never inferred from a name, key or enabled flag.
pub(crate) fn bind_source(
    db: &Database,
    provider: &ProxyProvider,
    path: &Path,
    source_ref: &str,
    local_subscription_free: bool,
) -> Result<SourcePlan, String> {
    let plan = subscription_source::prepare_cc_switch_source(path, source_ref, provider)?;
    if local_subscription_free {
        validate_local_plan(provider, &plan)?;
    }
    let prior = read(db, provider)?;
    // The explicit assertion survives an unchanged reimport. Changing any CC
    // source or connection field invalidates it and requires a new assertion.
    let local_subscription_free = local_subscription_free
        || prior.as_ref().is_some_and(|(_, old)| {
            old.local_subscription_free && old.source_binding == plan.binding
        });
    let observation = prior
        .as_ref()
        .filter(|(_, old)| {
            old.source_binding == plan.binding
                && old.local_subscription_free == local_subscription_free
        })
        .and_then(|(_, old)| old.observation.clone());
    let mut display_name: String = provider.name.chars().take(128).collect();
    for secret in std::iter::once(provider.api_key.as_str())
        .chain(provider.extra_headers.values().map(String::as_str))
        .filter(|value| !value.is_empty())
    {
        display_name = display_name.replace(secret, "[redacted]");
    }
    let source = Stored_source {
        display_name,
        source_path: path.to_path_buf(),
        source_record_ref: source_ref.into(),
        source_binding: plan.binding.clone(),
        local_subscription_free,
        observation,
    };
    write(
        db,
        provider,
        prior.as_ref().map(|(raw, _)| raw.as_str()),
        &source,
    )?;
    Ok(plan)
}

pub(crate) fn review_context(
    db: &Database,
    provider_id: &str,
) -> Option<(
    String,
    PathBuf,
    String,
    bool,
    Option<SubscriptionObservation>,
)> {
    let raw = db.provider_subscription_value(provider_id).ok()??;
    let source: Stored_source = serde_json::from_str(&raw).ok()?;
    Some((
        source.display_name,
        source.source_path,
        source.source_record_ref,
        source.local_subscription_free,
        source.observation,
    ))
}

pub(crate) fn source_for_provider(
    db: &Database,
    provider: &ProxyProvider,
) -> Result<(PathBuf, String, bool), String> {
    if let Some((_, source)) = read(db, provider)? {
        return Ok((
            source.source_path,
            source.source_record_ref,
            source.local_subscription_free,
        ));
    }
    let owner = db
        .supplier_discovery_owner(&provider.id)
        .map_err(|_| "provider_subscription_unavailable")?;
    if let Some((supplier_id, _)) = owner {
        let (_, supplier) = db
            .get_supplier_record_snapshot(&supplier_id)
            .map_err(|_| "provider_subscription_unavailable")?;
        let supplier = supplier.ok_or("provider_subscription_unavailable")?;
        for endpoint in supplier.endpoints {
            let provenance = if endpoint.proxy_provider_id.as_deref() == Some(provider.id.as_str())
            {
                Some(&endpoint.provenance)
            } else {
                endpoint
                    .alternatives
                    .iter()
                    .find(|alternative| {
                        alternative.legacy_proxy_provider_id.as_deref()
                            == Some(provider.id.as_str())
                    })
                    .map(|alternative| &alternative.provenance)
            };
            if let Some(source_ref) = provenance
                .and_then(|value| value.get("sourceRecordRef"))
                .and_then(serde_json::Value::as_str)
            {
                return Ok((
                    crate::services::import_cc_switch::cc_switch_db_path(),
                    source_ref.into(),
                    false,
                ));
            }
        }
    }
    Err("provider_subscription_source_missing".into())
}

fn checked_plan(provider: &ProxyProvider, source: &Stored_source) -> Result<SourcePlan, String> {
    let plan = subscription_source::prepare_cc_switch_source(
        &source.source_path,
        &source.source_record_ref,
        provider,
    )?;
    if plan.binding != source.source_binding
        || matches!(
            plan.view().reason,
            SourceReason::SourceBindingMismatch
                | SourceReason::SourceAccountUnsupported
                | SourceReason::InvalidSourceMetadata
        )
    {
        return Err("provider_subscription_source_changed".into());
    }
    if source.local_subscription_free && !local_endpoint(provider) {
        return Err("provider_subscription_source_changed".into());
    }
    Ok(plan)
}

/// Resolve credentials only from this provider's exact current source. The
/// static API-key default belongs solely to providers with no source binding;
/// malformed, missing or changed known sources fail before catalog/inference I/O.
pub(super) fn validated_auth_scheme(
    db: &Database,
    provider: &ProxyProvider,
) -> Result<crate::proxy::providers::AnthropicAuthScheme, String> {
    Ok(current_plan(db, provider)?
        .map(|plan| plan.auth_scheme())
        .unwrap_or(crate::proxy::providers::AnthropicAuthScheme::ApiKey))
}

/// Connection identity is available before subscription observations exist.
/// This local read grants no eligibility and never queries status or inference.
pub(super) fn current_plan(
    db: &Database,
    provider: &ProxyProvider,
) -> Result<Option<SourcePlan>, String> {
    if let Some((_, source)) = read(db, provider)? {
        return checked_plan(provider, &source).map(Some);
    }
    let (path, source_ref, _) = match source_for_provider(db, provider) {
        Ok(source) => source,
        Err(error) if error == "provider_subscription_source_missing" => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let plan = subscription_source::prepare_cc_switch_source(&path, &source_ref, provider)?;
    if matches!(
        plan.view().reason,
        SourceReason::SourceBindingMismatch
            | SourceReason::SourceAccountUnsupported
            | SourceReason::InvalidSourceMetadata
    ) {
        return Err("provider_subscription_source_changed".into());
    }
    Ok(Some(plan))
}

pub(crate) fn current_observation(
    db: &Database,
    provider: &ProxyProvider,
) -> Result<SubscriptionObservation, String> {
    let (_, source) = read(db, provider)?.ok_or("provider_subscription_missing")?;
    let plan = checked_plan(provider, &source)?;
    let observation = source
        .observation
        .ok_or_else(|| "provider_subscription_missing".to_string())?;
    if observation.source_binding != plan.binding {
        return Err("provider_subscription_source_changed".into());
    }
    Ok(observation)
}

pub(crate) async fn query_bound(
    db: &Database,
    provider: &ProxyProvider,
    explicit_recheck: bool,
) -> Result<SubscriptionObservation, String> {
    let (raw, mut source) = read(db, provider)?.ok_or("provider_subscription_missing")?;
    let plan = checked_plan(provider, &source)?;
    if !explicit_recheck {
        if let Some(observation) = source
            .observation
            .as_ref()
            .filter(|observation| observation.evidence.state == SubscriptionState::Inactive)
        {
            return Ok(observation.clone());
        }
    }
    let observation = if source.local_subscription_free {
        SubscriptionObservation {
            evidence: SubscriptionEvidence {
                state: SubscriptionState::NotRequired,
                provider_ref: provider.id.clone(),
                evidence_id: format!("cc_switch:{}", plan.binding),
                provenance: "cc_switch_explicit_local_subscription_free".into(),
                valid_until: None,
            },
            probe_allowed: true,
            reason: SourceReason::VerifiedAllowance,
            source_binding: plan.binding.clone(),
            source_record_ref: plan.source_record_ref.clone(),
            checked_at: chrono::Utc::now().timestamp(),
            status_http: None,
        }
    } else {
        subscription_source::query_subscription(&plan).await
    };
    if !subscription_source::validate_current_binding(&plan, provider)? {
        return Err("provider_subscription_source_changed".into());
    }
    source.observation = Some(observation.clone());
    write(db, provider, Some(&raw), &source)?;
    Ok(observation)
}

pub(crate) async fn refresh_if_stale(
    db: &Database,
    provider: &ProxyProvider,
) -> Result<(), String> {
    let previous = current_observation(db, provider)?;
    if previous.evidence.state != SubscriptionState::Active
        || !previous.probe_allowed
        || previous
            .evidence
            .valid_until
            .is_none_or(|until| chrono::Utc::now().timestamp() < until)
    {
        return Ok(());
    }
    let policy = policy_revision(&ScreeningConfig::load(db)?);
    let record = db
        .provider_admission_record(&provider.id)
        .map_err(|_| "provider_admission_unavailable")?
        .ok_or("provider_admission_missing")?;
    if !record.probe.as_ref().is_some_and(|probe| probe.eligible) {
        return Ok(());
    }
    let observation = match query_bound(db, provider, false).await {
        Ok(observation) => observation,
        Err(error) if error == "provider_subscription_conflict" => return Ok(()), // winner remains authoritative
        Err(error) => return Err(error),
    };
    let evidence = admission_evidence(&observation.evidence);
    if observation.probe_allowed && observation.evidence.state == SubscriptionState::Active {
        db.provider_admission_refresh_evidence(
            provider,
            &record.evidence,
            &evidence,
            &policy,
            record.version,
        )
        .map_err(|_| "provider_admission_conflict".to_string())?;
    } else {
        db.provider_admission_begin(provider, &evidence, &policy, record.version)
            .map_err(|_| "provider_admission_conflict".to_string())?;
    }
    Ok(())
}
