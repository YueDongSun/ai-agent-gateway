//! Availability checks run before catalog import or URL discovery.
use super::{apply::ProviderPlan, ProviderReport};
use crate::database::dao::{
    provider_admission::{provider_configuration_identity, Admission_state},
    proxy_providers::ProxyProvider,
    suppliers::{SupplierEndpointRecord, SupplierRecord},
};
use crate::database::Database;
use crate::services::{provider_admission, provider_screening::SubscriptionState};

// Transient observations suppress immediate retries, not every future refresh.
const HEALTH_RETRY_AFTER_SECS: i64 = 300;

fn screening_error_code(reason: &str) -> &str {
    match reason {
        "subscription_inactive"
        | "subscription_unknown"
        | "subscription_expired"
        | "invalid_evidence"
        | "evidence_provider_mismatch"
        | "invalid_configuration"
        | "invalid_model"
        | "provider_disabled"
        | "provider_construction_failed"
        | "probe_unsupported"
        | "client_credential_required"
        | "timeout"
        | "connection_refused"
        | "connection_failed"
        | "abnormal_response"
        | "authentication"
        | "rate_limited"
        | "quota_exceeded"
        | "response_too_large"
        | "provider_error"
        | "latency_exceeded" => reason,
        _ => "probe_failed",
    }
}

fn health_error_code(error: Option<&str>) -> &str {
    match error {
        Some(
            code @ ("proxy_http_client_build_failed"
            | "proxy_models_invalid_header"
            | "proxy_models_decode_failed"
            | "proxy_models_transport_failed"
            | "proxy_models_timeout"
            | "proxy_models_limit_exceeded"
            | "proxy_models_pagination_invalid"
            | "proxy_models_unavailable"),
        ) => code,
        Some(code)
            if code
                .strip_prefix("proxy_models_http_error_")
                .is_some_and(|status| {
                    status.len() == 3
                        && status
                            .parse::<u16>()
                            .is_ok_and(|status| (100..600).contains(&status))
                }) =>
        {
            code
        }
        _ => "catalog_unavailable",
    }
}

pub(crate) fn record_outcome(
    supplier_id: &str,
    provider_id: &str,
    protocol: &str,
    stage: &'static str,
    outcome: &'static str,
    reason: &str,
) {
    let protocol = if crate::database::dao::suppliers::SUPPORTED_PROTOCOLS.contains(&protocol) {
        protocol
    } else {
        "unknown"
    };
    // IDs are correlated through the same reference helper as request diagnostics.
    // Reasons come only from local stable codes, never upstream bodies or URLs.
    let reason = if reason.len() <= 200
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-'))
    {
        reason
    } else {
        "operation_failed"
    };
    log::info!(
        "supplier_import supplier_ref={} provider_ref={} url_type={} stage={} outcome={} reason={}",
        crate::diagnostics::safe_reference(supplier_id),
        crate::diagnostics::safe_reference(provider_id),
        protocol,
        stage,
        outcome,
        reason,
    );
}

pub(super) async fn check(
    db: &Database,
    supplier: &SupplierRecord,
    endpoint: &SupplierEndpointRecord,
    provider: &ProxyProvider,
) -> Option<ProviderReport> {
    let result = exclusion_reason(db, supplier, endpoint, provider).await;
    let (reason, failed) = match result {
        Ok(None) => return None,
        Ok(Some(reason)) => (reason, false),
        Err(reason) => {
            let excluded = matches!(
                reason.as_str(),
                "supplier_url_invalid"
                    | "provider_subscription_source_changed"
                    | "provider_subscription_local_binding_invalid"
                    | "subscription_source_not_found"
                    | "provider_subscription_missing"
            );
            (reason, !excluded)
        }
    };
    let skipped = reason == "auto_tracking_disabled";
    let outcome = if failed {
        "failed"
    } else if skipped {
        "skipped"
    } else {
        "excluded"
    };
    record_outcome(
        &supplier.id,
        &provider.id,
        &endpoint.protocol,
        "preflight",
        outcome,
        &reason,
    );
    Some(ProviderReport {
        provider_id: provider.id.clone(),
        ok: skipped,
        error: Some(if failed {
            format!("supplier_import_failed:preflight:{reason}")
        } else {
            format!("supplier_import_{outcome}:{reason}")
        }),
        plan: ProviderPlan::default(),
    })
}

