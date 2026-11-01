use super::contract::*;
use crate::proxy::canonical::ProviderError;
use std::future::Future;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

tokio::task_local! {
    static REQUEST_OBSERVATION: RequestObservation;
}

#[derive(Clone)]
pub(crate) struct RequestObservation(Arc<Mutex<RequestState>>);

struct RequestState {
    receipt: RoutingReceipt,
    expected_admission: Option<RouteAdmission>,
    admission: Option<RouteAdmission>,
    admitted_at: Instant,
    settled_at: Option<Instant>,
}

impl RequestObservation {
    pub(crate) fn new(receipt: RoutingReceipt, now: Instant) -> Self {
        Self::with_precondition(receipt, now, None)
    }

    pub(crate) fn with_precondition(
        receipt: RoutingReceipt,
        now: Instant,
        expected_admission: Option<RouteAdmission>,
    ) -> Self {
        Self(Arc::new(Mutex::new(RequestState {
            receipt,
            expected_admission,
            admission: None,
            admitted_at: now,
            settled_at: None,
        })))
    }

    pub(crate) fn snapshot(&self) -> RoutingReceipt {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::routing_snapshot(&state)
    }

    pub(crate) fn snapshot_v2(&self) -> RoutingReceiptV2 {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        RoutingReceiptV2 {
            schema_version: 2,
            issuer: "aag".into(),
            admission: state.admission.clone(),
            routing_receipt: Self::routing_snapshot(&state),
        }
    }

    fn check_admission(&self, snapshot: Option<(u64, bool)>) -> Result<(), &'static str> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(expected) = state.expected_admission.as_ref() else {
            return Ok(());
        };
        let Some((generation, pinned_provider_present)) =
            snapshot.filter(|(generation, _)| *generation > 0)
        else {
            return Err("receipt_route_snapshot_unavailable");
        };
        if state.settled_at.is_some()
            || expected.server_instance_id != state.receipt.server_instance_id
            || expected.generation != generation
            || expected.pinned_provider_present != pinned_provider_present
        {
            return Err("receipt_route_precondition_failed");
        }
        state.admission = Some(expected.clone());
        state.receipt.generation = Some(generation);
        Ok(())
    }

    fn routing_snapshot(state: &RequestState) -> RoutingReceipt {
        let mut receipt = state.receipt.clone();
        receipt.capture_complete = receipt.phase == ReceiptPhase::Settled
            && !receipt.capture_truncated
            && receipt.selector_state == ValueState::Reported
            && !receipt.attempts.is_empty()
            && matches!(receipt.body_state, BodyState::Exhausted | BodyState::Error)
            && receipt.attempts.iter().all(|attempt| {
                attempt.wire_model_state == ValueState::Reported
                    && attempt.identity.provider_ref.is_some()
                    && attempt.protocol != WireProtocol::Unknown
                    && match attempt.transport_state {
                        TransportState::ResponseParsed | TransportState::Failed => true,
                        TransportState::StreamEnded => !attempt.terminal_signals.is_empty(),
                        _ => false,
                    }
            });
        receipt
    }

    pub(crate) fn expired(&self, now: Instant) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .settled_at
            .is_some_and(|settled| {
                now.saturating_duration_since(settled)
                    >= Duration::from_secs(super::RETENTION_SECONDS)
            })
    }

    pub(crate) fn finish(&self, status: Option<u16>, body: BodyState) {
        self.finish_at(status, body, Instant::now());
    }

    fn finish_at(&self, status: Option<u16>, body: BodyState, now: Instant) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.settled_at.is_some() {
            return;
        }
        let elapsed = now
            .saturating_duration_since(state.admitted_at)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        state.receipt.phase = ReceiptPhase::Settled;
        state.receipt.http_status = status;
        state.receipt.body_state = body;
        state.receipt.expires_at_ms = Some(
            state
                .receipt
                .issued_at_ms
                .saturating_add(elapsed)
                .saturating_add(super::RETENTION_SECONDS * 1000),
        );
        state.settled_at = Some(now);
    }

    pub(crate) fn hop(
        &self,
        hop_index: usize,
        identity: RoutingIdentity,
        protocol: &str,
    ) -> ProviderObservation {
        let Ok(hop_index) = u32::try_from(hop_index) else {
            return ProviderObservation::default();
        };
        let protocol = match protocol {
            "openai-chat" => WireProtocol::OpenaiChat,
            "anthropic" => WireProtocol::Anthropic,
            "openai-responses" => WireProtocol::OpenaiResponses,
            "gemini" => WireProtocol::Gemini,
            _ => WireProtocol::Unknown,
        };
        ProviderObservation(Some(Arc::new(HopObservation {
            request: self.clone(),
            hop_index,
            identity,
            protocol,
            next_subattempt: AtomicU32::new(0),
        })))
    }

    fn selector(&self, value: &str) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.settled_at.is_some() {
            return;
        }
        state.receipt.requested_selector = safe_model(value);
        state.receipt.selector_state = if state.receipt.requested_selector.is_some() {
            ValueState::Reported
        } else {
            ValueState::Rejected
        };
    }

    fn generation(&self, value: u64) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.settled_at.is_none() && state.receipt.generation.is_none() {
            state.receipt.generation = Some(value);
        }
    }
}

