//! B/C native SDK actor. Admission is constructed only by the protected owner
//! configuration loader; wire traffic cannot choose a source, principal or home.
//! Actors own stdio and borrow the service's owner connection. The convenience
//! wrapper binds one pipe; a parent reader can reuse actors under one listener.

#[cfg(test)]
mod tests;

use crate::services::session_broker::{
    local_transport::{
        EnvelopeKind, LocalConnection, LocalListener, SendOutcome, ServicePeer, TransportClosed,
        TransportError, TransportFrame, TransportLimits,
    },
    native_core::{
        FrameKind, NativeCore, NativeCoreError, NativeCoreSpec, NativeEvent, NativeShutdown,
        RawJson, RequestTicket, RpcId, ServerReply, ServerRequestTicket,
    },
    service_host::{RuntimeSourceKey, ServiceContext, ServiceHostError, ServiceRole},
    windows_isolation::{CoreRole, NormalServiceRole, Sid, VerifiedNormalServicePrincipal},
    CanonicalSessionId,
};
use serde::de::IgnoredAny;
use std::{
    num::NonZeroU64,
    ops::Range,
    time::{Duration, Instant},
};

const FRAME_BYTES: usize = 32 * 1024 * 1024;
const CORRELATION_LIMIT: usize = 64;
const HEADER_FIELDS: usize = 64;
const ROUTING_BYTES: usize = 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const INITIALIZE_PARAMS: &str = r#"{"clientInfo":{"name":"aag_session_native_role","version":"0.2.0"},"capabilities":{"experimentalApi":true}}"#;
const READER_METHODS: &[&str] = &[
    "model/list",
    "thread/list",
    "thread/search",
    "thread/loaded/list",
    "thread/attachmentOwner/list",
    "thread/read",
    "thread/searchOccurrences",
    "thread/turns/list",
    "thread/items/list",
    "thread/items/read",
    "thread/timeline/list",
    "thread/attachment/list",
    "thread/goal/get",
    "thread/queue/list",
    "thread/backgroundTerminals/list",
];
const READER_FORBIDDEN_PARAMS: &[&str] = &[
    "path",
    "history",
    "config",
    "modelProvider",
    "approvalPolicy",
    "approvalsReviewer",
    "permissions",
    "sandbox",
    "sandboxPolicy",
    "runtimeWorkspaceRoots",
    "environments",
    "selectedCapabilityRoots",
    "dynamicTools",
    "disabledPluginIds",
];

