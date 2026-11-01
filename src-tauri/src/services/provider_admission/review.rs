//! Deliberate read/recheck surface; private configuration never crosses IPC.
use super::{admission_evidence, policy_revision, source_cache, source_for_provider};
use crate::{
    database::{dao::provider_admission::Provider_admission_record, Database},
    services::{provider_screening::ScreeningConfig, subscription_source},
};
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

/// Process-local ownership only. Persistent admission CAS remains authoritative
/// across processes; this guard does not promise exactly-once upstream execution.
#[derive(Default)]
pub struct Review_coordinator {
    active: Mutex<HashSet<String>>,
}

pub(crate) struct Review_permit<'a> {
    coordinator: &'a Review_coordinator,
    provider_id: String,
}

impl Review_coordinator {
    pub(crate) fn try_begin(&self, provider_id: &str) -> Result<Review_permit<'_>, String> {
        let mut active = self.active.lock().map_err(|_| "provider_admission_unavailable")?;
        if !active.insert(provider_id.into()) { return Err("provider_admission_busy".into()); }
        Ok(Review_permit { coordinator: self, provider_id: provider_id.into() })
    }
}

impl Drop for Review_permit<'_> {
    fn drop(&mut self) {
        // No code that can call external owners runs while this lock is held.
        if let Ok(mut active) = self.coordinator.active.lock() {
            active.remove(&self.provider_id);
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Screening_overview {
    pub enforced: bool,
    pub policy: ScreeningConfig,
    pub entries: Vec<Screening_entry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Screening_entry {
    pub provider_id: String,
    pub name: String,
    pub admitted_models: Vec<String>,
    pub subscription_state: String,
    pub subscription_reason: String,
    pub stage: String,
    pub reason: String,
    pub probe_model: Option<String>,
    pub checked_at: Option<i64>,
    pub latency_ms: Option<u64>,
    pub response: Option<String>,
    pub response_truncated: bool,
    pub response_redacted: bool,
}

fn code<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

fn entry(
    db: &Database,
    provider_id: &str,
    record: Option<&Provider_admission_record>,
    policy: &str,
) -> Screening_entry {
    let provider = db.get_proxy_provider(provider_id).ok().flatten();
    let source = source_cache::review_context(db, provider_id);
    let inspection_provider = provider.clone().or_else(|| {
        source.as_ref().and_then(|(_, path, _, _, _)| {
            crate::services::import_cc_switch::candidate_by_id(db, path, provider_id).ok()
        })
    });
    let observation = inspection_provider
        .as_ref()
        .and_then(|provider| source_cache::current_observation(db, provider).ok());
    let probe = record.and_then(|record| record.probe.as_ref());
    let admitted = provider
        .as_ref()
        .zip(observation.as_ref())
        .filter(|(_, observation)| observation.probe_allowed)
        .and_then(|(provider, observation)| {
            db.accepted_provider_models(
                provider,
                &admission_evidence(&observation.evidence),
                policy,
                chrono::Utc::now().timestamp(),
            )
            .ok()
        })
        .unwrap_or_default().into_iter().filter(|model| provider.as_ref().is_some_and(|provider| provider.models.contains(model))).collect::<Vec<_>>();
    let mut reason = probe
        .map(|probe| probe.reason.clone())
        .unwrap_or_else(|| "not_screened".into());
    if probe.is_some_and(|probe| probe.eligible) && admitted.is_empty() {
        reason = if provider.is_none() {
            "awaiting_registration"
        } else if record.is_some_and(|record| record.policy_revision != policy) {
            "policy_changed"
        } else {
            "subscription_evidence_expired_or_changed"
        }
        .into();
    }
    let stage = if reason == "accepted" || reason == "latency_exceeded" {
        "admission"
    } else if reason.starts_with("subscription_")
        || reason == "quota_exceeded"
        || reason == "not_screened"
    {
        "subscription"
    } else {
        "probe"
    };
    Screening_entry {
        provider_id: provider_id.into(),
        name: provider
            .as_ref()
            .map(|provider| provider.name.clone())
            .or_else(|| source.as_ref().map(|source| source.0.clone()))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| provider_id.into()),
        admitted_models: if record.is_some() {
            admitted
        } else {
            Vec::new()
        },
        subscription_state: observation
            .as_ref()
            .map(|observation| code(observation.evidence.state))
            .unwrap_or_else(|| "unknown".into()),
        subscription_reason: observation
            .as_ref()
            .map(|observation| code(observation.reason))
            .unwrap_or_else(|| "missing_evidence".into()),
        stage: stage.into(),
        reason,
        probe_model: probe
            .map(|probe| probe.probe_model.clone())
            .filter(|model| !model.is_empty()),
        checked_at: probe.map(|probe| probe.checked_at),
        latency_ms: probe.and_then(|probe| probe.latency_ms),
        response: probe.and_then(|probe| probe.response.clone()),
        response_truncated: probe.is_some_and(|probe| probe.response_truncated),
        response_redacted: probe.is_some_and(|probe| probe.response_redacted),
    }
}

pub fn overview(db: &Database) -> Result<Screening_overview, String> {
    let policy = ScreeningConfig::load(db)?;
    let records = db
        .list_provider_admission_records()
        .map_err(|_| "provider_admission_unavailable")?;
    let providers = db
        .get_all_proxy_providers()
        .map_err(|_| "provider_admission_unavailable")?;
    let revision = policy_revision(&policy);
    let mut entries: Vec<_> = records
        .iter()
        .map(|record| entry(db, &record.provider_id, Some(record), &revision))
        .collect();
    for provider in providers {
        if !records
            .iter()
            .any(|record| record.provider_id == provider.id)
        {
            entries.push(entry(db, &provider.id, None, &revision));
        }
    }
    entries.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.provider_id.cmp(&right.provider_id))
    });
    Ok(Screening_overview {
        enforced: db.provider_admission_enforced().map_err(|_| "provider_admission_unavailable")?,
        policy,
        entries,
    })
}

