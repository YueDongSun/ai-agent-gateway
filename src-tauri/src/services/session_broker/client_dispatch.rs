//! Source-bound client policy orchestration. Native payloads remain opaque.
//!
//! The service establishes the owner and physical source outside this module.
//! Backends retain native process and ticket ownership; clients receive neither
//! a writer permit nor a native request key. No protocol bodies are logged.

use super::local_transport::{EnvelopeKind, TransportFrame};
use super::native_core::{FrameKind, NativeFrame, RawJson, RpcId, ServerReply};
use super::policy::{classify_request, Operation};
use super::source_catalog::{SourceBinding, SourceKey, SourceMode};
use super::{
    BrokerDecision, BrokerError, CanonicalSessionId, ClientRequest, NativeThreadId,
    SessionRegistry, ThreadReference, VerifiedOwner, WritePermit,
};
use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    fmt,
};

const FRAME_BYTES: usize = 32 * 1024 * 1024;
const MAX_SERVER_ASSOCIATIONS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ClientTag(String);
impl ClientTag {
    pub fn new(value: impl Into<String>) -> Result<Self, DispatchError> {
        let value = value.into();
        super::validate_identifier(&value)?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A catalog observation, never deserialized from the client envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceGeneration {
    pub source_key: SourceKey,
    pub canonical_session_id: CanonicalSessionId,
    pub physical_generation: u64,
    pub mode: SourceMode,
}
impl SourceGeneration {
    pub fn from_protected_catalog(binding: &SourceBinding) -> Result<Self, DispatchError> {
        if binding.physical_generation == 0 {
            return Err(DispatchError::SourceMismatch);
        }
        Ok(Self {
            source_key: binding.source_key.clone(),
            canonical_session_id: binding.canonical_session_id.clone(),
            physical_generation: binding.physical_generation,
            mode: binding.mode,
        })
    }
}

/// Minted per admission. An RPC field cannot construct or select this token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WriteOperation {
    instance: String,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackendError {
    Timeout,
    Disconnected,
    Unavailable,
    InvalidResponse,
    SourceMismatch,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DispatchError {
    #[error("client_dispatch_policy_denied")]
    Policy(#[from] BrokerError),
    #[error("client_dispatch_invalid_envelope")]
    InvalidEnvelope,
    #[error("client_dispatch_source_mismatch")]
    SourceMismatch,
    #[error("client_dispatch_unknown_server_request")]
    UnknownServerRequest,
    #[error("client_dispatch_invalid_native_evidence")]
    InvalidNativeEvidence,
    #[error("client_dispatch_backend_failed")]
    Backend(BackendError),
}
impl From<BackendError> for DispatchError {
    fn from(value: BackendError) -> Self {
        Self::Backend(value)
    }
}

pub(crate) enum BackendReply {
    Result(RawJson),
    Error(RawJson),
}
pub(crate) enum WriteOutcome {
    /// The backend verified source-bound terminal/idle evidence. A successful
    /// transport response alone does not justify this variant.
    Finished(BackendReply),
    Running {
        native_turn_id: String,
        reply: BackendReply,
    },
    OutcomeUnknown,
}

pub(crate) struct BackendEvent<K> {
    pub source: SourceGeneration,
    pub kind: BackendEventKind<K>,
}
pub(crate) enum BackendEventKind<K> {
    Notification(NativeFrame),
    ServerRequest {
        native_key: K,
        frame: NativeFrame,
    },
    WriteReconciled {
        operation: WriteOperation,
        native_thread_id: NativeThreadId,
        outcome: WriteOutcome,
    },
}

/// Implemented by the service actor. Only this owner can produce native keys.
/// Every operation is bounded by that actor; no method may log payload bodies.
pub(crate) trait DispatchBackend {
    type ServerRequestKey: Clone;
    fn verify_source(&mut self, source: &SourceGeneration) -> Result<(), BackendError>;
    fn metadata(
        &mut self,
        source: &SourceGeneration,
        method: &str,
        params: &RawJson,
    ) -> Result<BackendReply, BackendError>;
    fn project_history(
        &mut self,
        source: &SourceGeneration,
        reference: &ThreadReference,
        method: &str,
        params: &RawJson,
    ) -> Result<BackendReply, BackendError>;
    fn write(
        &mut self,
        source: &SourceGeneration,
        reference: &ThreadReference,
        operation: &WriteOperation,
        method: &str,
        params: &RawJson,
    ) -> Result<WriteOutcome, BackendError>;
    fn inflight(
        &mut self,
        source: &SourceGeneration,
        reference: &ThreadReference,
        operation: &WriteOperation,
        method: &str,
        params: &RawJson,
    ) -> Result<BackendReply, BackendError>;
    fn reply_to_server(
        &mut self,
        source: &SourceGeneration,
        native_key: &Self::ServerRequestKey,
        reply: ServerReply,
    ) -> Result<(), BackendError>;
    fn poll_event(&mut self) -> Result<Option<BackendEvent<Self::ServerRequestKey>>, BackendError>;
}

pub(crate) enum DispatchResult {
    Response(TransportFrame),
    NotificationAccepted,
    OutcomeUnknown,
}
pub(crate) struct ClientDelivery {
    pub client: ClientTag,
    pub frame: TransportFrame,
}

struct ActiveWrite {
    permit: WritePermit,
    operation: WriteOperation,
    client: ClientTag,
    request_id: RpcId,
    native_turn_id: Option<String>,
}
struct ServerAssociation<K> {
    native_key: K,
    operation: WriteOperation,
    native_thread_id: NativeThreadId,
    native_turn_id: String,
    uncertain_reply: bool,
}

/// One serialized dispatcher owns one physical source generation.
pub(crate) struct ClientDispatcher<B: DispatchBackend> {
    registry: SessionRegistry,
    owner: VerifiedOwner,
    source: SourceGeneration,
    backend: B,
    instance: String,
    next_operation: u64,
    active: Option<ActiveWrite>,
    server_requests: HashMap<(ClientTag, RpcId), ServerAssociation<B::ServerRequestKey>>,
}
impl<B: DispatchBackend> ClientDispatcher<B> {
    pub fn new(
        registry: SessionRegistry,
        owner: VerifiedOwner,
        source: SourceGeneration,
        backend: B,
    ) -> Result<Self, DispatchError> {
        let mut identity = [0u8; 16];
        getrandom::getrandom(&mut identity).map_err(|_| BrokerError::RandomIdentity)?;
        Ok(Self {
            registry,
            owner,
            source,
            backend,
            instance: hex::encode(identity),
            next_operation: 0,
            active: None,
            server_requests: HashMap::new(),
        })
    }

    pub fn dispatch(
        &mut self,
        client: &ClientTag,
        frame: &TransportFrame,
    ) -> Result<DispatchResult, DispatchError> {
        self.verify_binding()?;
        let envelope = EnvelopeRouting::parse(frame.bytes())?;
        match frame.kind() {
            EnvelopeKind::Response => self.dispatch_reply(client, frame, envelope),
            EnvelopeKind::Notification => {
                if envelope.method.as_deref() != Some("initialized")
                    || raw_params(frame.bytes())?.as_str() != "{}"
                {
                    return Err(DispatchError::InvalidEnvelope);
                }
                // Each facade completes its own handshake. This never sends a
                // second initialized notification to the shared native core.
                Ok(DispatchResult::NotificationAccepted)
            }
            EnvelopeKind::Request => self.dispatch_request(client, frame, envelope),
        }
    }

    pub fn poll(&mut self) -> Result<Vec<ClientDelivery>, DispatchError> {
        self.verify_binding()?;
        let Some(event) = self.backend.poll_event()? else {
            return Ok(Vec::new());
        };
        if event.source != self.source {
            return Err(DispatchError::SourceMismatch);
        }
        match event.kind {
            BackendEventKind::Notification(frame) => self.native_notification(frame),
            BackendEventKind::ServerRequest { native_key, frame } => {
                self.native_server_request(native_key, frame)
            }
            BackendEventKind::WriteReconciled {
                operation,
                native_thread_id,
                outcome,
            } => {
                let active = self
                    .active
                    .as_ref()
                    .ok_or(DispatchError::InvalidNativeEvidence)?;
                if active.operation != operation
                    || active.permit.reference.native_thread_id != native_thread_id
                {
                    return Err(DispatchError::InvalidNativeEvidence);
                }
                self.registry.validate_write_permit(&active.permit)?;
                let client = active.client.clone();
                match self.apply_write_outcome(outcome)? {
                    DispatchResult::Response(frame) => Ok(vec![ClientDelivery { client, frame }]),
                    _ => Ok(Vec::new()),
                }
            }
        }
    }

    fn verify_binding(&mut self) -> Result<(), DispatchError> {
        if self.source.physical_generation == 0 {
            return Err(DispatchError::SourceMismatch);
        }
        self.backend.verify_source(&self.source)?;
        let snapshot = self
            .registry
            .session_snapshot(&self.owner, &self.source.canonical_session_id)?;
        if snapshot.read_only != (self.source.mode == SourceMode::ReadOnly) {
            return Err(DispatchError::SourceMismatch);
        }
        Ok(())
    }

    fn dispatch_request(
        &mut self,
        client: &ClientTag,
        frame: &TransportFrame,
        envelope: EnvelopeRouting,
    ) -> Result<DispatchResult, DispatchError> {
        let id = envelope.id.ok_or(DispatchError::InvalidEnvelope)?;
        let method = envelope.method.ok_or(DispatchError::InvalidEnvelope)?;
        super::validate_identifier(&method)?;
        let params = envelope.params.unwrap_or_default().policy;
        let request = ClientRequest {
            method: &method,
            params: &params,
        };
        let operation = classify_request(&request)?;
        if operation != Operation::ReadGlobal {
            let thread_id = requested_thread(&params)?;
            let reference = self.registry.resolve_thread(&self.owner, &thread_id)?;
            if reference.canonical_session_id != self.source.canonical_session_id {
                return Err(DispatchError::SourceMismatch);
            }
        }
        let raw_params = raw_params(frame.bytes())?;
        if matches!(method.as_str(), "turn/steer" | "turn/interrupt") {
            return self.dispatch_inflight(client, id, &method, &params, &raw_params);
        }
        // Reserve the opaque token sequence before SQL admission so exhaustion
        // cannot strand a permit outside this actor's active record.
        let next_operation = self
            .next_operation
            .checked_add(1)
            .ok_or(DispatchError::InvalidNativeEvidence)?;
        match self.registry.assess_client_request(&self.owner, request)? {
            BrokerDecision::Read { reference: None } => {
                let reply = self.backend.metadata(&self.source, &method, &raw_params)?;
                Ok(DispatchResult::Response(client_response(&id, reply)?))
            }
            BrokerDecision::Read {
                reference: Some(reference),
            }
            | BrokerDecision::ReadProjection { reference } => {
                let reply =
                    self.backend
                        .project_history(&self.source, &reference, &method, &raw_params)?;
                Ok(DispatchResult::Response(client_response(&id, reply)?))
            }
            BrokerDecision::Write(permit) => {
                self.next_operation = next_operation;
                self.dispatch_write(client, id, method, raw_params, permit)
            }
        }
    }

    fn dispatch_write(
        &mut self,
        client: &ClientTag,
        id: RpcId,
        method: String,
        params: RawJson,
        permit: WritePermit,
    ) -> Result<DispatchResult, DispatchError> {
        // The SQL admission remains authoritative even if the actor or caller
        // disappears. Failures after this point retain it for reconciliation.
        let operation = WriteOperation {
            instance: self.instance.clone(),
            sequence: self.next_operation,
        };
        let reference = permit.reference.clone();
        self.active = Some(ActiveWrite {
            permit,
            operation: operation.clone(),
            client: client.clone(),
            request_id: id,
            native_turn_id: None,
        });
        self.registry
            .validate_write_permit(&self.active.as_ref().unwrap().permit)?;
        let outcome = self
            .backend
            .write(&self.source, &reference, &operation, &method, &params)?;
        self.apply_write_outcome(outcome)
    }

    fn apply_write_outcome(
        &mut self,
        outcome: WriteOutcome,
    ) -> Result<DispatchResult, DispatchError> {
        let active = self
            .active
            .as_mut()
            .ok_or(DispatchError::InvalidNativeEvidence)?;
        match outcome {
            WriteOutcome::OutcomeUnknown => Ok(DispatchResult::OutcomeUnknown),
            WriteOutcome::Running {
                native_turn_id,
                reply,
            } => {
                super::validate_identifier(&native_turn_id)
                    .map_err(|_| DispatchError::InvalidNativeEvidence)?;
                if active
                    .native_turn_id
                    .as_ref()
                    .is_some_and(|current| current != &native_turn_id)
                {
                    return Err(DispatchError::InvalidNativeEvidence);
                }
                active.native_turn_id = Some(native_turn_id);
                Ok(DispatchResult::Response(client_response(
                    &active.request_id,
                    reply,
                )?))
            }
            WriteOutcome::Finished(reply) => {
                // After a turn was admitted, only its exact terminal native
                // event can retire it. Finished is for verified idle outcomes
                // such as a completed resume or a synchronous metadata write.
                if active.native_turn_id.is_some() {
                    return Err(DispatchError::InvalidNativeEvidence);
                }
                let response = client_response(&active.request_id, reply)?;
                self.finish_active()?;
                Ok(DispatchResult::Response(response))
            }
        }
    }

    fn dispatch_inflight(
        &mut self,
        client: &ClientTag,
        id: RpcId,
        method: &str,
        params: &Value,
        raw_params: &RawJson,
    ) -> Result<DispatchResult, DispatchError> {
        let active = self
            .active
            .as_ref()
            .ok_or(DispatchError::InvalidNativeEvidence)?;
        let expected_turn = if method == "turn/steer" {
            "expectedTurnId"
        } else {
            "turnId"
        };
        if &active.client != client
            || active.native_turn_id.as_deref() != params.get(expected_turn).and_then(Value::as_str)
            || active.native_turn_id.is_none()
        {
            return Err(DispatchError::InvalidNativeEvidence);
        }
        self.registry.authorize_inflight_request(
            &self.owner,
            &active.permit.reference.native_thread_id,
            ClientRequest { method, params },
            &active.permit,
        )?;
        let reply = self.backend.inflight(
            &self.source,
            &active.permit.reference,
            &active.operation,
            method,
            raw_params,
        )?;
        Ok(DispatchResult::Response(client_response(&id, reply)?))
    }

    fn dispatch_reply(
        &mut self,
        client: &ClientTag,
        frame: &TransportFrame,
        envelope: EnvelopeRouting,
    ) -> Result<DispatchResult, DispatchError> {
        let id = envelope.id.ok_or(DispatchError::InvalidEnvelope)?;
        let association = self
            .server_requests
            .get_mut(&(client.clone(), id.clone()))
            .ok_or(DispatchError::UnknownServerRequest)?;
        if association.uncertain_reply {
            return Err(DispatchError::UnknownServerRequest);
        }
        let active = self
            .active
            .as_ref()
            .ok_or(DispatchError::UnknownServerRequest)?;
        if active.operation != association.operation
            || active.native_turn_id.as_deref() != Some(association.native_turn_id.as_str())
        {
            return Err(DispatchError::UnknownServerRequest);
        }
        self.registry.authorize_tool_reply(
            &self.owner,
            &association.native_thread_id,
            &active.permit,
        )?;
        let raw = std::str::from_utf8(frame.bytes()).map_err(|_| DispatchError::InvalidEnvelope)?;
        let reply = if let Some(body) = raw_member(raw, "result")? {
            ServerReply::Result(parse_raw(body)?)
        } else {
            ServerReply::Error(parse_raw(
                raw_member(raw, "error")?.ok_or(DispatchError::InvalidEnvelope)?,
            )?)
        };
        // Once handed to the native owner, uncertain transport delivery cannot
        // be retried by replaying the same client response.
        association.uncertain_reply = true;
        self.backend
            .reply_to_server(&self.source, &association.native_key, reply)?;
        self.server_requests.remove(&(client.clone(), id));
        Ok(DispatchResult::NotificationAccepted)
    }

    fn native_server_request(
        &mut self,
        native_key: B::ServerRequestKey,
        frame: NativeFrame,
    ) -> Result<Vec<ClientDelivery>, DispatchError> {
        if !matches!(frame.kind(), FrameKind::ServerRequest { .. }) {
            return Err(DispatchError::InvalidNativeEvidence);
        }
        let envelope = EnvelopeRouting::parse(frame.raw_json().as_bytes())?;
        let id = envelope.id.ok_or(DispatchError::InvalidNativeEvidence)?;
        let params = envelope
            .params
            .ok_or(DispatchError::InvalidNativeEvidence)?
            .policy;
        let native_thread_id = requested_thread(&params)?;
        let native_turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .ok_or(DispatchError::InvalidNativeEvidence)?;
        let active = self
            .active
            .as_ref()
            .ok_or(DispatchError::InvalidNativeEvidence)?;
        if active.permit.reference.native_thread_id != native_thread_id
            || active.native_turn_id.as_deref() != Some(native_turn_id)
        {
            return Err(DispatchError::InvalidNativeEvidence);
        }
        self.registry
            .authorize_tool_reply(&self.owner, &native_thread_id, &active.permit)?;
        let key = (active.client.clone(), id);
        if self.server_requests.contains_key(&key)
            || self.server_requests.len() >= MAX_SERVER_ASSOCIATIONS
        {
            return Err(DispatchError::InvalidNativeEvidence);
        }
        let delivery = ClientDelivery {
            client: active.client.clone(),
            frame: transport_native(&frame)?,
        };
        self.server_requests.insert(
            key,
            ServerAssociation {
                native_key,
                operation: active.operation.clone(),
                native_thread_id,
                native_turn_id: native_turn_id.to_owned(),
                uncertain_reply: false,
            },
        );
        Ok(vec![delivery])
    }

    fn native_notification(
        &mut self,
        frame: NativeFrame,
    ) -> Result<Vec<ClientDelivery>, DispatchError> {
        if !matches!(frame.kind(), FrameKind::Notification { .. }) {
            return Err(DispatchError::InvalidNativeEvidence);
        }
        let Some(active) = self.active.as_ref() else {
            return Ok(Vec::new());
        };
        if let Some(terminal) = frame.terminal_turn() {
            if terminal.native_thread_id != active.permit.reference.native_thread_id.as_str()
                || Some(terminal.native_turn_id.as_str()) != active.native_turn_id.as_deref()
            {
                return Ok(Vec::new());
            }
            let delivery = ClientDelivery {
                client: active.client.clone(),
                frame: transport_native(&frame)?,
            };
            self.finish_active()?;
            return Ok(vec![delivery]);
        }
        // Payload notifications are delivered only to the admitted client's
        // exact thread/turn. A late turn-start event cannot bind an unknown
        // request; that requires its original operation token and response.
        let envelope = EnvelopeRouting::parse(frame.raw_json().as_bytes())?;
        let Some(params) = envelope.params else {
            return Ok(Vec::new());
        };
        if params.policy.get("threadId").and_then(Value::as_str)
            != Some(active.permit.reference.native_thread_id.as_str())
            || params.policy.get("turnId").and_then(Value::as_str)
                != active.native_turn_id.as_deref()
            || active.native_turn_id.is_none()
        {
            return Ok(Vec::new());
        }
        self.registry.validate_write_permit(&active.permit)?;
        Ok(vec![ClientDelivery {
            client: active.client.clone(),
            frame: transport_native(&frame)?,
        }])
    }

    fn finish_active(&mut self) -> Result<(), DispatchError> {
        let active = self
            .active
            .as_ref()
            .ok_or(DispatchError::InvalidNativeEvidence)?;
        self.registry.finish_write(&active.permit)?;
        let operation = &active.operation;
        self.server_requests
            .retain(|_, association| &association.operation != operation);
        self.active = None;
        Ok(())
    }
}

#[derive(Deserialize)]
struct EnvelopeRouting {
    id: Option<RpcId>,
    method: Option<String>,
    #[serde(default, deserialize_with = "deserialize_params")]
    params: Option<RoutingParams>,
}
fn deserialize_params<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<RoutingParams>, D::Error> {
    RoutingParams::deserialize(deserializer).map(Some)
}
impl EnvelopeRouting {
    fn parse(bytes: &[u8]) -> Result<Self, DispatchError> {
        serde_json::from_slice(bytes).map_err(|_| DispatchError::InvalidEnvelope)
    }
}

/// Only policy/routing scalars become Value. Opaque payload numbers and bodies
/// are consumed as IgnoredAny, then forwarded from the original raw span.
struct RoutingParams {
    policy: Value,
}
impl Default for RoutingParams {
    fn default() -> Self {
        Self {
            policy: Value::Object(Map::new()),
        }
    }
}
impl<'de> Deserialize<'de> for RoutingParams {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ParamsVisitor;
        impl<'de> Visitor<'de> for ParamsVisitor {
            type Value = RoutingParams;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("unique parameter fields")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<RoutingParams, M::Error> {
                let mut seen = HashSet::new();
                let mut policy = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom("duplicate parameter field"));
                    }
                    if matches!(key.as_str(), "threadId" | "turnId" | "expectedTurnId") {
                        let value: String = map.next_value()?;
                        super::validate_identifier(&value)
                            .map_err(|_| de::Error::custom("invalid routing identifier"))?;
                        policy.insert(key, Value::String(value));
                    } else {
                        // Registry policy consumes presence/null for override
                        // fields. Keep it the sole field allow/deny authority;
                        // every other opaque value also needs no decoding.
                        let present = map.next_value::<Option<IgnoredAny>>()?.is_some();
                        policy.insert(
                            key,
                            if present {
                                Value::Bool(true)
                            } else {
                                Value::Null
                            },
                        );
                    }
                }
                Ok(RoutingParams {
                    policy: Value::Object(policy),
                })
            }
        }
        deserializer.deserialize_map(ParamsVisitor)
    }
}