/// No Debug/Serde/Clone and no wire constructor. Source generation, native
/// paths and all three principals come from the protected source catalog.
pub(super) struct NativeRoleAdmission {
    pub(super) role: ServiceRole,
    pub(super) source_key: RuntimeSourceKey,
    pub(super) source_id: CanonicalSessionId,
    pub(super) physical_generation: NonZeroU64,
    pub(super) owner_sid: Sid,
    pub(super) runtime_sid: Sid,
    pub(super) reader_sid: Sid,
    pub(super) principal: VerifiedNormalServicePrincipal,
    pub(super) native_spec: NativeCoreSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeRoleError {
    RoleMismatch,
    PrincipalMismatch,
    SourceMismatch,
    InvalidEnvelope,
    DuplicateId,
    UnknownId,
    Capacity,
    InitializeFailed,
    InitializeTimeout,
    ReaderServerRequest,
    OwnerDisconnected,
    TransportFailed,
    CleanupUnknown,
    RuntimeUnavailable,
    Host(ServiceHostError),
    Native(NativeCoreError),
}

/// The caller must retain a failed owner and terminate/reconcile the service;
/// an uncertain native process can never be replaced by another actor.
pub(super) struct NativeRoleFailure {
    pub(super) error: NativeRoleError,
    pub(super) retained_owner: Option<NativeCore>,
}

/// One fully admitted native source, without a listener or connection owner.
/// The parent serializes frame/pump calls and owns routing between source keys.
pub(super) struct AdmittedNativeActor {
    context: ServiceContext,
    role: ServiceRole,
    source_key: RuntimeSourceKey,
    owner_sid: Sid,
    service_sid: Sid,
    core: NativeCore,
    requests: Correlations<RequestTicket>,
    server_requests: Correlations<ServerRequestTicket>,
    next_server_id: u64,
    fault: Option<NativeRoleError>,
}

impl AdmittedNativeActor {
    /// Requires an existing Tokio runtime, but creates no pipe or background
    /// dispatcher. All authority is checked before the native process is made.
    pub(super) async fn admit(
        context: &ServiceContext,
        admission: NativeRoleAdmission,
    ) -> Result<Self, NativeRoleFailure> {
        admission
            .validate(context)
            .map_err(|error| NativeRoleFailure {
                error,
                retained_owner: None,
            })?;
        tokio::runtime::Handle::try_current().map_err(|_| NativeRoleFailure {
            error: NativeRoleError::RuntimeUnavailable,
            retained_owner: None,
        })?;
        let service_sid = admission.principal.service_sid().clone();
        let mut core = NativeCore::spawn_for_service(admission.native_spec, admission.principal)
            .map_err(|failure| NativeRoleFailure {
                error: NativeRoleError::Native(failure.error),
                retained_owner: failure.retained_owner,
            })?;
        if let Err(error) = initialize(context, &mut core).await {
            let shutdown = core.shutdown(Instant::now() + CLEANUP_TIMEOUT);
            return Err(NativeRoleFailure {
                error: if matches!(shutdown, NativeShutdown::OutcomeUnknown { .. }) {
                    NativeRoleError::CleanupUnknown
                } else {
                    error
                },
                retained_owner: matches!(shutdown, NativeShutdown::OutcomeUnknown { .. })
                    .then_some(core),
            });
        }
        Ok(Self {
            context: context.clone(),
            role: admission.role,
            source_key: admission.source_key,
            owner_sid: admission.owner_sid,
            service_sid,
            core,
            requests: Correlations::new(CORRELATION_LIMIT),
            server_requests: Correlations::new(CORRELATION_LIMIT),
            next_server_id: 0,
            fault: None,
        })
    }

    pub(super) fn source_key(&self) -> RuntimeSourceKey {
        self.source_key
    }
    pub(super) fn role(&self) -> ServiceRole {
        self.role
    }
    pub(super) fn owner_sid(&self) -> &Sid {
        &self.owner_sid
    }
    pub(super) fn service_sid(&self) -> &Sid {
        &self.service_sid
    }

    /// The parent uses the same bounded write deadline when admitting a frame
    /// onto the shared owner connection; this never extends a cleanup budget.
    pub(super) fn deadline(&self) -> Instant {
        Instant::now()
            + self
                .context
                .cleanup_remaining()
                .unwrap_or(WRITE_TIMEOUT)
                .min(WRITE_TIMEOUT)
    }

    /// Poll stdio and deliver responses/events on the parent's one connection.
    /// Request correlation is retained until the send confirms every byte.
    /// Any error permanently faults this actor; the parent must clean it up.
    pub(super) async fn pump(
        &mut self,
        connection: &LocalConnection,
    ) -> Result<(), NativeRoleError> {
        if let Some(error) = self.fault {
            return Err(error);
        }
        let result = self.pump_inner(connection).await;
        if let Err(error) = result {
            self.fault = Some(error);
        }
        result
    }

