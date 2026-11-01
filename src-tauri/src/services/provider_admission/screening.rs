use super::{
    admission_evidence, bind_source, current_observation, policy_revision, source_cache,
    source_for_provider,
};
use crate::{
    database::{
        dao::{provider_admission::Admission_probe, proxy_providers::ProxyProvider},
        Database,
    },
    services::{
        provider_screening::{
            self, ScreeningConfig, ScreeningReason, ScreeningReport, ScreeningStage,
            SubscriptionEvidence, SubscriptionState,
        },
        subscription_source,
    },
};
use std::path::Path;

/// Tracks successful writes made by this check, even when a later stage fails.
/// Concurrent activity is reported separately by the current overview.
#[derive(Default)]
pub(crate) struct Screening_commits {
    pub source_committed: bool,
    pub admission_committed: bool,
    pub admission_version: Option<u64>,
}

fn reason_code(reason: ScreeningReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "provider_error".into())
}

fn without_probe(evidence: &SubscriptionEvidence, reason: ScreeningReason) -> ScreeningReport {
    ScreeningReport {
        eligible: false,
        stage: ScreeningStage::Subscription,
        reason,
        checked_at: chrono::Utc::now().timestamp(),
        evidence_identity: evidence.evidence_id.clone(),
        evidence_provenance: evidence.provenance.clone(),
        latency_ms: None,
        response: None,
        response_truncated: false,
        response_redacted: false,
        upstream_status: None,
        upstream_code: None,
    }
}

fn stored_probe(model: &str, report: &ScreeningReport) -> Admission_probe {
    Admission_probe {
        probe_model: if report.latency_ms.is_some() {
            model.into()
        } else {
            String::new()
        },
        checked_at: report.checked_at,
        eligible: report.eligible,
        reason: reason_code(report.reason),
        latency_ms: report.latency_ms,
        response: report.response.clone(),
        response_truncated: report.response_truncated,
        response_redacted: report.response_redacted,
        upstream_status: report.upstream_status,
        upstream_code: report.upstream_code.clone(),
    }
}

fn registered_revision(
    db: &Database,
    provider: &ProxyProvider,
) -> Result<Option<(String, bool)>, String> {
    db.get_proxy_provider(&provider.id)
        .map(|saved| {
            saved.map(|saved| {
                (
                    crate::database::dao::provider_admission::provider_configuration_identity(
                        &saved,
                    ),
                    saved.enabled,
                )
            })
        })
        .map_err(|_| "provider_admission_unavailable".into())
}

/// Real status-first screening of one exact connection. Models are only offered
/// inventory; `model` identifies the single actual inference probe.
pub async fn screen_bound_provider(
    db: &Database,
    provider: &ProxyProvider,
    model: &str,
    source_path: &Path,
    source_ref: &str,
    local_subscription_free: bool,
    explicit_recheck: bool,
) -> Result<ScreeningReport, String> {
    screen_bound_provider_tracked(
        db, provider, model, source_path, source_ref, local_subscription_free,
        explicit_recheck, &mut Screening_commits::default(),
    ).await
}

pub(crate) async fn screen_bound_provider_tracked(
    db: &Database,
    provider: &ProxyProvider,
    model: &str,
    source_path: &Path,
    source_ref: &str,
    local_subscription_free: bool,
    explicit_recheck: bool,
    commits: &mut Screening_commits,
) -> Result<ScreeningReport, String> {
    if !model.is_empty() && !provider.models.iter().any(|configured| configured == model) {
        return Err("provider_screening_model_unconfigured".into());
    }
    let registered_before = registered_revision(db, provider)?;
    let config = ScreeningConfig::load(db)?;
    let policy = policy_revision(&config);
    let plan = bind_source(
        db,
        provider,
        source_path,
        source_ref,
        local_subscription_free,
    )?;
    commits.source_committed = true;
    let prior_version = db
        .provider_admission_record(&provider.id)
        .map_err(|_| "provider_admission_unavailable")?
        .map(|record| record.version)
        .unwrap_or(0);
    let pending = SubscriptionEvidence {
        state: SubscriptionState::Unknown,
        provider_ref: provider.id.clone(),
        evidence_id: format!("cc_switch:{}", plan.binding),
        provenance: "cc_switch_native_status".into(),
        valid_until: None,
    };
    // Revoke the previous admission before any potentially slow status request.
    let pending_version = db
        .provider_admission_begin(
            provider,
            &admission_evidence(&pending),
            &policy,
            prior_version,
        )
        .map_err(|_| "provider_admission_conflict")?;
    commits.admission_committed = true;
    commits.admission_version = Some(pending_version);
    let observation = source_cache::query_bound(db, provider, explicit_recheck).await?;
    if registered_revision(db, provider)? != registered_before {
        return Err("provider_admission_configuration_changed".into());
    }
    let evidence = admission_evidence(&observation.evidence);
    let version = db
        .provider_admission_begin(provider, &evidence, &policy, pending_version)
        .map_err(|_| "provider_admission_conflict")?;
    commits.admission_version = Some(version);
    let report = if observation.probe_allowed {
        provider_screening::screen_provider_with_auth(provider, model, &observation.evidence, &config, plan.auth_scheme()).await
    } else {
        let reason = match observation.evidence.state {
            SubscriptionState::Inactive => ScreeningReason::SubscriptionInactive,
            SubscriptionState::Active => ScreeningReason::QuotaExceeded,
            _ => ScreeningReason::SubscriptionUnknown,
        };
        without_probe(&observation.evidence, reason)
    };
    let current = current_observation(db, provider)?;
    if registered_revision(db, provider)? != registered_before {
        return Err("provider_admission_configuration_changed".into());
    }
    if !subscription_source::validate_current_binding(&plan, provider)?
        || admission_evidence(&current.evidence) != evidence
        || current.probe_allowed != observation.probe_allowed
    {
        return Err("provider_subscription_source_changed".into());
    }
    let finished_version = db.provider_admission_finish(
        provider,
        &evidence,
        &policy,
        version,
        stored_probe(model, &report),
        if report.eligible {
            provider.models.clone()
        } else {
            Vec::new()
        },
    )
    .map_err(|_| "provider_admission_conflict".to_string())?;
    commits.admission_version = Some(finished_version);
    Ok(report)
}

pub async fn test_provider(
    db: &Database,
    provider: &ProxyProvider,
    model: &str,
) -> Result<ScreeningReport, String> {
    let (source_path, source_ref, local_free) = source_for_provider(db, provider)?;
    if !provider.models.iter().any(|configured| configured == model) {
        return Err("provider_screening_model_unconfigured".into());
    }
    screen_bound_provider(
        db,
        provider,
        model,
        &source_path,
        &source_ref,
        local_free,
        true,
    )
    .await
}