struct Recheck_target {
    provider: crate::database::dao::proxy_providers::ProxyProvider,
    model: String,
    source_path: PathBuf,
    source_ref: String,
    local_subscription_free: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recheck_changes {
    pub policy_changed: bool,
    pub registration_changed: bool,
    pub admission_committed: bool,
    pub source_committed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recheck_outcome {
    pub provider_id: String,
    pub check_started: bool,
    pub eligible: Option<bool>,
    pub check_error: Option<String>,
    pub changes: Recheck_changes,
    pub admission_version: Option<u64>,
}

pub(crate) fn rejected_recheck(provider_id: &str, error: &str) -> Recheck_outcome {
    Recheck_outcome {
        provider_id: provider_id.into(), check_started: false, eligible: None,
        check_error: Some(safe_recheck_error(error)),
        changes: Recheck_changes {
            policy_changed: false, registration_changed: false,
            admission_committed: false, source_committed: false,
        },
        admission_version: None,
    }
}

fn prepare_recheck(db: &Database, provider_id: &str, registered_only: bool) -> Result<Recheck_target, String> {
    let existing = db.get_proxy_provider(provider_id)
        .map_err(|_| "provider_admission_unavailable")?;
    let (provider, source_path, source_ref, local_subscription_free) = if let Some(provider) = existing {
        let (path, source_ref, local_free) = source_for_provider(db, &provider)?;
        (provider, path, source_ref, local_free)
    } else {
        if registered_only { return Err("provider_admission_missing".into()); }
        let (_, path, source_ref, local_free, _) = source_cache::review_context(db, provider_id)
            .ok_or("provider_subscription_source_missing")?;
        let provider = crate::services::import_cc_switch::candidate_by_id(db, &path, provider_id)?;
        (provider, path, source_ref, local_free)
    };
    if !provider.enabled { return Err("provider_disabled".into()); }
    let model = db.provider_admission_record(&provider.id)
        .map_err(|_| "provider_admission_unavailable")?
        .and_then(|record| record.probe.map(|probe| probe.probe_model))
        .filter(|model| !model.trim().is_empty() && provider.models.contains(model))
        .or_else(|| provider.models.iter().find(|model| !model.trim().is_empty()).cloned())
        .ok_or("provider_screening_model_unconfigured")?;
    ScreeningConfig::load(db)?;
    let plan = subscription_source::prepare_cc_switch_source(&source_path, &source_ref, &provider)?;
    if local_subscription_free {
        super::validate_local_source(&provider, &source_path, &source_ref)?;
    }
    if matches!(plan.view().reason,
        subscription_source::SourceReason::SourceBindingMismatch
            | subscription_source::SourceReason::SourceAccountUnsupported
            | subscription_source::SourceReason::InvalidSourceMetadata)
    {
        return Err("provider_subscription_source_changed".into());
    }
    Ok(Recheck_target { provider, model, source_path, source_ref, local_subscription_free })
}

/// Targeted operators must never fall through to source-wide registration when
/// a selected row disappears after preflight.
pub async fn recheck_registered(db: &Database, provider_id: &str) -> Result<(), String> {
    let target = prepare_recheck(db, provider_id, true)?;
    super::screen_bound_provider(db, &target.provider, &target.model, &target.source_path,
        &target.source_ref, target.local_subscription_free, true).await?;
    Ok(())
}

pub async fn recheck(db: &Database, provider_id: &str) -> Result<(), String> {
    let target = prepare_recheck(db, provider_id, false)?;
    super::screen_bound_provider(db, &target.provider, &target.model, &target.source_path,
        &target.source_ref, target.local_subscription_free, true).await?;
    Ok(())
}

pub(crate) fn safe_recheck_error(error: &str) -> String {
    match error {
        "provider_admission_unavailable" | "provider_admission_missing" | "provider_admission_busy"
        | "provider_admission_conflict" | "provider_admission_configuration_changed"
        | "provider_subscription_source_missing" | "provider_subscription_source_changed"
        | "provider_subscription_local_binding_invalid" | "provider_subscription_unavailable"
        | "provider_subscription_invalid" | "provider_subscription_missing"
        | "provider_subscription_conflict" | "provider_screening_policy_invalid"
        | "provider_screening_policy_unavailable" | "provider_screening_model_unconfigured"
        | "subscription_source_path_invalid" | "subscription_source_unavailable"
        | "subscription_source_not_found" | "subscription_source_settings_invalid"
        | "subscription_source_metadata_invalid"
        | "provider_disabled" => error.to_string(),
        _ => "provider_screening_failed".into(),
    }
}

/// One selected check owns only its source/admission writes. Registration and
/// global enforcement transitions belong to the explicit import workflow.
pub async fn recheck_outcome(db: &Database, provider_id: &str) -> Recheck_outcome {
    let mut commits = super::screening::Screening_commits::default();
    let mut check_started = false;
    let result = match prepare_recheck(db, provider_id, false) {
        Err(error) => Err(error),
        Ok(target) => {
            check_started = true;
            super::screening::screen_bound_provider_tracked(
                db, &target.provider, &target.model, &target.source_path, &target.source_ref,
                target.local_subscription_free, true, &mut commits,
            ).await
        }
    };
    Recheck_outcome {
        provider_id: provider_id.into(),
        check_started,
        eligible: result.as_ref().ok().map(|report| report.eligible),
        check_error: result.err().map(|error| safe_recheck_error(&error)),
        changes: Recheck_changes {
            policy_changed: false, registration_changed: false,
            admission_committed: commits.admission_committed,
            source_committed: commits.source_committed,
        },
        admission_version: commits.admission_version,
    }
}