    async fn pump_inner(&mut self, connection: &LocalConnection) -> Result<(), NativeRoleError> {
        if self.context.is_cancelled() {
            return Err(NativeRoleError::Host(ServiceHostError::StopRequested));
        }
        self.core.poll().map_err(NativeRoleError::Native)?;
        let mut ready = Vec::new();
        for (owner_id, ticket) in &self.requests.entries {
            if let Some(response) = self
                .core
                .take_response(ticket)
                .map_err(NativeRoleError::Native)?
            {
                ready.push((owner_id.clone(), response.frame));
            }
        }
        for (owner_id, frame) in ready {
            send(
                connection,
                rewrite_id(frame.raw_json(), &owner_id)?,
                self.deadline(),
            )
            .await?;
            self.requests.remove(&owner_id)?;
        }
        while let Some(event) = self.core.pop_event() {
            let frame = match event {
                NativeEvent::Notification(frame) => transport(frame.raw_json())?,
                NativeEvent::ServerRequest { ticket, frame } => {
                    if self.role == ServiceRole::Reader {
                        return Err(NativeRoleError::ReaderServerRequest);
                    }
                    self.next_server_id = self
                        .next_server_id
                        .checked_add(1)
                        .ok_or(NativeRoleError::Capacity)?;
                    let owner_id =
                        OwnerId::parse(&format!("\"aag_role_server_{}\"", self.next_server_id))?;
                    self.server_requests.insert(owner_id.clone(), ticket)?;
                    rewrite_id(frame.raw_json(), &owner_id)?
                }
            };
            send(connection, frame, self.deadline()).await?;
        }
        Ok(())
    }

    /// Accept one native RPC already routed by A to this admitted source. Wire
    /// fields cannot replace the actor's catalog identity or permission role.
    /// Any error permanently faults this actor; no request is retried.
    pub(super) async fn frame(
        &mut self,
        connection: &LocalConnection,
        incoming: TransportFrame,
    ) -> Result<(), NativeRoleError> {
        if let Some(error) = self.fault {
            return Err(error);
        }
        let result = self.frame_inner(connection, incoming).await;
        if let Err(error) = result {
            self.fault = Some(error);
        }
        result
    }

    async fn frame_inner(
        &mut self,
        connection: &LocalConnection,
        incoming: TransportFrame,
    ) -> Result<(), NativeRoleError> {
        if self.context.is_cancelled() {
            return Err(NativeRoleError::Host(ServiceHostError::StopRequested));
        }
        let raw =
            std::str::from_utf8(incoming.bytes()).map_err(|_| NativeRoleError::InvalidEnvelope)?;
        let members = raw_members(raw)?;
        let owner_id = OwnerId::parse(members.get("id").ok_or(NativeRoleError::InvalidEnvelope)?)?;
        match incoming.kind() {
            EnvelopeKind::Request => {
                self.requests.check(&owner_id)?;
                let method: String = serde_json::from_str(
                    members
                        .get("method")
                        .ok_or(NativeRoleError::InvalidEnvelope)?,
                )
                .map_err(|_| NativeRoleError::InvalidEnvelope)?;
                if method.is_empty()
                    || method.len() > ROUTING_BYTES
                    || method.chars().any(char::is_control)
                {
                    return Err(NativeRoleError::InvalidEnvelope);
                }
                if method == "initialize"
                    || (self.role == ServiceRole::Reader
                        && !reader_permits(&method, members.get("params"))?)
                {
                    send(connection, denied(&owner_id)?, self.deadline()).await?;
                    return Ok(());
                }
                let params = members
                    .get("params")
                    .map(RawJson::parse)
                    .transpose()
                    .map_err(NativeRoleError::Native)?;
                let ticket = self
                    .core
                    .request(&method, params.as_ref())
                    .map_err(NativeRoleError::Native)?;
                self.requests.insert(owner_id, ticket)?;
            }
            EnvelopeKind::Response => {
                if self.role == ServiceRole::Reader {
                    return Err(NativeRoleError::ReaderServerRequest);
                }
                let ticket = self.server_requests.remove(&owner_id)?;
                let reply = if let Some(raw) = members.get("result") {
                    ServerReply::Result(RawJson::parse(raw).map_err(NativeRoleError::Native)?)
                } else {
                    ServerReply::Error(
                        RawJson::parse(
                            members
                                .get("error")
                                .ok_or(NativeRoleError::InvalidEnvelope)?,
                        )
                        .map_err(NativeRoleError::Native)?,
                    )
                };
                self.core
                    .reply_to_server_request(&ticket, reply)
                    .map_err(NativeRoleError::Native)?;
            }
            EnvelopeKind::Notification => return Err(NativeRoleError::InvalidEnvelope),
        }
        Ok(())
    }

