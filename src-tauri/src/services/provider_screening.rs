//! Explicit, subscription-gated screening. This engine neither invents evidence
//! nor persists it, and sends only a fixed health instruction without business context.
use crate::{
    database::dao::proxy_providers::ProxyProvider,
    proxy::{
        canonical::{ChatCompletionRequest, ChatMessage, ProviderError, UpstreamErrorKind},
        providers::{build_provider, Provider},
    },
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const MAX_CONFIGURED_DEADLINE_MS: u64 = 120_000;
const MAX_REPORT_CHARS: usize = 4096;
const MAX_ID_CHARS: usize = 256;
const PROBE_TEXT: &str = "ok";
const PROBE_INSTRUCTION: &str = "Reply with exactly: ok";
/// Receipt identity changes with the health-request contract.
pub(crate) const PROBE_CONTRACT_REVISION: &str = "v2";
const MAX_PROBE_RESPONSE_BYTES: usize = 16 * 1024;
pub const SCREENING_CONFIG_KEY: &str = "provider_screening_policy";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionState {
    Active,
    Inactive,
    Unknown,
    NotRequired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionEvidence {
    pub state: SubscriptionState,
    /// Exact configured provider identity covered by this evidence.
    pub provider_ref: String,
    /// Opaque identifier supplied by the authoritative evidence owner.
    pub evidence_id: String,
    /// Non-secret source category/reference, never a credential or response body.
    pub provenance: String,
    pub valid_until: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScreeningConfig {
    pub max_latency_ms: u64,
    pub probe_deadline_ms: u64,
    pub max_output_tokens: u32,
    pub max_response_chars: usize,
}

impl Default for ScreeningConfig {
    fn default() -> Self {
        Self {
            max_latency_ms: 5_000,
            probe_deadline_ms: 15_000,
            max_output_tokens: 64,
            max_response_chars: 1024,
        }
    }
}

impl ScreeningConfig {
    pub fn parse(value: &str) -> Result<Self, String> {
        let config: Self =
            serde_json::from_str(value).map_err(|_| "provider_screening_policy_invalid")?;
        if !config.validate() {
            return Err("provider_screening_policy_invalid".into());
        }
        Ok(config)
    }

    pub fn load(db: &crate::database::Database) -> Result<Self, String> {
        match db
            .get_setting(SCREENING_CONFIG_KEY)
            .map_err(|_| "provider_screening_policy_unavailable")?
        {
            Some(raw) => Self::parse(&raw),
            None => Ok(Self::default()),
        }
    }

    pub fn validate(&self) -> bool {
        (1..=MAX_CONFIGURED_DEADLINE_MS).contains(&self.probe_deadline_ms)
            && (1..=self.probe_deadline_ms).contains(&self.max_latency_ms)
            && (1..=128).contains(&self.max_output_tokens)
            && (1..=MAX_REPORT_CHARS).contains(&self.max_response_chars)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreeningStage {
    Subscription,
    Configuration,
    Probe,
    Admission,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreeningReason {
    Accepted,
    SubscriptionInactive,
    SubscriptionUnknown,
    SubscriptionExpired,
    InvalidEvidence,
    EvidenceProviderMismatch,
    InvalidConfiguration,
    InvalidModel,
    ProviderDisabled,
    ProviderConstructionFailed,
    ProbeUnsupported,
    ClientCredentialRequired,
    Timeout,
    ConnectionRefused,
    ConnectionFailed,
    AbnormalResponse,
    Authentication,
    RateLimited,
    QuotaExceeded,
    ResponseTooLarge,
    ProviderError,
    LatencyExceeded,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreeningReport {
    pub eligible: bool,
    pub stage: ScreeningStage,
    pub reason: ScreeningReason,
    pub checked_at: i64,
    pub evidence_identity: String,
    pub evidence_provenance: String,
    /// Actual elapsed time around the provider call, including transport/body parsing.
    pub latency_ms: Option<u64>,
    pub response: Option<String>,
    pub response_truncated: bool,
    pub response_redacted: bool,
    pub upstream_status: Option<u16>,
    pub upstream_code: Option<String>,
}

fn evidence_exclusion(evidence: &SubscriptionEvidence, now: i64) -> Option<ScreeningReason> {
    match evidence.state {
        SubscriptionState::Inactive => Some(ScreeningReason::SubscriptionInactive),
        SubscriptionState::Unknown => Some(ScreeningReason::SubscriptionUnknown),
        _ if evidence.valid_until.is_some_and(|deadline| deadline <= now) => {
            Some(ScreeningReason::SubscriptionExpired)
        }
        _ if !valid_identifier(&evidence.evidence_id)
            || !valid_identifier(&evidence.provenance) =>
        {
            Some(ScreeningReason::InvalidEvidence)
        }
        _ => None,
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_ID_CHARS
        && !value.chars().any(char::is_control)
}

fn bounded_redacted(text: &str, provider: &ProxyProvider, limit: usize) -> (String, bool, bool) {
    let mut redacted = text.to_owned();
    let mut secrets: Vec<_> = std::iter::once(provider.api_key.as_str())
        .chain(provider.extra_headers.values().map(String::as_str))
        .filter(|value| !value.is_empty())
        .collect();
    secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
    for secret in secrets {
        redacted = redacted.replace(secret, "[redacted]");
    }
    let changed = redacted != text;
    let truncated = redacted.chars().count() > limit;
    (redacted.chars().take(limit).collect(), truncated, changed)
}

fn classify_error(error: &ProviderError) -> ScreeningReason {
    if error.stage == "connection_refused" {
        return ScreeningReason::ConnectionRefused;
    }
    if error.code == "upstream_timeout" || error.stage == "timeout" {
        return ScreeningReason::Timeout;
    }
    if error.stage == "connect" {
        return ScreeningReason::ConnectionFailed;
    }
    if error.code == "upstream_response_too_large" {
        return ScreeningReason::ResponseTooLarge;
    }
    if matches!(error.kind, Some(UpstreamErrorKind::AuthExpired)) {
        return ScreeningReason::Authentication;
    }
    if matches!(error.kind, Some(UpstreamErrorKind::QuotaExceeded)) {
        return ScreeningReason::QuotaExceeded;
    }
    if matches!(error.kind, Some(UpstreamErrorKind::RateLimited)) {
        return ScreeningReason::RateLimited;
    }
    if matches!(error.status, Some(401 | 403)) {
        return ScreeningReason::Authentication;
    }
    if error.status == Some(429) {
        return ScreeningReason::RateLimited;
    }
    if error.code == "upstream_decode_error" {
        return ScreeningReason::AbnormalResponse;
    }
    ScreeningReason::ProviderError
}

fn safe_upstream_code(code: &str) -> &str {
    if code.len() <= 80
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && matches!(
            code,
            "provider_error"
                | "upstream_timeout"
                | "upstream_transport_error"
                | "upstream_http_error"
                | "upstream_decode_error"
                | "upstream_response_too_large"
                | "upstream_empty_response"
                | "upstream_protocol_error"
                | "responses_upstream_state_unsupported"
                | "gemini_response_unsupported_semantics"
        )
    {
        code
    } else {
        "provider_error"
    }
}

fn attach_probe_capture(
    report: &mut ScreeningReport,
    instance: &dyn Provider,
    provider: &ProxyProvider,
    config: &ScreeningConfig,
) {
    let (captured_truncated, captured_redacted) = instance.probe_response_flags();
    if let Some(body) = instance.take_probe_response() {
        let (actual, truncated, redacted) =
            bounded_redacted(&body, provider, config.max_response_chars);
        report.response = Some(actual);
        report.response_truncated = captured_truncated || truncated;
        report.response_redacted = captured_redacted || redacted;
    }
}

pub async fn screen_provider(
    provider: &ProxyProvider,
    model: &str,
    evidence: &SubscriptionEvidence,
    config: &ScreeningConfig,
) -> ScreeningReport {
    screen_with_factory(provider, model, evidence, config, build_provider).await
}

pub(crate) async fn screen_provider_with_auth(
    provider: &ProxyProvider,
    model: &str,
    evidence: &SubscriptionEvidence,
    config: &ScreeningConfig,
    auth_scheme: crate::proxy::providers::AnthropicAuthScheme,
) -> ScreeningReport {
    screen_with_factory(provider, model, evidence, config, |provider| {
        crate::proxy::providers::build_provider_with_auth(provider, auth_scheme)
    }).await
}

async fn screen_with_factory<F>(
    provider: &ProxyProvider,
    model: &str,
    evidence: &SubscriptionEvidence,
    config: &ScreeningConfig,
    factory: F,
) -> ScreeningReport
where
    F: FnOnce(&ProxyProvider) -> Result<Box<dyn Provider>, String>,
{
    let now = chrono::Utc::now().timestamp();
    let mut report = ScreeningReport {
        eligible: false,
        stage: ScreeningStage::Subscription,
        reason: ScreeningReason::SubscriptionUnknown,
        checked_at: now,
        evidence_identity: if valid_identifier(&evidence.evidence_id) {
            bounded_redacted(&evidence.evidence_id, provider, MAX_ID_CHARS).0
        } else {
            String::new()
        },
        evidence_provenance: if valid_identifier(&evidence.provenance) {
            bounded_redacted(&evidence.provenance, provider, MAX_ID_CHARS).0
        } else {
            String::new()
        },
        latency_ms: None,
        response: None,
        response_truncated: false,
        response_redacted: false,
        upstream_status: None,
        upstream_code: None,
    };
    if let Some(reason) = evidence_exclusion(evidence, now) {
        report.reason = reason;
        return report;
    }
    if evidence.provider_ref != provider.id {
        report.reason = ScreeningReason::EvidenceProviderMismatch;
        return report;
    }
    report.stage = ScreeningStage::Configuration;
    if !config.validate() {
        report.reason = ScreeningReason::InvalidConfiguration;
        return report;
    }
    if !valid_identifier(model) {
        report.reason = ScreeningReason::InvalidModel;
        return report;
    }
    if !provider.enabled {
        report.reason = ScreeningReason::ProviderDisabled;
        return report;
    }
    let mut instance = match factory(provider) {
        Ok(instance) => instance,
        Err(_) => {
            report.reason = ScreeningReason::ProviderConstructionFailed;
            return report;
        }
    };
    if !instance.set_response_byte_limit(MAX_PROBE_RESPONSE_BYTES) {
        report.reason = ScreeningReason::ProbeUnsupported;
        return report;
    }
    if instance.requires_client_credential() {
        report.reason = ScreeningReason::ClientCredentialRequired;
        return report;
    }
    let request = ChatCompletionRequest {
        model: model.into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: serde_json::Value::String(PROBE_INSTRUCTION.into()),
            tool_calls: None,
            tool_call_id: None,
            extra: Default::default(),
        }],
        stream: false,
        max_tokens: Some(config.max_output_tokens),
        ..Default::default()
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let remaining_ms = evidence
        .valid_until
        .map(|deadline| deadline.saturating_mul(1000).saturating_sub(now_ms))
        .unwrap_or(i64::MAX);
    if remaining_ms <= 0 {
        report.stage = ScreeningStage::Subscription;
        report.reason = ScreeningReason::SubscriptionExpired;
        return report;
    }
    let deadline_ms = config.probe_deadline_ms.min(remaining_ms as u64);
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_millis(deadline_ms),
        instance.complete(&request, model),
    )
    .await;
    report.latency_ms = Some(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
    report.stage = ScreeningStage::Probe;
    let response = match result {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            report.reason = classify_error(&error);
            // Decode/transport failures use a local gateway status (often502),
            // which is not an observed upstream HTTP status.
            report.upstream_status = (error.stage == "response_status").then_some(error.status).flatten();
            report.upstream_code = Some(safe_upstream_code(&error.code).to_string());
            attach_probe_capture(&mut report, instance.as_ref(), provider, config);
            return report;
        }
        Err(_) => {
            attach_probe_capture(&mut report, instance.as_ref(), provider, config);
            report.reason = if evidence
                .valid_until
                .is_some_and(|deadline| deadline <= chrono::Utc::now().timestamp())
            {
                report.stage = ScreeningStage::Subscription;
                ScreeningReason::SubscriptionExpired
            } else {
                ScreeningReason::Timeout
            };
            return report;
        }
    };
    let choice = response.choices.first();
    let text = choice
        .and_then(|choice| choice.message.content.as_deref())
        .unwrap_or("");
    let (actual, truncated, redacted) = bounded_redacted(text, provider, config.max_response_chars);
    report.response = Some(actual);
    report.response_truncated = truncated;
    report.response_redacted = redacted;
    if let Some(reason) = evidence_exclusion(evidence, chrono::Utc::now().timestamp()) {
        report.stage = ScreeningStage::Subscription;
        report.reason = reason;
        return report;
    }
    if response.choices.len() != 1
        || text.trim() != PROBE_TEXT
        || choice.and_then(|choice| choice.finish_reason.as_deref()) != Some("stop")
        || choice.is_some_and(|choice| choice.message.role != "assistant")
        || choice.is_some_and(|choice| {
            choice
                .message
                .refusal
                .as_ref()
                .is_some_and(|value| !value.is_empty())
        })
        || choice.is_some_and(|choice| {
            choice
                .message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty())
        })
    {
        report.reason = ScreeningReason::AbnormalResponse;
        // A successful HTTP envelope can contain only reasoning or terminate
        // at its output limit. The visible-text projection loses that evidence.
        // Capture was enabled solely for this bounded, literal-ok screening
        // request; retain its already redacted envelope without changing admission.
        if text.trim().is_empty()
            || choice.and_then(|choice| choice.finish_reason.as_deref()) != Some("stop")
        {
            attach_probe_capture(&mut report, instance.as_ref(), provider, config);
        }
        return report;
    }
    report.stage = ScreeningStage::Admission;
    if report.latency_ms.unwrap_or(u64::MAX) > config.max_latency_ms {
        report.reason = ScreeningReason::LatencyExceeded;
        return report;
    }
    report.reason = ScreeningReason::Accepted;
    report.eligible = true;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Json, State},
        http::StatusCode,
        routing::post,
        Router,
    };
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    struct Fixture {
        base_url: String,
        requests: Arc<Mutex<Vec<Value>>>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl Fixture {
        async fn raw(body: &str, status: StatusCode) -> Self {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let app = Router::new()
                .fallback(post(
                    |State((requests, body, status)): State<(
                        Arc<Mutex<Vec<Value>>>,
                        String,
                        StatusCode,
                    )>,
                     Json(request): Json<Value>| async move {
                        requests.lock().unwrap().push(request);
                        (status, body)
                    },
                ))
                .with_state((requests.clone(), body.to_string(), status));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            Self {
                base_url,
                requests,
                task,
            }
        }
        async fn new(reply: &str, finish: &str, delay_ms: u64, status: StatusCode) -> Self {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let app = Router::new().route("/v1/chat/completions",post(
                |State((requests,reply,finish,delay_ms,status)):State<(Arc<Mutex<Vec<Value>>>,String,String,u64,StatusCode)>,Json(body):Json<Value>|async move {
                    requests.lock().unwrap().push(body);
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    (status,Json(json!({"id":"fixture","object":"chat.completion","created":0,"model":"fixture-model",
                        "choices":[{"index":0,"finish_reason":finish,"message":{"role":"assistant","content":reply}}]})))
                })).with_state((requests.clone(),reply.to_string(),finish.to_string(),delay_ms,status));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            Self {
                base_url,
                requests,
                task,
            }
        }
        fn provider(&self) -> ProxyProvider {
            serde_json::from_value(json!({"id":"fixture_provider","name":"Claims to be fast","type":"openai",
                "baseURL":self.base_url,"apiKey":"fixture_key","models":["fixture-model"],"enabled":true})).unwrap()
        }
    }
    fn evidence(state: SubscriptionState) -> SubscriptionEvidence {
        SubscriptionEvidence {
            state,
            provider_ref: "fixture_provider".into(),
            evidence_id: "fixture_evidence".into(),
            provenance: "explicit_test_fixture".into(),
            valid_until: None,
        }
    }

    #[tokio::test]
    async fn malformed_success_and_http_failure_return_actual_probe_body_for_every_codec() {
        for protocol in ["openai", "anthropic", "openai-responses", "gemini"] {
            for (status, reason) in [
                (StatusCode::OK, ScreeningReason::AbnormalResponse),
                (StatusCode::UNAUTHORIZED, ScreeningReason::Authentication),
            ] {
                let fixture = Fixture::raw(
                    "Gateway response: fixture_key / header_fixture_secret",
                    status,
                )
                .await;
                let mut provider = fixture.provider();
                provider.provider_type = protocol.into();
                provider
                    .extra_headers
                    .insert("x-fixture-secret".into(), "header_fixture_secret".into());
                let report = screen_provider(
                    &provider,
                    "fixture-model",
                    &evidence(SubscriptionState::Active),
                    &ScreeningConfig::default(),
                )
                .await;
                assert_eq!(report.reason, reason, "{protocol}");
                assert_eq!(
                    report.response.as_deref(),
                    Some("Gateway response: [redacted] / [redacted]"),
                    "{protocol}"
                );
                assert!(report.response_redacted);
                assert!(!report.response_truncated);
                assert!(!report.eligible);
                assert_eq!(fixture.requests.lock().unwrap().len(), 1);
                let mut ordinary = build_provider(&provider).unwrap();
                let request = ChatCompletionRequest {
                    model: "fixture-model".into(),
                    messages: vec![ChatMessage {
                        role: "user".into(),
                        content: json!("Business payload must never be captured"),
                        tool_calls: None,
                        tool_call_id: None,
                        extra: Default::default(),
                    }],
                    max_tokens: Some(16),
                    ..Default::default()
                };
                assert!(ordinary.complete(&request, "fixture-model").await.is_err());
                assert!(
                    ordinary.take_probe_response().is_none(),
                    "ordinary {protocol} must retain no body"
                );
                assert_eq!(ordinary.probe_response_flags(), (false, false));
                // Enabling a cap is explicit; merely building a real codec did not enable capture.
                assert!(ordinary.set_response_byte_limit(MAX_PROBE_RESPONSE_BYTES));
            }
        }
    }

    #[tokio::test]
    async fn transport_capture_truncation_survives_later_redaction_and_report_limits() {
        let secret = "very_long_fixture_secret_".repeat(128);
        let body = format!("{}{}", secret.repeat(30), &secret[..64]);
        let fixture = Fixture::raw(&body, StatusCode::UNAUTHORIZED).await;
        let mut provider = fixture.provider();
        provider.api_key = secret;
        let report = screen_provider(
            &provider,
            "fixture-model",
            &evidence(SubscriptionState::Active),
            &ScreeningConfig::default(),
        )
        .await;
        assert_eq!(report.reason, ScreeningReason::Authentication);
        assert!(report.response_truncated);
        assert!(report.response_redacted);
        assert!(report.response.as_ref().unwrap().chars().count() <= 1024);
        assert!(!report
            .response
            .as_ref()
            .unwrap()
            .contains("very_long_fixture_secret"));
    }

    #[tokio::test]
    async fn empty_or_incomplete_success_retains_bounded_redacted_anthropic_envelope() {
        for (content, stop_reason) in [
            (json!([{"type":"thinking","thinking":"fixture_key header_fixture_secret reasoning-only fixture","signature":"fixture_signature"}]), "max_tokens"),
            (json!([{"type":"text","text":"ok"}]), "max_tokens"),
            (json!([]), "end_turn"),
        ] {
            let envelope = json!({"id":"fixture","type":"message","role":"assistant","model":"fixture-model",
                "content":content,"stop_reason":stop_reason,"usage":{"input_tokens":1,"output_tokens":64},
                "fixture_detail":"fixture_key header_fixture_secret"});
            let fixture = Fixture::raw(&envelope.to_string(), StatusCode::OK).await;
            let mut provider = fixture.provider();
            provider.provider_type = "anthropic".into();
            provider.extra_headers.insert("x-fixture-secret".into(), "header_fixture_secret".into());
            let report = screen_provider(&provider, "fixture-model", &evidence(SubscriptionState::Active), &ScreeningConfig::default()).await;
            assert!(!report.eligible);
            assert_eq!(report.reason, ScreeningReason::AbnormalResponse);
            assert!(report.response_redacted);
            assert!(!report.response_truncated);
            let actual = report.response.as_deref().unwrap();
            let captured: Value = serde_json::from_str(actual).expect("retain the successful HTTP envelope, not an empty assistant projection");
            assert_eq!(captured["stop_reason"], stop_reason);
            assert_eq!(captured["usage"]["output_tokens"], 64);
            assert!(!actual.contains("fixture_key"));
            assert!(!actual.contains("header_fixture_secret"));
            assert_eq!(fixture.requests.lock().unwrap().len(), 1);
        }

        let envelope = json!({"id":"fixture","type":"message","role":"assistant","model":"fixture-model",
            "content":[{"type":"thinking","thinking":format!("fixture_key {}", "synthetic reasoning ".repeat(200))}],
            "stop_reason":"max_tokens","usage":{"input_tokens":1,"output_tokens":64}});
        let fixture = Fixture::raw(&envelope.to_string(), StatusCode::OK).await;
        let mut provider = fixture.provider(); provider.provider_type = "anthropic".into();
        let config = ScreeningConfig { max_response_chars: 128, ..Default::default() };
        let report = screen_provider(&provider, "fixture-model", &evidence(SubscriptionState::Active), &config).await;
        assert_eq!(report.reason, ScreeningReason::AbnormalResponse);
        assert!(!report.eligible);
        assert!(report.response_redacted && report.response_truncated);
        assert!(report.response.as_deref().unwrap().chars().count() <= 128);
        assert!(!report.response.as_deref().unwrap().contains("fixture_key"));
    }

    #[tokio::test]
    async fn json_escape_variants_of_credentials_are_redacted_in_http_probe_captures() {
        for protocol in ["openai", "anthropic", "openai-responses", "gemini"] {
            for (key, header, body) in [
                (
                    "fixture/key",
                    "header/secret",
                    r#"{"error":"fixture\/key","header":"header\/secret","nested":{"fixture\/key":["header\/secret"]}}"#,
                ),
                (
                    "fixture_key",
                    "header_secret",
                    r#"{"error":"\u0066ixture_key","\u0066ixture_key":"\u0068eader_secret"}"#,
                ),
            ] {
                let fixture = Fixture::raw(body, StatusCode::UNAUTHORIZED).await;
                let mut provider = fixture.provider();
                provider.provider_type = protocol.into();
                provider.api_key = key.into();
                provider
                    .extra_headers
                    .insert("x-fixture-secret".into(), header.into());
                let report = screen_provider(
                    &provider,
                    "fixture-model",
                    &evidence(SubscriptionState::Active),
                    &ScreeningConfig::default(),
                )
                .await;
                assert_eq!(report.reason, ScreeningReason::Authentication, "{protocol}");
                assert!(report.response_redacted, "{protocol}");
                assert!(!report.response_truncated);
                let decoded: Value =
                    serde_json::from_str(report.response.as_deref().unwrap()).unwrap();
                let normalized = decoded.to_string();
                assert!(
                    !normalized.contains(key),
                    "decoded API key leaked for {protocol}"
                );
                assert!(
                    !normalized.contains(header),
                    "decoded header secret leaked for {protocol}"
                );
                assert_eq!(decoded["error"], "[redacted]");
                assert!(
                    decoded.get("[redacted]").is_some()
                        || decoded["nested"].get("[redacted]").is_some()
                );
                assert_eq!(fixture.requests.lock().unwrap().len(), 1);
            }
        }
        // A malformed protocol envelope can still be valid JSON. Its decode
        // failure must use the same decoded-string redaction as HTTP errors.
        let fixture = Fixture::raw(r#"{"error":"\u0066ixture_key"}"#, StatusCode::OK).await;
        let report = screen_provider(
            &fixture.provider(),
            "fixture-model",
            &evidence(SubscriptionState::Active),
            &ScreeningConfig::default(),
        )
        .await;
        assert_eq!(report.reason, ScreeningReason::AbnormalResponse);
        assert!(report.response_redacted);
        assert_eq!(report.upstream_status, None, "local decode status is not an upstream HTTP result");
        assert_eq!(
            report.response.as_deref(),
            Some(r#"{"error":"[redacted]"}"#)
        );
    }

    #[test]
    fn persisted_screening_policy_defaults_and_validation_are_explicit() {
        let db = crate::database::Database::memory().unwrap();
        let defaults = ScreeningConfig::load(&db).unwrap();
        assert_eq!(defaults.max_latency_ms, 5000);
        assert_eq!(defaults.probe_deadline_ms, 15000);
        db.set_setting(
            SCREENING_CONFIG_KEY,
            r#"{"max_latency_ms":2500,"probe_deadline_ms":9000}"#,
        )
        .unwrap();
        assert_eq!(ScreeningConfig::load(&db).unwrap().max_latency_ms, 2500);
        for bad in [
            r#"{"unknown_field":true}"#,
            r#"{"max_latency_ms":0}"#,
            r#"{"probe_deadline_ms":1}"#,
            r#"{"max_output_tokens":999}"#,
        ] {
            assert!(ScreeningConfig::parse(bad).is_err());
            db.set_setting(SCREENING_CONFIG_KEY, bad).unwrap();
            assert!(ScreeningConfig::load(&db).is_err());
        }
    }

    #[tokio::test]
    async fn inactive_unknown_and_expired_evidence_exclude_before_factory_or_network() {
        let fixture = Fixture::new("ok", "stop", 0, StatusCode::OK).await;
        for (state, valid_until, expected) in [
            (
                SubscriptionState::Inactive,
                None,
                ScreeningReason::SubscriptionInactive,
            ),
            (
                SubscriptionState::Unknown,
                None,
                ScreeningReason::SubscriptionUnknown,
            ),
            (
                SubscriptionState::Active,
                Some(1),
                ScreeningReason::SubscriptionExpired,
            ),
        ] {
            let mut evidence = evidence(state);
            evidence.valid_until = valid_until;
            let report = screen_with_factory(
                &fixture.provider(),
                "fixture-model",
                &evidence,
                &ScreeningConfig::default(),
                |_| panic!("factory must not run for excluded subscription"),
            )
            .await;
            assert!(!report.eligible);
            assert_eq!(report.reason, expected);
            assert_eq!(report.stage, ScreeningStage::Subscription);
            assert!(report.latency_ms.is_none());
            assert!(report.response.is_none());
        }
        assert!(fixture.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn evidence_for_another_provider_never_constructs_or_contacts_the_target() {
        let fixture = Fixture::new("ok", "stop", 0, StatusCode::OK).await;
        let mut evidence = evidence(SubscriptionState::Active);
        evidence.provider_ref = "different_provider".into();
        let report = screen_with_factory(
            &fixture.provider(),
            "fixture-model",
            &evidence,
            &ScreeningConfig::default(),
            |_| panic!("misbound evidence cannot reach the factory"),
        )
        .await;
        assert_eq!(report.reason, ScreeningReason::EvidenceProviderMismatch);
        assert!(!report.eligible);
        assert!(report.latency_ms.is_none());
        assert!(fixture.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn oversized_success_bodies_are_refused_by_the_instance_bound_before_parsing() {
        let fixture = Fixture::new(
            &"x".repeat(MAX_PROBE_RESPONSE_BYTES + 1),
            "stop",
            0,
            StatusCode::OK,
        )
        .await;
        let report = screen_provider(
            &fixture.provider(),
            "fixture-model",
            &evidence(SubscriptionState::Active),
            &ScreeningConfig::default(),
        )
        .await;
        assert_eq!(report.reason, ScreeningReason::ResponseTooLarge);
        assert!(!report.eligible);
        assert!(report.response.is_none());
        assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn refused_loopback_connection_is_not_mislabeled_as_timeout_or_authentication() {
        // A configured proxy may return a real HTTP 502 for this closed port.
        // This test owns a direct loopback transport failure, so bypass proxies
        // only for its fixture and restore the process environment on Drop.
        let _environment_lock = crate::config::test_env_lock().lock().await;
        struct Loopback_proxy_environment([(&'static str, Option<std::ffi::OsString>); 2]);
        impl Drop for Loopback_proxy_environment {
            fn drop(&mut self) {
                for (name, value) in &self.0 {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
        let _proxy_environment = Loopback_proxy_environment([
            ("NO_PROXY", std::env::var_os("NO_PROXY")),
            ("no_proxy", std::env::var_os("no_proxy")),
        ]);
        for name in ["NO_PROXY", "no_proxy"] {
            std::env::set_var(name, "127.0.0.1,localhost,::1");
        }
        let fixture = Fixture::new("ok", "stop", 0, StatusCode::OK).await;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let mut provider = fixture.provider();
        provider.base_url = base;
        let report = screen_provider(
            &provider,
            "fixture-model",
            &evidence(SubscriptionState::Active),
            &ScreeningConfig::default(),
        )
        .await;
        assert_eq!(
            report.reason,
            ScreeningReason::ConnectionRefused,
            "safe probe outcome: code={:?}, status={:?}",
            report.upstream_code,
            report.upstream_status,
        );
        assert!(!report.eligible);
        assert_eq!(report.upstream_status, None);
        assert_eq!(report.upstream_code.as_deref(), Some("upstream_transport_error"));
        assert!(report.response.is_none());
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains(&provider.base_url));
        assert!(!serialized.contains(&provider.api_key));
        assert!(fixture.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn active_and_not_required_send_one_exact_ok_instruction_and_record_actual_response() {
        let fixture = Fixture::new(" ok \n", "stop", 0, StatusCode::OK).await;
        for state in [SubscriptionState::Active, SubscriptionState::NotRequired] {
            let report = screen_provider(
                &fixture.provider(),
                "fixture-model",
                &evidence(state),
                &ScreeningConfig::default(),
            )
            .await;
            assert!(report.eligible, "{:?}", report.reason);
            assert_eq!(report.reason, ScreeningReason::Accepted);
            assert_eq!(report.response.as_deref(), Some(" ok \n"));
            assert!(report.latency_ms.is_some());
            assert_eq!(report.evidence_identity, "fixture_evidence");
        }
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            assert_eq!(request["messages"], json!([{"role":"user","content":"Reply with exactly: ok"}]));
            assert_eq!(request["model"], "fixture-model");
            assert_eq!(request["stream"], false);
            assert_eq!(request["max_tokens"], 64);
            assert!(request.get("temperature").is_none());
            assert!(request.get("tools").is_none());
        }
    }

    #[tokio::test]
    async fn abnormal_response_is_actual_bounded_and_credentials_are_redacted() {
        let fixture = Fixture::new(
            "Unexpected fixture_key response with extra detail",
            "stop",
            0,
            StatusCode::OK,
        )
        .await;
        let config = ScreeningConfig {
            max_response_chars: 24,
            ..Default::default()
        };
        let report = screen_provider(
            &fixture.provider(),
            "fixture-model",
            &evidence(SubscriptionState::Active),
            &config,
        )
        .await;
        assert!(!report.eligible);
        assert_eq!(report.reason, ScreeningReason::AbnormalResponse);
        assert_eq!(report.response.as_deref(), Some("Unexpected [redacted] re"));
        assert!(report.response_truncated && report.response_redacted);
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("fixture_key"));
    }

    #[tokio::test]
    async fn exact_text_with_incomplete_termination_is_not_admitted() {
        let fixture = Fixture::new("ok", "length", 0, StatusCode::OK).await;
        let report = screen_provider(
            &fixture.provider(),
            "fixture-model",
            &evidence(SubscriptionState::Active),
            &ScreeningConfig::default(),
        )
        .await;
        assert_eq!(report.reason, ScreeningReason::AbnormalResponse);
        let captured: Value = serde_json::from_str(report.response.as_deref().unwrap()).unwrap();
        assert_eq!(captured["choices"][0]["finish_reason"], "length");
        assert_eq!(captured["choices"][0]["message"]["content"], "ok");
        assert!(!report.eligible);
    }

    #[tokio::test]
    async fn measured_latency_and_deadline_are_enforced_independently_of_provider_name() {
        let fixture = Fixture::new("ok", "stop", 100, StatusCode::OK).await;
        let config = ScreeningConfig {
            max_latency_ms: 25,
            probe_deadline_ms: 2_000,
            ..Default::default()
        };
        let slow = screen_provider(
            &fixture.provider(),
            "fixture-model",
            &evidence(SubscriptionState::Active),
            &config,
        )
        .await;
        assert_eq!(slow.reason, ScreeningReason::LatencyExceeded);
        assert!(!slow.eligible);
        assert!(slow.latency_ms.unwrap() >= 100);
        assert_eq!(slow.response.as_deref(), Some("ok"));
        let config = ScreeningConfig {
            max_latency_ms: 10,
            probe_deadline_ms: 25,
            ..Default::default()
        };
        let timed_out = screen_provider(
            &fixture.provider(),
            "fixture-model",
            &evidence(SubscriptionState::Active),
            &config,
        )
        .await;
        assert_eq!(timed_out.reason, ScreeningReason::Timeout);
        assert!(!timed_out.eligible);
        assert!(timed_out.latency_ms.unwrap() >= 25);
    }

    #[tokio::test]
    async fn http_failures_keep_classification_and_return_captured_probe_bodies() {
        for (status, reason) in [
            (StatusCode::UNAUTHORIZED, ScreeningReason::Authentication),
            (StatusCode::TOO_MANY_REQUESTS, ScreeningReason::RateLimited),
            (StatusCode::BAD_GATEWAY, ScreeningReason::ProviderError),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                ScreeningReason::ProviderError,
            ),
        ] {
            let fixture = Fixture::new("private upstream error", "stop", 0, status).await;
            let report = screen_provider(
                &fixture.provider(),
                "fixture-model",
                &evidence(SubscriptionState::Active),
                &ScreeningConfig::default(),
            )
            .await;
            assert_eq!(report.reason, reason);
            assert_eq!(report.upstream_status, Some(status.as_u16()));
            assert_eq!(report.upstream_code.as_deref(), Some("upstream_http_error"));
            assert!(report
                .response
                .as_deref()
                .is_some_and(|body| body.contains("private upstream error")));
            assert_eq!(fixture.requests.lock().unwrap().len(), 1);
        }
    }

    #[test]
    fn config_is_bounded_and_connection_refused_requires_explicit_transport_evidence() {
        assert_eq!(
            safe_upstream_code("secret fixture content"),
            "provider_error"
        );
        assert_eq!(
            safe_upstream_code("unknown_private_token"),
            "provider_error"
        );
        assert_eq!(
            safe_upstream_code("upstream_http_error"),
            "upstream_http_error"
        );
        assert!(ScreeningConfig::default().validate());
        assert!(!ScreeningConfig {
            max_latency_ms: 20_000,
            ..Default::default()
        }
        .validate());
        assert!(!ScreeningConfig {
            max_response_chars: MAX_REPORT_CHARS + 1,
            ..Default::default()
        }
        .validate());
        let refused = crate::proxy::canonical::make_provider_error(
            "upstream_transport_error",
            "connection_refused",
            None,
            None,
            None,
        );
        assert_eq!(classify_error(&refused), ScreeningReason::ConnectionRefused);
        let unknown = crate::proxy::canonical::make_provider_error(
            "upstream_transport_error",
            "connect",
            None,
            None,
            None,
        );
        assert_eq!(classify_error(&unknown), ScreeningReason::ConnectionFailed);
    }
}