fn requested_thread(params: &Value) -> Result<NativeThreadId, DispatchError> {
    Ok(NativeThreadId::new(
        params
            .get("threadId")
            .and_then(Value::as_str)
            .ok_or(BrokerError::InvalidRequest)?,
    )?)
}
fn raw_params(bytes: &[u8]) -> Result<RawJson, DispatchError> {
    let raw = std::str::from_utf8(bytes).map_err(|_| DispatchError::InvalidEnvelope)?;
    parse_raw(raw_member(raw, "params")?.unwrap_or("{}"))
}
fn parse_raw(raw: &str) -> Result<RawJson, DispatchError> {
    RawJson::parse(raw).map_err(|_| DispatchError::InvalidEnvelope)
}
fn transport_native(frame: &NativeFrame) -> Result<TransportFrame, DispatchError> {
    TransportFrame::parse(frame.raw_json().as_bytes().to_vec(), FRAME_BYTES)
        .map_err(|_| DispatchError::InvalidNativeEvidence)
}
fn client_response(id: &RpcId, reply: BackendReply) -> Result<TransportFrame, DispatchError> {
    let id = match id {
        RpcId::String(value) => {
            serde_json::to_string(value).map_err(|_| DispatchError::InvalidEnvelope)?
        }
        RpcId::Number(value) => value.to_string(),
    };
    let (field, body) = match reply {
        BackendReply::Result(body) => ("result", body),
        BackendReply::Error(body) => ("error", body),
    };
    let raw = format!("{{\"id\":{id},\"{field}\":{}}}", body.as_str());
    TransportFrame::parse(raw.into_bytes(), FRAME_BYTES).map_err(|_| DispatchError::InvalidEnvelope)
}