    /// Quiesce this source before the parent replaces its slot. This does not
    /// stop the shared C service or close the parent's reader connection.
    pub(super) fn cleanup(self, deadline: Instant) -> Result<(), NativeRoleFailure> {
        let error = self.fault;
        self.finish(error, TransportClosed::Closed, deadline)
    }

    fn finish(
        mut self,
        error: Option<NativeRoleError>,
        pipes: TransportClosed,
        deadline: Instant,
    ) -> Result<(), NativeRoleFailure> {
        self.core.cancellation().cancel();
        let native_closed = self.core.shutdown(deadline);
        cleanup_result(error, pipes, native_closed).map_err(|error| NativeRoleFailure {
            error,
            retained_owner: matches!(native_closed, NativeShutdown::OutcomeUnknown { .. })
                .then_some(self.core),
        })
    }
}
impl std::fmt::Debug for NativeRoleFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeRoleFailure")
            .field("error", &self.error)
            .field("has_retained_owner", &self.retained_owner.is_some())
            .finish()
    }
}

impl NativeRoleAdmission {
    fn validate(&self, context: &ServiceContext) -> Result<(), NativeRoleError> {
        let core_role =
            admitted_core_role(self.role, self.source_key, self.physical_generation.get())?;
        if context.role() != self.role {
            return Err(NativeRoleError::RoleMismatch);
        }
        if self.owner_sid == self.runtime_sid
            || self.owner_sid == self.reader_sid
            || self.runtime_sid == self.reader_sid
        {
            return Err(NativeRoleError::PrincipalMismatch);
        }
        // ServicePeer also validates each virtual-service SID before any bind.
        for (role, sid) in [
            (ServiceRole::Policy, &self.owner_sid),
            (ServiceRole::Reader, &self.reader_sid),
            (
                ServiceRole::runtime(self.source_key.as_str(), self.physical_generation.get())
                    .map_err(NativeRoleError::Host)?,
                &self.runtime_sid,
            ),
        ] {
            ServicePeer::new(role, sid.clone()).map_err(|_| NativeRoleError::PrincipalMismatch)?;
        }
        let expected_sid = if self.role == ServiceRole::Reader {
            &self.reader_sid
        } else {
            &self.runtime_sid
        };
        if context.verified_user_sid().map_err(NativeRoleError::Host)? != expected_sid.value()
            || self.principal.service_sid() != expected_sid
        {
            return Err(NativeRoleError::PrincipalMismatch);
        }
        let binding = &self.native_spec.binding;
        if binding.canonical_session_id != self.source_id
            || binding.physical_generation != self.physical_generation.get()
            || binding.role != core_role
        {
            return Err(NativeRoleError::SourceMismatch);
        }
        match (self.role, self.principal.role()) {
            (ServiceRole::Reader, NormalServiceRole::TrustedReader) => {}
            (
                ServiceRole::Runtime { .. },
                NormalServiceRole::RuntimeGeneration {
                    source_id,
                    physical_generation,
                },
            ) if source_id == &self.source_id
                && *physical_generation == self.physical_generation.get() => {}
            _ => return Err(NativeRoleError::PrincipalMismatch),
        }
        self.principal
            .verify_current()
            .map_err(|_| NativeRoleError::PrincipalMismatch)
    }
}

fn admitted_core_role(
    role: ServiceRole,
    source_key: RuntimeSourceKey,
    generation: u64,
) -> Result<CoreRole, NativeRoleError> {
    match role {
        ServiceRole::Policy => Err(NativeRoleError::RoleMismatch),
        ServiceRole::Reader => Ok(CoreRole::ReadOnly),
        ServiceRole::Runtime {
            source_key: observed,
            generation: observed_generation,
        } if observed == source_key && observed_generation.get() == generation => {
            Ok(CoreRole::ReadWrite)
        }
        _ => Err(NativeRoleError::SourceMismatch),
    }
}

