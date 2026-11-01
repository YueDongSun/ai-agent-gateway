//! Closed, metadata-only routing receipt contracts.

use serde::{Deserialize, Serialize};
use std::net::IpAddr;

pub const RECEIPT_VERSION: u16 = 1;
pub const MAX_ATTEMPTS: usize = 32;
pub const MAX_MODEL_BYTES: usize = 256;
pub const MAX_REFERENCE_BYTES: usize = 128;

const CREDENTIAL_PREFIXES: [&str; 5] = ["sk-", "ghp_", "github_pat_", "xoxb-", "xoxp-"];

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptPhase {
    Admitted,
    Routing,
    Streaming,
    Settled,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BodyState {
    Pending,
    Exhausted,
    Dropped,
    Error,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportState {
    Sending,
    HeadersReceived,
    ResponseParsed,
    StreamEnded,
    Failed,
    Abandoned,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValueState {
    Missing,
    Reported,
    Rejected,
    Conflicting,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssociationKind {
    SupplierEndpoint,
    ManualProvider,
    LegacyQueue,
    Unmapped,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    OpenaiChat,
    Anthropic,
    OpenaiResponses,
    Gemini,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialMode {
    StoredKey,
    ClientPassthrough,
    None,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    Model,
    MessageModel,
    ResponseModel,
    ModelVersion,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSignal {
    OpenaiFinish,
    OpenaiDone,
    AnthropicMessageStop,
    ResponsesCompleted,
    ResponsesIncomplete,
    ResponsesFailed,
    ResponsesCancelled,
    GeminiFinish,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttestationState {
    Unavailable,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoutingIdentity {
    pub provider_ref: Option<String>,
    pub supplier_ref: Option<String>,
    pub endpoint_ref: Option<String>,
    pub association: AssociationKind,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttemptRecord {
    pub index: u32,
    pub hop_index: u32,
    pub subattempt: u32,
    pub identity: RoutingIdentity,
    pub protocol: WireProtocol,
    pub credential_mode: CredentialMode,
    pub wire_model: Option<String>,
    pub wire_model_state: ValueState,
    pub provider_model: Option<String>,
    pub provider_model_state: ValueState,
    pub provider_model_source: Option<ModelSource>,
    pub http_status: Option<u16>,
    pub send_started: bool,
    pub transport_state: TransportState,
    pub terminal_signals: Vec<TerminalSignal>,
    pub error_code: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoutingReceipt {
    pub schema_version: u16,
    pub issuer: String,
    pub evidence_kind: String,
    pub server_instance_id: String,
    pub request_nonce: String,
    pub request_id: String,
    pub surface: String,
    pub generation: Option<u64>,
    pub requested_selector: Option<String>,
    pub selector_state: ValueState,
    pub phase: ReceiptPhase,
    pub http_status: Option<u16>,
    pub body_state: BodyState,
    pub selected_attempt: Option<u32>,
    pub attempts: Vec<AttemptRecord>,
    pub capture_complete: bool,
    pub capture_truncated: bool,
    pub upstream_model_attestation: AttestationState,
    pub issued_at_ms: u64,
    pub expires_at_ms: Option<u64>,
}

/// Matched against the exact immutable runtime snapshot used for dispatch.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RouteAdmission {
    pub server_instance_id: String,
    pub generation: u64,
    pub pinned_provider_present: bool,
}

/// V2 adds admission proof without changing the closed v1 routing envelope.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoutingReceiptV2 {
    pub schema_version: u16,
    pub issuer: String,
    pub admission: Option<RouteAdmission>,
    pub routing_receipt: RoutingReceipt,
}

pub fn safe_reference(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > MAX_REFERENCE_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || has_credential_prefix(value)
    {
        return None;
    }
    Some(value.to_owned())
}

pub fn safe_model(value: &str) -> Option<String> {
    let address_candidate = value
        .strip_prefix('[')
        .and_then(|address| address.strip_suffix(']'))
        .unwrap_or(value);
    if value.is_empty()
        || value.len() > MAX_MODEL_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'@' | b'[' | b']')
        })
        || value.starts_with('/')
        || value.contains("://")
        || has_credential_prefix(value)
        || address_candidate.parse::<IpAddr>().is_ok()
    {
        return None;
    }
    Some(value.to_owned())
}

fn has_credential_prefix(value: &str) -> bool {
    CREDENTIAL_PREFIXES.iter().any(|prefix| {
        value
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn references_preserve_bounded_identifiers_without_rewriting() {
        for value in ["provider_42", "Supplier-A1", "0", "_", "-"] {
            assert_eq!(safe_reference(value).as_deref(), Some(value));
        }
        let at_limit = "a".repeat(MAX_REFERENCE_BYTES);
        assert_eq!(
            safe_reference(&at_limit).as_deref(),
            Some(at_limit.as_str())
        );
        assert!(safe_reference(&"a".repeat(MAX_REFERENCE_BYTES + 1)).is_none());

        for value in [
            "",
            " provider",
            "provider ",
            "provider name",
            "provider\nextra",
            "provider\tname",
            "provider\0",
            "provider\u{7f}",
            "provider.name",
            "namespace/provider",
            "provider:42",
            "provider@42",
            "provider[1]",
            "prøvider",
            "provider\u{a0}",
        ] {
            assert!(
                safe_reference(value).is_none(),
                "accepted reference {value:?}"
            );
        }
    }

    #[test]
    fn models_preserve_namespaces_selectors_and_context_markers_exactly() {
        for value in [
            "model",
            "vendor/model-2.1",
            "precise:vendor/model-2.1[1M]",
            "alias:code_best",
            "vendor/model_2@preview:2026.10",
            "Model-X[1M]",
            "Model-X[1m]",
            "namespace/sk-model",
        ] {
            assert_eq!(safe_model(value).as_deref(), Some(value));
        }
        let at_limit = "m".repeat(MAX_MODEL_BYTES);
        assert_eq!(safe_model(&at_limit).as_deref(), Some(at_limit.as_str()));
        assert!(safe_model(&"m".repeat(MAX_MODEL_BYTES + 1)).is_none());
    }

    #[test]
    fn models_reject_unsafe_metadata_instead_of_trimming_or_substituting() {
        for value in [
            "",
            " model",
            "model ",
            "model name",
            "model\nextra",
            "model\rextra",
            "model\textra",
            "model\0",
            "model\u{7f}",
            "model\u{a0}",
            "mødel",
            "/namespace/model",
            "//model",
            "https://example.invalid/model",
            "model://extra",
            "namespace\\model",
            "model?secret=value",
            "model#extra",
            "model%2fextra",
            "127.0.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "2001:db8::1",
            "::ffff:192.0.2.1",
            "[::1]",
            "[2001:db8::1]",
        ] {
            assert!(safe_model(value).is_none(), "accepted model {value:?}");
        }
    }

    #[test]
    fn credential_prefixes_are_rejected_case_insensitively_by_both_helpers() {
        for value in [
            "sk-example",
            "SK-example",
            "Sk-example",
            "ghp_example",
            "GHP_example",
            "github_pat_example",
            "GitHub_Pat_example",
            "xoxb-example",
            "XOXB-example",
            "xoxp-example",
            "XoXp-example",
        ] {
            assert!(safe_reference(value).is_none());
            assert!(safe_model(value).is_none());
        }
    }

    fn representative_receipt_json() -> Value {
        json!({
            "schema_version": 1,
            "issuer": "aag",
            "evidence_kind": "gateway_observed",
            "server_instance_id": "abcdef0123456789abcdef0123456789",
            "request_nonce": "0123456789abcdef0123456789abcdef",
            "request_id": "request_42",
            "surface": "chat",
            "generation": 7,
            "requested_selector": "precise:vendor/model-2[1M]",
            "selector_state": "reported",
            "phase": "settled",
            "http_status": 200,
            "body_state": "exhausted",
            "selected_attempt": 2,
            "attempts": [
                {
                    "index": 1,
                    "hop_index": 0,
                    "subattempt": 0,
                    "identity": {
                        "provider_ref": "provider_1",
                        "supplier_ref": null,
                        "endpoint_ref": null,
                        "association": "manual_provider"
                    },
                    "protocol": "openai_chat",
                    "credential_mode": "stored_key",
                    "wire_model": "vendor/model-2",
                    "wire_model_state": "reported",
                    "provider_model": null,
                    "provider_model_state": "missing",
                    "provider_model_source": null,
                    "http_status": 503,
                    "send_started": true,
                    "transport_state": "failed",
                    "terminal_signals": [],
                    "error_code": "upstream_http_error"
                },
                {
                    "index": 2,
                    "hop_index": 1,
                    "subattempt": 0,
                    "identity": {
                        "provider_ref": "provider_2",
                        "supplier_ref": "supplier_1",
                        "endpoint_ref": "endpoint_2",
                        "association": "supplier_endpoint"
                    },
                    "protocol": "openai_chat",
                    "credential_mode": "stored_key",
                    "wire_model": "vendor/model-2",
                    "wire_model_state": "reported",
                    "provider_model": "model-2",
                    "provider_model_state": "reported",
                    "provider_model_source": "model",
                    "http_status": 200,
                    "send_started": true,
                    "transport_state": "stream_ended",
                    "terminal_signals": ["openai_finish", "openai_done"],
                    "error_code": null
                }
            ],
            "capture_complete": true,
            "capture_truncated": false,
            "upstream_model_attestation": "unavailable",
            "issued_at_ms": 1700000000000_u64,
            "expires_at_ms": 1700000060000_u64
        })
    }

    #[test]
    fn representative_receipt_round_trip_preserves_the_complete_metadata_envelope() {
        let receipt_json = representative_receipt_json();
        let receipt: RoutingReceipt =
            serde_json::from_value(receipt_json.clone()).expect("valid receipt fixture");
        let encoded = serde_json::to_value(&receipt).expect("serialize receipt");
        assert_eq!(encoded, receipt_json);
        let decoded: RoutingReceipt = serde_json::from_value(encoded).expect("decode receipt");
        assert!(receipt == decoded);
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_metadata_object_boundary() {
        for pointer in ["", "/attempts/1", "/attempts/1/identity"] {
            let mut receipt_json = representative_receipt_json();
            receipt_json
                .pointer_mut(pointer)
                .expect("fixture object exists")
                .as_object_mut()
                .expect("fixture value is an object")
                .insert("unexpected_metadata".into(), json!("not_retained"));
            assert!(
                serde_json::from_value::<RoutingReceipt>(receipt_json).is_err(),
                "accepted unknown field at {pointer}"
            );
        }
    }

    #[test]
    fn unknown_enum_values_are_rejected_instead_of_becoming_evidence() {
        for pointer in [
            "/phase",
            "/body_state",
            "/selector_state",
            "/upstream_model_attestation",
            "/attempts/1/identity/association",
            "/attempts/1/protocol",
            "/attempts/1/credential_mode",
            "/attempts/1/provider_model_source",
            "/attempts/1/transport_state",
            "/attempts/1/terminal_signals/0",
        ] {
            let mut receipt_json = representative_receipt_json();
            *receipt_json
                .pointer_mut(pointer)
                .expect("fixture enum value exists") = json!("unrecognized_variant");
            assert!(
                serde_json::from_value::<RoutingReceipt>(receipt_json).is_err(),
                "accepted unknown enum value at {pointer}"
            );
        }
    }
}