pub(crate) fn current() -> Option<RequestObservation> {
    REQUEST_OBSERVATION.try_with(Clone::clone).ok()
}

pub(crate) async fn scope<T>(
    observation: RequestObservation,
    future: impl Future<Output = T>,
) -> T {
    REQUEST_OBSERVATION.scope(observation, future).await
}

pub(crate) fn check_admission(snapshot: Option<(u64, bool)>) -> Result<(), &'static str> {
    match current() {
        Some(observation) => observation.check_admission(snapshot),
        None => Ok(()),
    }
}

pub(crate) fn observe_selector(value: &str) {
    if let Some(observation) = current() {
        observation.selector(value);
    }
}
pub(crate) fn observe_generation(value: u64) {
    if let Some(observation) = current() {
        observation.generation(value);
    }
}

struct HopObservation {
    request: RequestObservation,
    hop_index: u32,
    identity: RoutingIdentity,
    protocol: WireProtocol,
    next_subattempt: AtomicU32,
}

/// Explicit, cloneable context for one admitted hop. No provider-global state.
#[derive(Clone, Default)]
pub struct ProviderObservation(Option<Arc<HopObservation>>);

impl std::fmt::Debug for ProviderObservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProviderObservation { metadata: redacted }")
    }
}

impl ProviderObservation {
    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Call after local construction succeeds, immediately before submission.
    /// Metadata exhaustion cannot change the existing inference policy.
    pub fn begin_wire(
        &self,
        wire_model: &str,
        credential_mode: CredentialMode,
    ) -> Result<WireAttempt, ProviderError> {
        let Some(hop) = &self.0 else {
            return Ok(WireAttempt {
                binding: None,
                finalized: false,
            });
        };
        let mut state = hop
            .request
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.settled_at.is_some() || state.receipt.attempts.len() >= MAX_ATTEMPTS {
            state.receipt.capture_truncated = true;
            state.receipt.selected_attempt = None;
            return Ok(WireAttempt {
                binding: None,
                finalized: false,
            });
        }
        let wire_model = safe_model(wire_model);
        let wire_model_state = if wire_model.is_some() {
            ValueState::Reported
        } else {
            ValueState::Rejected
        };
        let index = u32::try_from(state.receipt.attempts.len() + 1).expect("bounded attempt count");
        let subattempt = hop.next_subattempt.fetch_add(1, Ordering::Relaxed);
        state.receipt.attempts.push(AttemptRecord {
            index,
            hop_index: hop.hop_index,
            subattempt,
            identity: hop.identity.clone(),
            protocol: hop.protocol,
            credential_mode,
            wire_model,
            wire_model_state,
            provider_model: None,
            provider_model_state: ValueState::Missing,
            provider_model_source: None,
            http_status: None,
            send_started: true,
            transport_state: TransportState::Sending,
            terminal_signals: Vec::new(),
            error_code: None,
        });
        state.receipt.phase = ReceiptPhase::Routing;
        Ok(WireAttempt {
            binding: Some((hop.request.clone(), index)),
            finalized: false,
        })
    }

    pub(crate) fn selected(&self, streaming: bool) {
        let Some(hop) = &self.0 else {
            return;
        };
        let mut state = hop
            .request
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.settled_at.is_some() {
            return;
        }
        state.receipt.selected_attempt = if state.receipt.capture_truncated {
            None
        } else {
            state
                .receipt
                .attempts
                .iter()
                .rev()
                .find(|attempt| attempt.hop_index == hop.hop_index)
                .map(|attempt| attempt.index)
        };
        if streaming {
            state.receipt.phase = ReceiptPhase::Streaming;
        }
    }

