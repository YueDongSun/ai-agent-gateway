//! Pure bounded correlation state, shared by the actor and synthetic fixtures.

use super::framing::{request_frame, response_frame, FrameKind};
use super::framing::{NativeFrame, RawJson, RpcId};
use super::{NativeCoreError, NativeCoreLimits};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryState {
    Queued,
    Dispatching,
    Sent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnknownReason {
    Deadline,
    Transport,
    NativeExit,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RequestTicket {
    pub(super) id: RpcId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ServerRequestTicket {
    id: RpcId,
    serial: u64,
    instance: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestSnapshot {
    pub ticket: RequestTicket,
    pub method: String,
    pub delivery: DeliveryState,
    pub unknown_reason: Option<UnknownReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RequestProgress {
    Pending(RequestSnapshot),
    ResponseReady,
    OutcomeUnknown(RequestSnapshot),
}

pub(crate) struct NativeResponse {
    pub frame: NativeFrame,
    pub after_timeout: bool,
    pub previous_unknown: Option<UnknownReason>,
}
pub(crate) enum NativeEvent {
    Notification(NativeFrame),
    ServerRequest {
        ticket: ServerRequestTicket,
        frame: NativeFrame,
    },
}
pub(crate) enum ServerReply {
    Result(RawJson),
    Error(RawJson),
}

pub(super) struct OutboundFrame {
    pub sequence: u64,
    pub bytes: Vec<u8>,
}

struct RequestRecord {
    ticket: RequestTicket,
    order: u64,
    method: String,
    delivery: DeliveryState,
    unknown_reason: Option<UnknownReason>,
    timed_out: bool,
    response: Option<NativeFrame>,
}
impl RequestRecord {
    fn snapshot(&self) -> RequestSnapshot {
        RequestSnapshot {
            ticket: self.ticket.clone(),
            method: self.method.clone(),
            delivery: self.delivery,
            unknown_reason: self.unknown_reason,
        }
    }
}

enum WritePurpose {
    Request(RpcId),
    Notification,
    ServerReply(ServerRequestTicket),
}
struct WriteRecord {
    purpose: WritePurpose,
    bytes: usize,
    dispatched: bool,
}
struct ServerRecord {
    serial: u64,
    reply_queued: bool,
}

pub(super) struct Multiplexer {
    limits: NativeCoreLimits,
    instance: String,
    next_request: u64,
    next_write: u64,
    next_server: u64,
    requests: HashMap<RpcId, RequestRecord>,
    writes: HashMap<u64, WriteRecord>,
    outbound: VecDeque<OutboundFrame>,
    outbound_bytes: usize,
    response_bytes: usize,
    events: VecDeque<NativeEvent>,
    event_bytes: usize,
    servers: HashMap<RpcId, ServerRecord>,
    closed: Option<UnknownReason>,
    initialized_queued: bool,
}
impl Multiplexer {
    pub fn new(limits: NativeCoreLimits) -> Result<Self, NativeCoreError> {
        limits.validate()?;
        let mut nonce = [0u8; 16];
        getrandom::getrandom(&mut nonce).map_err(|_| NativeCoreError::RandomIdentity)?;
        Ok(Self {
            limits,
            instance: hex::encode(nonce),
            next_request: 0,
            next_write: 0,
            next_server: 0,
            requests: HashMap::new(),
            writes: HashMap::new(),
            outbound: VecDeque::new(),
            outbound_bytes: 0,
            response_bytes: 0,
            events: VecDeque::new(),
            event_bytes: 0,
            servers: HashMap::new(),
            closed: None,
            initialized_queued: false,
        })
    }
    pub fn request(
        &mut self,
        method: &str,
        params: Option<&RawJson>,
    ) -> Result<RequestTicket, NativeCoreError> {
        if self.closed.is_some() {
            return Err(NativeCoreError::Stopped);
        }
        if self.requests.len() >= self.limits.pending_requests {
            return Err(NativeCoreError::CapacityExceeded);
        }
        let order = self
            .next_request
            .checked_add(1)
            .ok_or(NativeCoreError::CapacityExceeded)?;
        let id = RpcId::String(format!("aag_native_{}_{order}", self.instance));
        let bytes = request_frame(&id, method, params, self.limits.frame_bytes)?;
        self.queue(bytes, WritePurpose::Request(id.clone()))?;
        self.next_request = order;
        let ticket = RequestTicket { id: id.clone() };
        self.requests.insert(
            id,
            RequestRecord {
                ticket: ticket.clone(),
                order,
                method: method.to_owned(),
                delivery: DeliveryState::Queued,
                unknown_reason: None,
                timed_out: false,
                response: None,
            },
        );
        Ok(ticket)
    }
    pub fn initialized(&mut self) -> Result<(), NativeCoreError> {
        if self.initialized_queued {
            return Err(NativeCoreError::MalformedFrame);
        }
        let bytes = b"{\"method\":\"initialized\",\"params\":{}}\n".to_vec();
        if bytes.len() - 1 > self.limits.frame_bytes {
            return Err(NativeCoreError::FrameTooLarge);
        }
        self.queue(bytes, WritePurpose::Notification)?;
        self.initialized_queued = true;
        Ok(())
    }
    pub fn take_outbound(&mut self) -> Option<OutboundFrame> {
        if self.closed.is_some() {
            return None;
        }
        let frame = self.outbound.pop_front()?;
        let record = self.writes.get_mut(&frame.sequence)?;
        record.dispatched = true;
        if let WritePurpose::Request(id) = &record.purpose {
            if let Some(request) = self.requests.get_mut(id) {
                request.delivery = DeliveryState::Dispatching;
            }
        }
        Some(frame)
    }
    pub fn requeue_front(&mut self, frame: OutboundFrame) -> Result<(), NativeCoreError> {
        if self.closed.is_some() {
            return Err(NativeCoreError::Stopped);
        }
        let record = self
            .writes
            .get_mut(&frame.sequence)
            .ok_or(NativeCoreError::InvalidWriteReport)?;
        if !record.dispatched
            || record.bytes != frame.bytes.len()
            || self
                .outbound
                .front()
                .is_some_and(|next| next.sequence <= frame.sequence)
        {
            return Err(NativeCoreError::InvalidWriteReport);
        }
        record.dispatched = false;
        if let WritePurpose::Request(id) = &record.purpose {
            if let Some(request) = self.requests.get_mut(id) {
                request.delivery = DeliveryState::Queued;
            }
        }
        self.outbound.push_front(frame);
        Ok(())
    }
    pub fn written(&mut self, sequence: u64) -> Result<(), NativeCoreError> {
        let record = self
            .writes
            .get(&sequence)
            .ok_or(NativeCoreError::InvalidWriteReport)?;
        if !record.dispatched {
            return Err(NativeCoreError::InvalidWriteReport);
        }
        let record = self
            .writes
            .remove(&sequence)
            .ok_or(NativeCoreError::InvalidWriteReport)?;
        self.outbound_bytes -= record.bytes;
        match record.purpose {
            WritePurpose::Request(id) => {
                if let Some(request) = self.requests.get_mut(&id) {
                    request.delivery = DeliveryState::Sent;
                }
            }
            WritePurpose::ServerReply(ticket) => {
                if self
                    .servers
                    .get(&ticket.id)
                    .is_some_and(|server| server.serial == ticket.serial)
                {
                    self.servers.remove(&ticket.id);
                }
            }
            WritePurpose::Notification => {}
        }
        Ok(())
    }
    pub fn receive(&mut self, frame: NativeFrame) -> Result<(), NativeCoreError> {
        let bytes = frame.raw_json().len();
        match frame.kind().clone() {
            FrameKind::Response { id, .. } => {
                let record = self
                    .requests
                    .get_mut(&id)
                    .ok_or(NativeCoreError::UnknownRequest)?;
                if record.response.is_some() {
                    return Err(NativeCoreError::DuplicateResponse);
                }
                if record.delivery == DeliveryState::Queued {
                    return Err(NativeCoreError::MalformedFrame);
                }
                if self.response_bytes.saturating_add(bytes) > self.limits.response_bytes {
                    return Err(NativeCoreError::CapacityExceeded);
                }
                record.delivery = DeliveryState::Sent;
                record.response = Some(frame);
                self.response_bytes += bytes;
            }
            FrameKind::Notification { .. } => {
                self.check_event_capacity(bytes)?;
                self.events.push_back(NativeEvent::Notification(frame));
                self.event_bytes += bytes;
            }
            FrameKind::ServerRequest { id, .. } => {
                if self.servers.contains_key(&id) {
                    return Err(NativeCoreError::DuplicateServerRequest);
                }
                self.check_event_capacity(bytes)?;
                if self.servers.len() >= self.limits.pending_server_requests {
                    return Err(NativeCoreError::CapacityExceeded);
                }
                let serial = self
                    .next_server
                    .checked_add(1)
                    .ok_or(NativeCoreError::CapacityExceeded)?;
                let ticket = ServerRequestTicket {
                    id: id.clone(),
                    serial,
                    instance: self.instance.clone(),
                };
                self.servers.insert(
                    id,
                    ServerRecord {
                        serial,
                        reply_queued: false,
                    },
                );
                self.next_server = serial;
                self.events
                    .push_back(NativeEvent::ServerRequest { ticket, frame });
                self.event_bytes += bytes;
            }
        }
        Ok(())
    }
    pub fn pop_event(&mut self) -> Option<NativeEvent> {
        let event = self.events.pop_front()?;
        self.event_bytes -= match &event {
            NativeEvent::Notification(frame) | NativeEvent::ServerRequest { frame, .. } => {
                frame.raw_json().len()
            }
        };
        Some(event)
    }
    pub fn reply(
        &mut self,
        ticket: &ServerRequestTicket,
        reply: ServerReply,
    ) -> Result<(), NativeCoreError> {
        if self.closed.is_some() {
            return Err(NativeCoreError::Stopped);
        }
        if ticket.instance != self.instance {
            return Err(NativeCoreError::UnknownServerRequest);
        }
        let server = self
            .servers
            .get(&ticket.id)
            .ok_or(NativeCoreError::UnknownServerRequest)?;
        if server.serial != ticket.serial || server.reply_queued {
            return Err(NativeCoreError::UnknownServerRequest);
        }
        let bytes = match reply {
            ServerReply::Result(body) => {
                response_frame(&ticket.id, false, &body, self.limits.frame_bytes)?
            }
            ServerReply::Error(body) => {
                response_frame(&ticket.id, true, &body, self.limits.frame_bytes)?
            }
        };
        self.queue(bytes, WritePurpose::ServerReply(ticket.clone()))?;
        self.servers
            .get_mut(&ticket.id)
            .ok_or(NativeCoreError::UnknownServerRequest)?
            .reply_queued = true;
        Ok(())
    }
    pub fn progress(&self, ticket: &RequestTicket) -> Result<RequestProgress, NativeCoreError> {
        let record = self
            .requests
            .get(&ticket.id)
            .ok_or(NativeCoreError::UnknownRequest)?;
        if record.response.is_some() {
            return Ok(RequestProgress::ResponseReady);
        }
        let snapshot = record.snapshot();
        Ok(if record.unknown_reason.is_some() {
            RequestProgress::OutcomeUnknown(snapshot)
        } else {
            RequestProgress::Pending(snapshot)
        })
    }
    pub fn timeout(&mut self, ticket: &RequestTicket) -> Result<RequestSnapshot, NativeCoreError> {
        let record = self
            .requests
            .get_mut(&ticket.id)
            .ok_or(NativeCoreError::UnknownRequest)?;
        if record.response.is_none() {
            record.timed_out = true;
            if record.unknown_reason.is_none() {
                record.unknown_reason = Some(UnknownReason::Deadline);
            }
        }
        Ok(record.snapshot())
    }
    pub fn take_response(
        &mut self,
        ticket: &RequestTicket,
    ) -> Result<Option<NativeResponse>, NativeCoreError> {
        let record = self
            .requests
            .get(&ticket.id)
            .ok_or(NativeCoreError::UnknownRequest)?;
        if record.response.is_none() {
            return Ok(None);
        }
        let record = self
            .requests
            .remove(&ticket.id)
            .ok_or(NativeCoreError::UnknownRequest)?;
        let frame = record.response.ok_or(NativeCoreError::UnknownRequest)?;
        self.response_bytes -= frame.raw_json().len();
        Ok(Some(NativeResponse {
            frame,
            after_timeout: record.timed_out,
            previous_unknown: record.unknown_reason,
        }))
    }
    pub fn close(&mut self, reason: UnknownReason) {
        self.closed.get_or_insert(reason);
        self.outbound.clear();
        for record in self.requests.values_mut() {
            if record.response.is_none()
                && matches!(record.unknown_reason, None | Some(UnknownReason::Deadline))
            {
                record.unknown_reason = Some(reason);
            }
        }
    }
    pub fn pending_outcomes(&self) -> Vec<RequestSnapshot> {
        let mut records: Vec<_> = self.requests.values().collect();
        records.sort_by_key(|record| record.order);
        records.into_iter().map(RequestRecord::snapshot).collect()
    }
    fn check_event_capacity(&self, bytes: usize) -> Result<(), NativeCoreError> {
        if self.events.len() >= self.limits.queued_events
            || self.event_bytes.saturating_add(bytes) > self.limits.event_bytes
        {
            return Err(NativeCoreError::CapacityExceeded);
        }
        Ok(())
    }
    fn queue(&mut self, bytes: Vec<u8>, purpose: WritePurpose) -> Result<(), NativeCoreError> {
        if self.closed.is_some() {
            return Err(NativeCoreError::Stopped);
        }
        if self.writes.len() >= self.limits.outbound_frames
            || self.outbound_bytes.saturating_add(bytes.len()) > self.limits.outbound_bytes
        {
            return Err(NativeCoreError::CapacityExceeded);
        }
        let sequence = self
            .next_write
            .checked_add(1)
            .ok_or(NativeCoreError::CapacityExceeded)?;
        self.outbound_bytes += bytes.len();
        self.writes.insert(
            sequence,
            WriteRecord {
                purpose,
                bytes: bytes.len(),
                dispatched: false,
            },
        );
        self.outbound.push_back(OutboundFrame { sequence, bytes });
        self.next_write = sequence;
        Ok(())
    }
}
