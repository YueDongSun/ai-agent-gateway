//! Admission owner shared by imports, discovery and every upstream dispatch.
//! Source/receipt state is private; the review API returns only bounded probe data.
pub mod review;
mod screening;
mod source_cache;
#[cfg(test)]
mod tests;
pub use screening::{screen_bound_provider, test_provider};
pub(crate) use source_cache::refresh_if_stale as refresh_subscription;
pub(crate) use source_cache::{
    bind_source, current_observation, source_for_provider, validate_local_source,
};

use crate::{
    database::{
        dao::{
            provider_admission::{
                provider_configuration_identity, Admission_evidence, Admission_state,
            },
            proxy_providers::ProxyProvider,
        },
        Database,
    },
    services::provider_screening::{ScreeningConfig, SubscriptionEvidence, SubscriptionState},
};

pub fn enforcement_enabled(db: &Database) -> bool {
    db.provider_admission_enforced().unwrap_or(true)
}

/// Auth scheme follows the CC Switch field that supplied this exact credential.
/// Unbound manual providers retain their API-key contract while enforcement is off.
pub(crate) fn auth_scheme(
    db: &Database,
    provider: &ProxyProvider,
) -> Result<crate::proxy::providers::AnthropicAuthScheme, String> {
    use crate::proxy::providers::AnthropicAuthScheme;
    // An existing source binding remains authoritative even while global
    // enforcement is off. A changed/unreadable source is not a manual provider.
    let scheme = source_cache::validated_auth_scheme(db, provider)?;
    if provider.provider_type == "anthropic" {
        if enforcement_enabled(db) {
            current_observation(db, provider)?;
        }
        Ok(scheme)
    } else {
        Ok(AnthropicAuthScheme::ApiKey)
    }
}

pub(crate) fn source_identity(db: &Database, provider: &ProxyProvider) -> Option<String> {
    // This identifies a transport/source binding, not subscription eligibility.
    // A bound connection must be capturable before its first status observation.
    source_cache::current_plan(db, provider)
        .ok()
        .flatten()
        .map(|plan| format!("cc_switch:{}", plan.binding))
}

pub(crate) fn policy_revision(config: &ScreeningConfig) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        crate::services::provider_screening::PROBE_CONTRACT_REVISION,
        config.max_latency_ms,
        config.probe_deadline_ms,
        config.max_output_tokens,
        config.max_response_chars
    )
}

pub(crate) fn admission_evidence(evidence: &SubscriptionEvidence) -> Admission_evidence {
    Admission_evidence {
        revision: evidence.evidence_id.clone(),
        state: match evidence.state {
            SubscriptionState::Active => Admission_state::Active,
            SubscriptionState::Inactive => Admission_state::Inactive,
            SubscriptionState::Unknown => Admission_state::Unknown,
            SubscriptionState::NotRequired => Admission_state::NotRequired,
        },
        valid_until: evidence.valid_until,
    }
}

fn current_provider_matches(db: &Database, provider: &ProxyProvider) -> Result<(), String> {
    let current = db
        .get_proxy_provider(&provider.id)
        .map_err(|_| "provider_admission_unavailable")?
        .ok_or("provider_admission_missing")?;
    if !current.enabled
        || provider_configuration_identity(&current) != provider_configuration_identity(provider)
    {
        return Err("provider_admission_configuration_changed".into());
    }
    Ok(())
}

/// Pure local read: source changes and evidence expiry remove pairs immediately.
pub fn admitted_models(db: &Database, provider: &ProxyProvider) -> Vec<String> {
    if !enforcement_enabled(db) {
        return provider.models.clone();
    }
    let result = (|| {
        let observation = current_observation(db, provider)?;
        if !observation.probe_allowed {
            return Ok(Vec::new());
        }
        let policy = policy_revision(&ScreeningConfig::load(db)?);
        db.accepted_provider_models(
            provider,
            &admission_evidence(&observation.evidence),
            &policy,
            chrono::Utc::now().timestamp(),
        )
        .map_err(|_| "provider_admission_unavailable".to_string())
    })();
    result
        .unwrap_or_default()
        .into_iter()
        .filter(|model| provider.models.contains(model))
        .collect()
}

pub async fn ensure_provider_admitted(
    db: &Database,
    provider: &ProxyProvider,
    model: &str,
) -> Result<(), String> {
    if !enforcement_enabled(db) {
        return Ok(());
    }
    current_provider_matches(db, provider)?;
    source_cache::refresh_if_stale(db, provider).await?;
    current_provider_matches(db, provider)?;
    if !db
        .get_proxy_provider(&provider.id)
        .map_err(|_| "provider_admission_unavailable")?
        .is_some_and(|current| current.models.iter().any(|configured| configured == model))
    {
        return Err("provider_admission_excluded".into());
    }
    if !admitted_models(db, provider)
        .iter()
        .any(|candidate| candidate == model)
    {
        return Err("provider_admission_excluded".into());
    }
    Ok(())
}

pub async fn ensure_catalog_admitted(
    db: &Database,
    provider: &ProxyProvider,
) -> Result<(), String> {
    if !enforcement_enabled(db) {
        return Ok(());
    }
    current_provider_matches(db, provider)?;
    source_cache::refresh_if_stale(db, provider).await?;
    current_provider_matches(db, provider)?;
    if admitted_models(db, provider).is_empty() {
        return Err("provider_admission_excluded".into());
    }
    Ok(())
}

/// Discovery extends the inventory of a screened connection, without pretending
/// each discovered model received its own inference probe.
pub async fn refresh_model_admission(
    db: &Database,
    provider: &ProxyProvider,
    models: &[String],
) -> Result<(), String> {
    if !enforcement_enabled(db) {
        return Ok(());
    }
    ensure_catalog_admitted(db, provider).await?;
    let observation = current_observation(db, provider)?;
    let policy = policy_revision(&ScreeningConfig::load(db)?);
    let record = db
        .provider_admission_record(&provider.id)
        .map_err(|_| "provider_admission_unavailable")?
        .ok_or("provider_admission_missing")?;
    let mut inventory = record.offered_models.clone();
    inventory.extend(models.iter().cloned());
    inventory.sort();
    inventory.dedup();
    db.provider_admission_extend_models(
        provider,
        &admission_evidence(&observation.evidence),
        &policy,
        record.version,
        inventory,
    )
    .map_err(|_| "provider_admission_conflict".to_string())?;
    Ok(())
}

pub(crate) async fn refresh_runtime_subscriptions(db: &Database) -> Result<(), String> {
    if !enforcement_enabled(db) {
        return Ok(());
    }
    for provider in db
        .get_all_proxy_providers()
        .map_err(|_| "provider_admission_unavailable")?
    {
        // Missing, inactive and failed admissions are not automatic rechecks.
        let _ = source_cache::refresh_if_stale(db, &provider).await;
    }
    Ok(())
}