/// Dedicated B/C service business entrypoint. Validation precedes runtime,
/// process creation and pipe binding. A is categorically rejected here.
pub(super) fn run(
    context: ServiceContext,
    admission: NativeRoleAdmission,
) -> Result<(), NativeRoleFailure> {
    admission
        .validate(&context)
        .map_err(|error| NativeRoleFailure {
            error,
            retained_owner: None,
        })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| NativeRoleFailure {
            error: NativeRoleError::RuntimeUnavailable,
            retained_owner: None,
        })?;
    runtime.block_on(async {
        let actor = AdmittedNativeActor::admit(&context, admission).await?;
        run_actor(context, actor).await
    })
}

async fn run_actor(
    context: ServiceContext,
    mut actor: AdmittedNativeActor,
) -> Result<(), NativeRoleFailure> {
    let mut listener = None;
    let mut connection = None;
    let business = async {
        let peer = ServicePeer::new(actor.role(), actor.service_sid().clone())
            .map_err(|_| NativeRoleError::PrincipalMismatch)?;
        let limits = TransportLimits {
            connections: 1,
            ..TransportLimits::default()
        };
        listener = Some(
            LocalListener::bind(&context, peer, &[actor.owner_sid().clone()], limits)
                .map_err(|_| NativeRoleError::TransportFailed)?,
        );
        context.ready().map_err(NativeRoleError::Host)?;
        let active_listener = listener.as_ref().ok_or(NativeRoleError::TransportFailed)?;
        // There is no reconnection after a possible request write. The same
        // physical generation must be reconciled by A before another admission.
        loop {
            if context.is_cancelled() {
                return Ok(());
            }
            actor.core.poll().map_err(NativeRoleError::Native)?;
            reject_reader_server_requests(actor.role, &mut actor.core)?;
            match active_listener.accept(Instant::now() + POLL_INTERVAL).await {
                Ok(value) => {
                    connection = Some(value);
                    break;
                }
                Err(TransportError::DeadlineExceeded) => {}
                Err(_) => return Err(NativeRoleError::TransportFailed),
            }
        }
        serve(
            &context,
            &mut actor,
            connection
                .as_ref()
                .ok_or(NativeRoleError::TransportFailed)?,
        )
        .await
    }
    .await;
    let error = business.err();
    context.request_stop();
    let deadline = Instant::now()
        + context
            .cleanup_remaining()
            .unwrap_or(CLEANUP_TIMEOUT)
            .min(CLEANUP_TIMEOUT);
    let mut pipe_closed = TransportClosed::Closed;
    // Stop accepting first, then close the owner connection, then cancel and
    // join the native job. Each cleanup consumes the same fixed deadline.
    if let Some(value) = &listener {
        value.cancellation().cancel();
    }
    if let Some(value) = &connection {
        value.cancellation().cancel();
        pipe_closed = value.close(deadline).await;
    }
    if let Some(value) = &listener {
        if let TransportClosed::OutcomeUnknown(problem) = value.close(deadline).await {
            pipe_closed = TransportClosed::OutcomeUnknown(problem);
        }
    }
    actor.finish(error, pipe_closed, deadline)
}