/// Locate a validated top-level JSON member without decoding its value. The
/// transport/header parser validates syntax and duplicate routing separately.
fn raw_member<'a>(raw: &'a str, wanted: &str) -> Result<Option<&'a str>, DispatchError> {
    let bytes = raw.as_bytes();
    let mut cursor = skip_space(bytes, 0);
    if bytes.get(cursor) != Some(&b'{') {
        return Err(DispatchError::InvalidEnvelope);
    }
    cursor += 1;
    loop {
        cursor = skip_space(bytes, cursor);
        if bytes.get(cursor) == Some(&b'}') {
            return Ok(None);
        }
        let key_end = value_end(bytes, cursor)?;
        let key: String = serde_json::from_str(&raw[cursor..key_end])
            .map_err(|_| DispatchError::InvalidEnvelope)?;
        cursor = skip_space(bytes, key_end);
        if bytes.get(cursor) != Some(&b':') {
            return Err(DispatchError::InvalidEnvelope);
        }
        cursor = skip_space(bytes, cursor + 1);
        let end = value_end(bytes, cursor)?;
        if key == wanted {
            return Ok(Some(&raw[cursor..end]));
        }
        cursor = skip_space(bytes, end);
        match bytes.get(cursor) {
            Some(b',') => cursor += 1,
            Some(b'}') => return Ok(None),
            _ => return Err(DispatchError::InvalidEnvelope),
        }
    }
}
fn skip_space(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    cursor
}
fn value_end(bytes: &[u8], start: usize) -> Result<usize, DispatchError> {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for (offset, byte) in bytes
        .get(start..)
        .ok_or(DispatchError::InvalidEnvelope)?
        .iter()
        .copied()
        .enumerate()
    {
        let cursor = start + offset;
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
                if depth == 0 {
                    return Ok(cursor + 1);
                }
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' if depth > 0 => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(cursor + 1);
                    }
                }
                b',' | b'}' | b':' if depth == 0 => return Ok(cursor),
                _ => {}
            }
        }
    }
    Err(DispatchError::InvalidEnvelope)
}

#[cfg(test)]
mod tests;