async fn exclusion_reason(
    db: &Database,
    supplier: &SupplierRecord,
    endpoint: &SupplierEndpointRecord,
    provider: &ProxyProvider,
) -> Result<Option<String>, String> {
    let endpoint_enabled = if endpoint.proxy_provider_id.as_deref() == Some(&provider.id) {
        endpoint.enabled
    } else {
        endpoint
            .alternatives
            .iter()
            .find(|variant| variant.legacy_proxy_provider_id.as_deref() == Some(&provider.id))
            .is_some_and(|variant| variant.enabled)
    };
    if !supplier.enabled || !endpoint_enabled || !provider.enabled {
        return Ok(Some("supplier_disabled".into()));
    }
    if !provider.auto_track_models {
        return Ok(Some("auto_tracking_disabled".into()));
    }
    if matches!(
        provider.provider_type.as_str(),
        "mock" | "anthropic_passthrough"
    ) {
        return Ok(Some("url_type_unprobeable".into()));
    }
    let url =
        reqwest::Url::parse(&provider.base_url).map_err(|_| "supplier_url_invalid".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || provider.base_url.chars().any(char::is_control)
    {
        return Ok(Some("supplier_url_invalid".into()));
    }
    let now = chrono::Utc::now().timestamp();
    // Status refresh uses the existing source owner. An expired observation is
    // not silently treated as a still-active subscription or copied to a new URL.
    if db
        .provider_subscription_value(&provider.id)
        .map_err(|error| error.external_message())?
        .is_some()
    {
        provider_admission::refresh_subscription(db, provider).await?;
        let observation = provider_admission::current_observation(db, provider)?;
        let reason = match observation.evidence.state {
            SubscriptionState::Inactive => Some("subscription_inactive"),
            SubscriptionState::Unknown => Some("subscription_unavailable"),
            _ if observation
                .evidence
                .valid_until
                .is_some_and(|until| until <= now) =>
            {
                Some("subscription_expired")
            }
            _ if !observation.probe_allowed => Some("subscription_unavailable"),
            _ => None,
        };
        if let Some(reason) = reason {
            let source_reason = serde_json::to_value(observation.reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "status_unavailable".into());
            return Ok(Some(format!("{reason}:{source_reason}")));
        }
    }
    let admission = db
        .provider_admission_record(&provider.id)
        .map_err(|error| error.external_message())?;
    if let Some(record) = &admission {
        let reason = if record.configuration_identity != provider_configuration_identity(provider) {
            Some("configuration_obsolete")
        } else if record.evidence.state == Admission_state::Inactive {
            Some("subscription_inactive")
        } else if record
            .evidence
            .valid_until
            .is_some_and(|until| until <= now)
        {
            Some("subscription_expired")
        } else if record.evidence.state == Admission_state::Unknown {
            Some("subscription_unavailable")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Ok(Some(reason.into()));
        }
        if let Some(probe) = record.probe.as_ref().filter(|probe| !probe.eligible) {
            return Ok(Some(format!(
                "screening_unavailable:{}",
                screening_error_code(&probe.reason)
            )));
        }
    }
    if let Some(health) = db
        .supplier_discovery_health(&provider.id)
        .map_err(|error| error.external_message())?
    {
        let newer_screening = admission
            .as_ref()
            .and_then(|record| record.probe.as_ref())
            .is_some_and(|probe| probe.eligible && probe.checked_at > health.checked_at);
        // A malformed catalog is not proof that its supplier is unavailable.
        // Preserve explicit retry/validation behavior for invalid response data.
        if health.status != "catalog_available"
            && health.status != "catalog_invalid"
            && !newer_screening
            && now.saturating_sub(health.checked_at) < HEALTH_RETRY_AFTER_SECS
        {
            return Ok(Some(format!(
                "url_unavailable:{}",
                health_error_code(health.error.as_deref())
            )));
        }
    }
    Ok(None)
}