async fn initialize(
    context: &ServiceContext,
    core: &mut NativeCore,
) -> Result<(), NativeRoleError> {
    let params = RawJson::parse(INITIALIZE_PARAMS).map_err(NativeRoleError::Native)?;
    let ticket = core
        .request("initialize", Some(&params))
        .map_err(NativeRoleError::Native)?;
    let deadline = Instant::now() + INITIALIZE_TIMEOUT;
    loop {
        if context.is_cancelled() {
            return Err(NativeRoleError::Host(ServiceHostError::StopRequested));
        }
        if Instant::now() >= deadline {
            return Err(NativeRoleError::InitializeTimeout);
        }
        core.poll().map_err(NativeRoleError::Native)?;
        // Initialization cannot require an approval or tool response in either
        // role; never manufacture a successful handshake from such a request.
        while let Some(event) = core.pop_event() {
            if matches!(event, NativeEvent::ServerRequest { .. }) {
                return Err(NativeRoleError::InitializeFailed);
            }
        }
        if let Some(response) = core
            .take_response(&ticket)
            .map_err(NativeRoleError::Native)?
        {
            if !matches!(
                response.frame.kind(),
                FrameKind::Response {
                    is_error: false,
                    ..
                }
            ) {
                return Err(NativeRoleError::InitializeFailed);
            }
            core.notify_initialized().map_err(NativeRoleError::Native)?;
            core.poll().map_err(NativeRoleError::Native)?;
            return Ok(());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn reject_reader_server_requests(
    role: ServiceRole,
    core: &mut NativeCore,
) -> Result<(), NativeRoleError> {
    if role == ServiceRole::Reader {
        while let Some(event) = core.pop_event() {
            if matches!(event, NativeEvent::ServerRequest { .. }) {
                return Err(NativeRoleError::ReaderServerRequest);
            }
        }
    }
    Ok(())
}

async fn serve(
    context: &ServiceContext,
    actor: &mut AdmittedNativeActor,
    connection: &LocalConnection,
) -> Result<(), NativeRoleError> {
    loop {
        if context.is_cancelled() {
            return Ok(());
        }
        actor.pump(connection).await?;
        let incoming = match connection.receive(Instant::now() + POLL_INTERVAL).await {
            Ok(frame) => frame,
            Err(TransportError::DeadlineExceeded) => continue,
            Err(_) if context.is_cancelled() => return Ok(()),
            Err(_) => return Err(NativeRoleError::OwnerDisconnected),
        };
        actor.frame(connection, incoming).await?;
    }
}

async fn send(
    connection: &LocalConnection,
    frame: TransportFrame,
    deadline: Instant,
) -> Result<(), NativeRoleError> {
    match connection.send(frame, deadline).await {
        SendOutcome::Written => Ok(()),
        SendOutcome::NotSent(_) | SendOutcome::OutcomeUnknown(_) => {
            Err(NativeRoleError::TransportFailed)
        }
    }
}

fn cleanup_result(
    error: Option<NativeRoleError>,
    pipes: TransportClosed,
    native: NativeShutdown,
) -> Result<(), NativeRoleError> {
    if matches!(pipes, TransportClosed::OutcomeUnknown(_))
        || matches!(native, NativeShutdown::OutcomeUnknown { .. })
    {
        return Err(NativeRoleError::CleanupUnknown);
    }
    match error {
        Some(NativeRoleError::Host(ServiceHostError::StopRequested)) | None => Ok(()),
        Some(error) => Err(error),
    }
}

fn reader_permits(method: &str, params: Option<&str>) -> Result<bool, NativeRoleError> {
    if !READER_METHODS.contains(&method) {
        return Ok(false);
    }
    if let Some(params) = params {
        let members = raw_members(params)?;
        if READER_FORBIDDEN_PARAMS
            .iter()
            .any(|key| members.get(key).is_some_and(|value| value != "null"))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Clone)]
struct OwnerId {
    key: RpcId,
    raw: String,
}
impl OwnerId {
    fn parse(raw: &str) -> Result<Self, NativeRoleError> {
        if raw.len() > ROUTING_BYTES {
            return Err(NativeRoleError::InvalidEnvelope);
        }
        let key = serde_json::from_str(raw).map_err(|_| NativeRoleError::InvalidEnvelope)?;
        Ok(Self {
            key,
            raw: raw.to_owned(),
        })
    }
}
struct Correlations<T> {
    entries: Vec<(OwnerId, T)>,
    limit: usize,
}
impl<T> Correlations<T> {
    fn new(limit: usize) -> Self {
        Self {
            entries: Vec::new(),
            limit,
        }
    }
    fn check(&self, id: &OwnerId) -> Result<(), NativeRoleError> {
        if self
            .entries
            .iter()
            .any(|(existing, _)| existing.key == id.key)
        {
            return Err(NativeRoleError::DuplicateId);
        }
        if self.entries.len() >= self.limit {
            return Err(NativeRoleError::Capacity);
        }
        Ok(())
    }
    fn insert(&mut self, id: OwnerId, value: T) -> Result<(), NativeRoleError> {
        self.check(&id)?;
        self.entries.push((id, value));
        Ok(())
    }
    fn remove(&mut self, id: &OwnerId) -> Result<T, NativeRoleError> {
        let index = self
            .entries
            .iter()
            .position(|(existing, _)| existing.key == id.key)
            .ok_or(NativeRoleError::UnknownId)?;
        Ok(self.entries.remove(index).1)
    }
}

fn transport(raw: &str) -> Result<TransportFrame, NativeRoleError> {
    TransportFrame::parse(raw.as_bytes().to_vec(), FRAME_BYTES)
        .map_err(|_| NativeRoleError::InvalidEnvelope)
}
fn denied(id: &OwnerId) -> Result<TransportFrame, NativeRoleError> {
    transport(&format!(
        r#"{{"id":{},"error":{{"code":-32601,"message":"session_native_role_method_forbidden"}}}}"#,
        id.raw
    ))
}
fn rewrite_id(raw: &str, id: &OwnerId) -> Result<TransportFrame, NativeRoleError> {
    let members = raw_members(raw)?;
    let span = members
        .fields
        .iter()
        .find(|(name, _)| name == "id")
        .map(|(_, span)| span)
        .ok_or(NativeRoleError::InvalidEnvelope)?;
    let mut rewritten = String::with_capacity(raw.len() + id.raw.len());
    rewritten.push_str(&raw[..span.start]);
    rewritten.push_str(&id.raw);
    rewritten.push_str(&raw[span.end..]);
    transport(&rewritten)
}

struct RawMembers<'a> {
    raw: &'a str,
    fields: Vec<(String, Range<usize>)>,
}
impl<'a> RawMembers<'a> {
    fn get(&self, key: &str) -> Option<&'a str> {
        self.fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, span)| &self.raw[span.clone()])
    }
}