    pub(crate) fn failed(&self, code: &str) {
        let Some(hop) = &self.0 else {
            return;
        };
        let mut state = hop
            .request
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(attempt) = state
            .receipt
            .attempts
            .iter_mut()
            .rev()
            .find(|attempt| attempt.hop_index == hop.hop_index)
        {
            if attempt.transport_state != TransportState::ResponseParsed
                && !(attempt.transport_state == TransportState::StreamEnded
                    && !attempt.terminal_signals.is_empty())
            {
                attempt.transport_state = TransportState::Failed;
                attempt.error_code = Some(safe_error_code(code).into());
            }
        }
    }
}

/// Exactly one owner finalizes a real send. Moving this guard into a stream
/// covers unread streams and drop before their first poll.
pub struct WireAttempt {
    binding: Option<(RequestObservation, u32)>,
    finalized: bool,
}

impl std::fmt::Debug for WireAttempt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WireAttempt { metadata: redacted }")
    }
}

pub struct WireResponse {
    pub response: reqwest::Response,
    pub attempt: WireAttempt,
}

impl WireAttempt {
    pub fn enabled(&self) -> bool {
        self.binding.is_some()
    }

    fn change(&self, update: impl FnOnce(&mut AttemptRecord)) {
        let Some((request, index)) = &self.binding else {
            return;
        };
        let mut state = request
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(record) = state.receipt.attempts.get_mut((*index - 1) as usize) {
            update(record);
        }
    }

    pub fn response_status(&self, status: u16) {
        self.change(|record| {
            record.http_status = Some(status);
            record.transport_state = TransportState::HeadersReceived;
        });
    }

    pub fn reported_model(&self, source: ModelSource, value: Option<&str>) {
        let Some(value) = value else {
            return;
        };
        self.change(|record| {
            if matches!(
                record.provider_model_state,
                ValueState::Rejected | ValueState::Conflicting
            ) {
                return;
            }
            let Some(value) = safe_model(value) else {
                record.provider_model = None;
                record.provider_model_state = ValueState::Rejected;
                record.provider_model_source = Some(source);
                return;
            };
            if record
                .provider_model
                .as_ref()
                .is_some_and(|previous| previous != &value)
                || record
                    .provider_model_source
                    .as_ref()
                    .is_some_and(|previous| previous != &source)
            {
                record.provider_model = None;
                record.provider_model_state = ValueState::Conflicting;
            } else {
                record.provider_model = Some(value);
                record.provider_model_state = ValueState::Reported;
                record.provider_model_source = Some(source);
            }
        });
    }

    pub fn rejected_model(&self, source: ModelSource) {
        self.change(|record| {
            record.provider_model = None;
            record.provider_model_state = ValueState::Rejected;
            record.provider_model_source = Some(source);
        });
    }

    pub fn terminal(&self, signal: TerminalSignal) {
        self.change(|record| {
            if !record.terminal_signals.contains(&signal) {
                record.terminal_signals.push(signal);
            }
        });
    }

    pub fn response_parsed(&mut self) {
        self.change(|record| record.transport_state = TransportState::ResponseParsed);
        self.finalized = true;
    }

    pub fn stream_ended(&mut self) {
        self.change(|record| record.transport_state = TransportState::StreamEnded);
        self.finalized = true;
    }

    pub fn failed(&mut self, code: &str) {
        self.change(|record| {
            record.transport_state = TransportState::Failed;
            record.error_code = Some(safe_error_code(code).into());
        });
        self.finalized = true;
    }
}

impl Drop for WireAttempt {
    fn drop(&mut self) {
        if !self.finalized {
            self.change(|record| {
                if matches!(
                    record.transport_state,
                    TransportState::Sending | TransportState::HeadersReceived
                ) {
                    record.transport_state = TransportState::Abandoned;
                }
            });
        }
    }
}

