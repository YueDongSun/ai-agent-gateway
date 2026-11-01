//! Bounded JSONL framing. Raw message text is retained, never reserialized.

use super::{NativeCoreError, MAX_FRAME_BYTES, READ_CHUNK_BYTES};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;

const MAX_ROUTING_TEXT_BYTES: usize = 1024;

/// A validated JSON value compacted only outside JSON strings. No Debug/Serde.
pub(crate) struct RawJson {
    text: String,
}
impl RawJson {
    pub fn parse(text: &str) -> Result<Self, NativeCoreError> {
        if text.len() > MAX_FRAME_BYTES {
            return Err(NativeCoreError::FrameTooLarge);
        }
        serde_json::from_str::<IgnoredAny>(text).map_err(|_| NativeCoreError::InvalidJson)?;
        let mut compact = Vec::with_capacity(text.len());
        let (mut in_string, mut escaped) = (false, false);
        for byte in text.bytes() {
            if in_string {
                compact.push(byte);
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
            } else if !matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
                compact.push(byte);
                if byte == b'"' {
                    in_string = true;
                }
            }
        }
        let text = String::from_utf8(compact).map_err(|_| NativeCoreError::InvalidUtf8)?;
        Ok(Self { text })
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum RpcId {
    String(String),
    Number(i64),
}

impl<'de> Deserialize<'de> for RpcId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IdVisitor;
        impl<'de> Visitor<'de> for IdVisitor {
            type Value = RpcId;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded string or integer RPC id")
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<RpcId, E> {
                Ok(RpcId::Number(value))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<RpcId, E> {
                i64::try_from(value)
                    .map(RpcId::Number)
                    .map_err(|_| E::custom("RPC id out of range"))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<RpcId, E> {
                if !valid_routing_text(value) {
                    return Err(E::custom("invalid RPC id"));
                }
                Ok(RpcId::String(value.to_owned()))
            }
        }
        deserializer.deserialize_any(IdVisitor)
    }
}

impl RpcId {
    pub(super) fn json(&self) -> String {
        match self {
            Self::String(value) => {
                serde_json::to_string(value).expect("string serialization cannot fail")
            }
            Self::Number(value) => value.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FrameKind {
    Response { id: RpcId, is_error: bool },
    Notification { method: String },
    ServerRequest { id: RpcId, method: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalTurn {
    pub native_thread_id: String,
    pub native_turn_id: String,
    pub status: String,
}

pub(crate) struct NativeFrame {
    raw: String,
    kind: FrameKind,
    terminal_turn: Option<TerminalTurn>,
}
impl NativeFrame {
    pub fn parse(raw: String) -> Result<Self, NativeCoreError> {
        if raw.len() > MAX_FRAME_BYTES {
            return Err(NativeCoreError::FrameTooLarge);
        }
        if raw.contains(['\n', '\r']) {
            return Err(NativeCoreError::MalformedFrame);
        }
        let header: FrameHeader =
            serde_json::from_str(&raw).map_err(|_| NativeCoreError::MalformedFrame)?;
        let kind = match (
            header.method,
            header.id,
            header.has_result,
            header.has_error,
        ) {
            (Some(method), Some(id), false, false) => FrameKind::ServerRequest { id, method },
            (Some(method), None, false, false) => FrameKind::Notification { method },
            (None, Some(id), true, false) => FrameKind::Response {
                id,
                is_error: false,
            },
            (None, Some(id), false, true) => FrameKind::Response { id, is_error: true },
            _ => return Err(NativeCoreError::MalformedFrame),
        };
        let terminal_turn = if matches!(&kind, FrameKind::Notification { method } if method == "turn/completed")
        {
            terminal_turn(&raw)
        } else {
            None
        };
        Ok(Self {
            raw,
            kind,
            terminal_turn,
        })
    }
    pub fn raw_json(&self) -> &str {
        &self.raw
    }
    pub fn kind(&self) -> &FrameKind {
        &self.kind
    }
    pub fn terminal_turn(&self) -> Option<&TerminalTurn> {
        self.terminal_turn.as_ref()
    }
}

pub(crate) struct FrameDecoder {
    limit: usize,
    pending: Vec<u8>,
    failed: Option<NativeCoreError>,
    finished: bool,
}
impl FrameDecoder {
    pub fn new(limit: usize) -> Result<Self, NativeCoreError> {
        if limit == 0 || limit > MAX_FRAME_BYTES {
            return Err(NativeCoreError::InvalidConfiguration);
        }
        Ok(Self {
            limit,
            pending: Vec::new(),
            failed: None,
            finished: false,
        })
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<NativeFrame>, NativeCoreError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if self.finished {
            return Err(NativeCoreError::Stopped);
        }
        if bytes.len() > READ_CHUNK_BYTES {
            return self.fail(NativeCoreError::InputChunkTooLarge);
        }
        let mut frames = Vec::new();
        for segment in bytes.split_inclusive(|byte| *byte == b'\n') {
            let ended = segment.last() == Some(&b'\n');
            let content = if ended {
                &segment[..segment.len() - 1]
            } else {
                segment
            };
            if self.pending.len().saturating_add(content.len()) > self.limit.saturating_add(1) {
                return self.fail(NativeCoreError::FrameTooLarge);
            }
            self.pending.extend_from_slice(content);
            if self.pending.len() > self.limit && self.pending.last() != Some(&b'\r') {
                return self.fail(NativeCoreError::FrameTooLarge);
            }
            if ended {
                match self.complete_line(false) {
                    Ok(Some(frame)) => frames.push(frame),
                    Ok(None) => {}
                    Err(error) => return self.fail(error),
                }
            }
        }
        Ok(frames)
    }
    pub fn finish(&mut self) -> Result<Vec<NativeFrame>, NativeCoreError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        match self.complete_line(true) {
            Ok(frame) => Ok(frame.into_iter().collect()),
            Err(error) => self.fail(error),
        }
    }
    fn complete_line(&mut self, at_eof: bool) -> Result<Option<NativeFrame>, NativeCoreError> {
        let mut bytes = std::mem::take(&mut self.pending);
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        if bytes.len() > self.limit {
            return Err(NativeCoreError::FrameTooLarge);
        }
        if bytes.iter().all(|byte| byte.is_ascii_whitespace()) {
            return Ok(None);
        }
        let raw = String::from_utf8(bytes).map_err(|_| NativeCoreError::InvalidUtf8)?;
        NativeFrame::parse(raw).map(Some).map_err(|error| {
            if at_eof && error == NativeCoreError::MalformedFrame {
                NativeCoreError::IncompleteFrame
            } else {
                error
            }
        })
    }
    fn fail<T>(&mut self, error: NativeCoreError) -> Result<T, NativeCoreError> {
        self.failed = Some(error);
        self.pending.clear();
        Err(error)
    }
}

pub(super) fn request_frame(
    id: &RpcId,
    method: &str,
    params: Option<&RawJson>,
    limit: usize,
) -> Result<Vec<u8>, NativeCoreError> {
    if !valid_routing_text(method) {
        return Err(NativeCoreError::MalformedFrame);
    }
    let method = serde_json::to_string(method).map_err(|_| NativeCoreError::InvalidJson)?;
    let mut raw = format!("{{\"id\":{},\"method\":{method}", id.json());
    if let Some(params) = params {
        raw.push_str(",\"params\":");
        raw.push_str(params.as_str());
    }
    raw.push('}');
    line_bytes(raw, limit)
}
pub(super) fn response_frame(
    id: &RpcId,
    is_error: bool,
    body: &RawJson,
    limit: usize,
) -> Result<Vec<u8>, NativeCoreError> {
    let field = if is_error { "error" } else { "result" };
    line_bytes(
        format!("{{\"id\":{},\"{field}\":{}}}", id.json(), body.as_str()),
        limit,
    )
}

fn line_bytes(mut raw: String, limit: usize) -> Result<Vec<u8>, NativeCoreError> {
    if raw.len() > limit {
        return Err(NativeCoreError::FrameTooLarge);
    }
    raw.push('\n');
    Ok(raw.into_bytes())
}

fn valid_routing_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ROUTING_TEXT_BYTES
        && !value.chars().any(char::is_control)
}

#[derive(Default)]
struct FrameHeader {
    id: Option<RpcId>,
    method: Option<String>,
    has_result: bool,
    has_error: bool,
}
impl<'de> Deserialize<'de> for FrameHeader {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HeaderVisitor;
        impl<'de> Visitor<'de> for HeaderVisitor {
            type Value = FrameHeader;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("one RPC object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                use serde::de::Error;
                let mut header = FrameHeader::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "id" => {
                            if header.id.is_some() {
                                return Err(M::Error::custom("duplicate id"));
                            }
                            header.id = Some(map.next_value()?);
                        }
                        "method" => {
                            if header.method.is_some() {
                                return Err(M::Error::custom("duplicate method"));
                            }
                            let method: String = map.next_value()?;
                            if !valid_routing_text(&method) {
                                return Err(M::Error::custom("invalid method"));
                            }
                            header.method = Some(method);
                        }
                        "result" => {
                            if header.has_result {
                                return Err(M::Error::custom("duplicate result"));
                            }
                            header.has_result = true;
                            map.next_value::<IgnoredAny>()?;
                        }
                        "error" => {
                            if header.has_error {
                                return Err(M::Error::custom("duplicate error"));
                            }
                            header.has_error = true;
                            map.next_value::<IgnoredAny>()?;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(header)
            }
        }
        deserializer.deserialize_map(HeaderVisitor)
    }
}

fn terminal_turn(raw: &str) -> Option<TerminalTurn> {
    #[derive(Deserialize)]
    struct Envelope {
        params: Params,
    }
    #[derive(Deserialize)]
    struct Params {
        #[serde(rename = "threadId")]
        thread_id: String,
        turn: Turn,
    }
    #[derive(Deserialize)]
    struct Turn {
        id: String,
        status: String,
    }
    let envelope: Envelope = serde_json::from_str(raw).ok()?;
    let Params { thread_id, turn } = envelope.params;
    if !valid_routing_text(&thread_id)
        || !valid_routing_text(&turn.id)
        || !matches!(turn.status.as_str(), "completed" | "failed" | "interrupted")
    {
        return None;
    }
    Some(TerminalTurn {
        native_thread_id: thread_id,
        native_turn_id: turn.id,
        status: turn.status,
    })
}