/// Header-only deserialization: byte offsets locate each original JSON value.
/// IgnoredAny validates opaque values without narrowing big integers through
/// Value/f64 or rebuilding signatures, nested ids, results or parameters.
fn raw_members(raw: &str) -> Result<RawMembers<'_>, NativeRoleError> {
    if raw.len() > FRAME_BYTES {
        return Err(NativeRoleError::Capacity);
    }
    serde_json::from_str::<IgnoredAny>(raw).map_err(|_| NativeRoleError::InvalidEnvelope)?;
    let bytes = raw.as_bytes();
    let mut cursor = skip_space(bytes, 0);
    if bytes.get(cursor) != Some(&b'{') {
        return Err(NativeRoleError::InvalidEnvelope);
    }
    cursor += 1;
    let mut fields = Vec::new();
    loop {
        cursor = skip_space(bytes, cursor);
        if bytes.get(cursor) == Some(&b'}') {
            return Ok(RawMembers { raw, fields });
        }
        if fields.len() >= HEADER_FIELDS {
            return Err(NativeRoleError::Capacity);
        }
        let mut key_stream =
            serde_json::Deserializer::from_str(&raw[cursor..]).into_iter::<String>();
        let key = key_stream
            .next()
            .ok_or(NativeRoleError::InvalidEnvelope)?
            .map_err(|_| NativeRoleError::InvalidEnvelope)?;
        cursor = skip_space(bytes, cursor + key_stream.byte_offset());
        if bytes.get(cursor) != Some(&b':') {
            return Err(NativeRoleError::InvalidEnvelope);
        }
        cursor = skip_space(bytes, cursor + 1);
        let mut value_stream =
            serde_json::Deserializer::from_str(&raw[cursor..]).into_iter::<IgnoredAny>();
        value_stream
            .next()
            .ok_or(NativeRoleError::InvalidEnvelope)?
            .map_err(|_| NativeRoleError::InvalidEnvelope)?;
        let end = cursor + value_stream.byte_offset();
        if fields.iter().any(|(name, _)| name == &key) {
            return Err(NativeRoleError::InvalidEnvelope);
        }
        fields.push((key, cursor..end));
        cursor = skip_space(bytes, end);
        match bytes.get(cursor) {
            Some(b',') => cursor += 1,
            Some(b'}') => return Ok(RawMembers { raw, fields }),
            _ => return Err(NativeRoleError::InvalidEnvelope),
        }
    }
}
fn skip_space(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    cursor
}