fn safe_error_code(value: &str) -> &'static str {
    match value {
        "upstream_http_error" => "upstream_http_error",
        "upstream_timeout" => "upstream_timeout",
        "upstream_transport_error" => "upstream_transport_error",
        "upstream_decode_error" => "upstream_decode_error",
        "proxy_stream_error" | "upstream_stream_error" => "upstream_stream_error",
        "upstream_error" => "upstream_error",
        "routing_receipt_attempt_limit" => "routing_receipt_attempt_limit",
        _ => "provider_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::routing_receipts::{ReceiptRegistry, ReceiptSecret};

    fn request() -> RequestObservation {
        ReceiptRegistry::new()
            .unwrap()
            .reserve(
                "11111111111111111111111111111111",
                &ReceiptSecret::fixture(1),
                "req_fixture",
                "chat",
            )
            .unwrap()
    }
    fn identity() -> RoutingIdentity {
        RoutingIdentity {
            provider_ref: Some("provider_a".into()),
            supplier_ref: None,
            endpoint_ref: None,
            association: AssociationKind::ManualProvider,
        }
    }

    #[test]
    fn attempts_are_request_owned_and_drop_is_not_completion() {
        let first = request();
        let second = request();
        first.selector("alias:fast");
        second.selector("alias:slow");
        let one = first.hop(0, identity(), "openai-chat");
        let two = second.hop(0, identity(), "openai-chat");
        let a = one.begin_wire("wire_a", CredentialMode::StoredKey).unwrap();
        let mut b = two.begin_wire("wire_b", CredentialMode::None).unwrap();
        a.reported_model(ModelSource::Model, Some("reported_a"));
        b.reported_model(ModelSource::Model, Some("reported_b"));
        b.response_parsed();
        drop(a);
        assert_eq!(
            first.snapshot().attempts[0].transport_state,
            TransportState::Abandoned
        );
        assert_eq!(
            second.snapshot().attempts[0].transport_state,
            TransportState::ResponseParsed
        );
        assert_eq!(
            first.snapshot().attempts[0].provider_model.as_deref(),
            Some("reported_a")
        );
    }

    #[test]
    fn compatibility_sends_and_hops_have_distinct_ordinals() {
        let request = request();
        let hop = request.hop(3, identity(), "anthropic");
        let mut first = hop.begin_wire("wire", CredentialMode::StoredKey).unwrap();
        first.response_status(400);
        first.failed("upstream_http_error");
        let mut retry = hop.begin_wire("wire", CredentialMode::StoredKey).unwrap();
        retry.response_status(503);
        retry.failed("upstream_http_error");
        let next = request.hop(4, identity(), "gemini");
        let mut third = next.begin_wire("other", CredentialMode::StoredKey).unwrap();
        third.response_parsed();
        next.selected(false);
        let view = request.snapshot();
        assert_eq!(
            view.attempts
                .iter()
                .map(|a| (a.index, a.hop_index, a.subattempt))
                .collect::<Vec<_>>(),
            vec![(1, 3, 0), (2, 3, 1), (3, 4, 0)]
        );
        assert_eq!(view.selected_attempt, Some(3));
    }

    #[test]
    fn eof_alone_and_conflicting_model_claims_do_not_become_complete_evidence() {
        let request = request();
        request.selector("alias:fast");
        let hop = request.hop(0, identity(), "openai-chat");
        let mut wire = hop.begin_wire("wire", CredentialMode::StoredKey).unwrap();
        wire.reported_model(ModelSource::Model, Some("first"));
        wire.reported_model(ModelSource::Model, Some("second"));
        wire.stream_ended();
        hop.selected(true);
        request.finish(Some(200), BodyState::Exhausted);
        let view = request.snapshot();
        assert_eq!(
            view.attempts[0].provider_model_state,
            ValueState::Conflicting
        );
        assert_eq!(view.attempts[0].provider_model, None);
        assert!(!view.capture_complete);
    }

    #[test]
    fn exhausted_metadata_does_not_block_transport_or_select_a_stale_attempt() {
        let request = request();
        request.selector("model");
        let hop = request.hop(0, identity(), "openai-chat");
        for _ in 0..MAX_ATTEMPTS {
            let mut wire = hop.begin_wire("model", CredentialMode::None).unwrap();
            wire.response_parsed();
        }
        hop.selected(false);
        let mut unrecorded = hop
            .begin_wire("model", CredentialMode::None)
            .expect("metadata does not deny transport");
        assert!(!unrecorded.enabled());
        unrecorded.response_parsed();
        hop.selected(false);
        request.finish(Some(200), BodyState::Exhausted);
        let view = request.snapshot();
        assert_eq!(view.attempts.len(), MAX_ATTEMPTS);
        assert!(view.capture_truncated);
        assert!(!view.capture_complete);
        assert_eq!(view.selected_attempt, None);
    }
}
