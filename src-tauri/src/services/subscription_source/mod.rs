//! CC Switch owns usage-query configuration; AAG implements only fixed native
//! templates, never executes stored JavaScript, and never reads native OAuth homes.
//! Contracts inspected: farion1231/cc-switch services/coding_plan.rs, balance.rs,
//! commands/provider.rs and provider.rs (2026-10-29). No status query is startup work.
mod native;
#[cfg(test)]
mod tests;

use crate::{
    database::dao::proxy_providers::ProxyProvider,
    proxy::providers::AnthropicAuthScheme,
    services::{
        import_cc_switch::{
            cc_switch_db::{read_cc_switch_providers, CcSwitchRow},
            settings_extract::{extract_upstream_pair, settings_env},
        },
        provider_screening::{SubscriptionEvidence, SubscriptionState},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const STATUS_TIMEOUT_MS: u64 = 15_000;
const MAX_STATUS_BYTES: usize = 64 * 1024;
/// Freshness of an observation, not a claimed vendor subscription end date.
const EVIDENCE_TTL_SECS: i64 = 300;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceReason {
    Ready,
    MissingUsageConfiguration,
    UsageQueryDisabled,
    ProviderDisabled,
    UnsupportedTemplate,
    UnsupportedProvider,
    SourceBindingMismatch,
    InvalidSourceMetadata,
    MissingCredential,
    SourceAccountUnsupported,
    HostNotAllowed,
    VerifiedAllowance,
    QuotaExhausted,
    BalanceUnavailable,
    NoSubscription,
    CredentialRejected,
    StatusRateLimited,
    StatusUnavailable,
    MalformedStatus,
    StatusTooLarge,
    StatusTimeout,
    StatusTransportFailed,
    SourceChanged,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeAdapter {
    Kimi,
    Zhipu,
    MiniMax,
    DeepSeek,
    StepFun,
    SiliconFlow,
    OpenRouter,
    Novita,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePlanView {
    pub provider_ref: String,
    pub source_record_ref: String,
    pub binding: String,
    pub reason: SourceReason,
    pub adapter: Option<NativeAdapter>,
    /// Only fixed public allowlisted destinations; no private base URL or token.
    pub status_url: Option<String>,
}

/// Private request/credential state deliberately has no Debug or Serialize.
pub struct SourcePlan {
    pub provider_ref: String,
    pub source_record_ref: String,
    pub binding: String,
    source_path: PathBuf,
    provider: ProxyProvider,
    auth_scheme: AnthropicAuthScheme,
    reason: SourceReason,
    request: Option<native::NativeRequest>,
}
impl SourcePlan {
    pub fn auth_scheme(&self) -> AnthropicAuthScheme {
        self.auth_scheme
    }

    pub fn view(&self) -> SourcePlanView {
        SourcePlanView {
            provider_ref: self.provider_ref.clone(),
            source_record_ref: self.source_record_ref.clone(),
            binding: self.binding.clone(),
            reason: self.reason,
            adapter: self.request.as_ref().map(|request| request.adapter),
            status_url: self.request.as_ref().map(|request| request.url.clone()),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionObservation {
    pub evidence: SubscriptionEvidence,
    /// Active credentials/plan with exhausted allowance must still skip the ok probe.
    pub probe_allowed: bool,
    pub reason: SourceReason,
    pub source_binding: String,
    pub source_record_ref: String,
    pub checked_at: i64,
    pub status_http: Option<u16>,
}

/// Read exactly one CC source identity, and bind its transport credential to the
/// configured AAG provider. Disabled/unsupported metadata produces an inspectable
/// unknown plan without any network capability.
pub fn prepare_cc_switch_source(
    path: &Path,
    source_record_ref: &str,
    provider: &ProxyProvider,
) -> Result<SourcePlan, String> {
    crate::config::private_fs::validate_external_file_path(path)
        .map_err(|_| "subscription_source_path_invalid".to_string())?;
    let canonical_path = path
        .canonicalize()
        .map_err(|_| "subscription_source_unavailable".to_string())?;
    let rows = read_cc_switch_providers(path)
        .map_err(|_| "subscription_source_unavailable".to_string())?;
    let row = rows
        .into_iter()
        .find(|row| source_ref(&row.app_type, &row.id) == source_record_ref)
        .ok_or_else(|| "subscription_source_not_found".to_string())?;
    prepare_row(&canonical_path, source_record_ref, provider, &row)
}

// Same identity contract as the importer's safe_source_ref. Reuse it through
// the existing owning module's crate-visible re-export.
fn source_ref(app_type: &str, id: &str) -> String {
    crate::services::import_cc_switch::subscription_source_ref(app_type, id)
}

fn prepare_row(
    path: &Path,
    source_record_ref: &str,
    provider: &ProxyProvider,
    row: &CcSwitchRow,
) -> Result<SourcePlan, String> {
    let binding = source_binding(path, row, provider);
    let mut plan = SourcePlan {
        provider_ref: provider.id.clone(),
        source_record_ref: source_record_ref.into(),
        binding,
        source_path: path.to_path_buf(),
        provider: provider.clone(),
        auth_scheme: AnthropicAuthScheme::default(),
        reason: SourceReason::InvalidSourceMetadata,
        request: None,
    };
    if row.meta_issue.is_some() {
        return Ok(plan);
    }
    if !provider.enabled {
        plan.reason = SourceReason::ProviderDisabled;
        return Ok(plan);
    }
    let settings: Value = serde_json::from_str(&row.settings_config)
        .map_err(|_| "subscription_source_settings_invalid".to_string())?;
    let Some((base, key)) = extract_upstream_pair(&row.app_type, &settings, row.meta.as_ref())
    else {
        return Ok(plan);
    };
    plan.auth_scheme = source_auth_scheme(row, &settings);
    if key != provider.api_key
        || (base != provider.base_url && !row.endpoint_urls.contains(&provider.base_url))
    {
        plan.reason = SourceReason::SourceBindingMismatch;
        return Ok(plan);
    }
    if row
        .meta
        .as_ref()
        .and_then(|meta| meta.auth_binding_kind.as_deref())
        .is_some_and(|kind| kind != "provider_config")
        || provider.provider_type == "anthropic_passthrough"
    {
        plan.reason = SourceReason::SourceAccountUnsupported;
        return Ok(plan);
    }
    let metadata: Value = serde_json::from_str(&row.meta_json)
        .map_err(|_| "subscription_source_metadata_invalid".to_string())?;
    let Some(usage) = metadata.get("usage_script") else {
        plan.reason = SourceReason::MissingUsageConfiguration;
        return Ok(plan);
    };
    if usage.get("enabled").and_then(Value::as_bool) != Some(true) {
        plan.reason = SourceReason::UsageQueryDisabled;
        return Ok(plan);
    }
    // code is intentionally neither extracted nor executed: these template types
    // use CC Switch's native status path regardless of the script placeholder.
    let template = usage
        .get("templateType")
        .and_then(Value::as_str)
        .unwrap_or("");
    let kind = usage.get("codingPlanProvider").and_then(Value::as_str);
    match native::plan_request(template, kind, &base, &key) {
        Ok(request) => {
            plan.request = Some(request);
            plan.reason = SourceReason::Ready;
        }
        Err(reason) => plan.reason = reason,
    }
    Ok(plan)
}

fn source_auth_scheme(row: &CcSwitchRow, settings: &Value) -> AnthropicAuthScheme {
    let env = settings_env(settings);
    if settings.get("auth_mode").and_then(Value::as_str) == Some("bearer_only")
        || env.get("AUTH_MODE").and_then(Value::as_str) == Some("bearer_only")
    {
        return AnthropicAuthScheme::Bearer;
    }
    if !matches!(
        row.app_type.as_str(),
        "claude_code" | "claude" | "claude_desktop"
    ) {
        return AnthropicAuthScheme::ApiKey;
    }
    // Match extract_upstream_pair exactly: a configured meta field wins only
    // when its value is a string, then AUTH_TOKEN precedes API_KEY. Selecting
    // by field identity also preserves semantics when the two values are equal.
    let selected = row
        .meta
        .as_ref()
        .and_then(|meta| meta.api_key_field.as_deref())
        .filter(|field| !field.is_empty())
        .filter(|field| env.get(*field).and_then(Value::as_str).is_some())
        .or_else(|| {
            env.get("ANTHROPIC_AUTH_TOKEN")
                .and_then(Value::as_str)
                .map(|_| "ANTHROPIC_AUTH_TOKEN")
        })
        .or_else(|| {
            env.get("ANTHROPIC_API_KEY")
                .and_then(Value::as_str)
                .map(|_| "ANTHROPIC_API_KEY")
        });
    if selected == Some("ANTHROPIC_AUTH_TOKEN") {
        AnthropicAuthScheme::Bearer
    } else {
        AnthropicAuthScheme::ApiKey
    }
}

fn source_binding(path: &Path, row: &CcSwitchRow, provider: &ProxyProvider) -> String {
    let mut hash = Sha256::new();
    let path = path.to_string_lossy();
    hash.update((path.len() as u64).to_be_bytes());
    hash.update(path.as_bytes());
    for text in [
        &row.app_type,
        &row.id,
        &row.settings_config,
        &row.meta_json,
        &provider.id,
        &provider.provider_type,
        &provider.base_url,
        &provider.api_key,
    ] {
        hash.update((text.len() as u64).to_be_bytes());
        hash.update(text.as_bytes());
    }
    for endpoint in &row.endpoint_urls {
        hash.update((endpoint.len() as u64).to_be_bytes());
        hash.update(endpoint.as_bytes());
    }
    let headers: std::collections::BTreeMap<_, _> = provider.extra_headers.iter().collect();
    for (name, value) in headers {
        for text in [name, value] {
            hash.update((text.len() as u64).to_be_bytes());
            hash.update(text.as_bytes());
        }
    }
    hash.update([u8::from(provider.enabled)]);
    hex::encode(hash.finalize())
}

/// No status request. Owners can invalidate cached grants immediately when CC
/// settings, credentials or the AAG connection change.
pub fn validate_current_binding(
    plan: &SourcePlan,
    provider: &ProxyProvider,
) -> Result<bool, String> {
    let current = prepare_cc_switch_source(&plan.source_path, &plan.source_record_ref, provider)?;
    Ok(current.binding == plan.binding && current.reason != SourceReason::SourceBindingMismatch)
}

fn observation(
    plan: &SourcePlan,
    state: SubscriptionState,
    allowed: bool,
    reason: SourceReason,
    status: Option<u16>,
) -> SubscriptionObservation {
    let now = chrono::Utc::now().timestamp();
    SubscriptionObservation {
        evidence: SubscriptionEvidence {
            state,
            provider_ref: plan.provider_ref.clone(),
            evidence_id: format!("cc_switch:{}", plan.binding),
            provenance: "cc_switch_native_status".into(),
            valid_until: Some(now + EVIDENCE_TTL_SECS),
        },
        probe_allowed: allowed,
        reason,
        source_binding: plan.binding.clone(),
        source_record_ref: plan.source_record_ref.clone(),
        checked_at: now,
        status_http: status,
    }
}

/// Explicit fresh status query, including an explicitly requested renewal recheck.
/// Normal routing must retain a matching known-inactive cache without calling
/// this method; it must never infer renewal from elapsed time or a provider name.
pub async fn query_subscription(plan: &SourcePlan) -> SubscriptionObservation {
    let Some(request) = plan.request.as_ref() else {
        return observation(plan, SubscriptionState::Unknown, false, plan.reason, None);
    };
    if !validate_current_binding(plan, &plan.provider).unwrap_or(false) {
        return observation(
            plan,
            SubscriptionState::Unknown,
            false,
            SourceReason::SourceChanged,
            None,
        );
    }
    let result = query_native(request).await;
    if !validate_current_binding(plan, &plan.provider).unwrap_or(false) {
        return observation(
            plan,
            SubscriptionState::Unknown,
            false,
            SourceReason::SourceChanged,
            None,
        );
    }
    match result {
        Ok((status, body)) => {
            let (state, allowed, reason) = native::interpret(request.adapter, status, &body);
            observation(plan, state, allowed, reason, Some(status))
        }
        Err(reason) => observation(plan, SubscriptionState::Unknown, false, reason, None),
    }
}

async fn query_native(request: &native::NativeRequest) -> Result<(u16, Value), SourceReason> {
    let mut authorization = reqwest::header::HeaderValue::from_str(&request.authorization)
        .map_err(|_| SourceReason::InvalidSourceMetadata)?;
    authorization.set_sensitive(true);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(STATUS_TIMEOUT_MS))
        .build()
        .map_err(|_| SourceReason::StatusTransportFailed)?;
    let mut response = client
        .get(&request.url)
        .header(reqwest::header::AUTHORIZATION, authorization)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                SourceReason::StatusTimeout
            } else {
                SourceReason::StatusTransportFailed
            }
        })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Ok((status, Value::Null));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_STATUS_BYTES as u64)
    {
        return Err(SourceReason::StatusTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            SourceReason::StatusTimeout
        } else {
            SourceReason::StatusTransportFailed
        }
    })? {
        if chunk.len() > MAX_STATUS_BYTES - body.len() {
            return Err(SourceReason::StatusTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map(|body| (status, body))
        .map_err(|_| SourceReason::MalformedStatus)
}
