//! Length-prefixed UTF-8 RPC envelopes. Routing is validated without rewriting
//! opaque payload values, large numbers, whitespace, or unknown fields.

use super::TransportError;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use std::{collections::HashSet, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvelopeKind {
    Request,
    Notification,
    Response,
}

/// No Debug/Serde/Clone: callers deliberately access the original bytes.
pub(crate) struct TransportFrame {
    bytes: Vec<u8>,
    kind: EnvelopeKind,
}
impl TransportFrame {
    pub fn parse(bytes: Vec<u8>, limit: usize) -> Result<Self, TransportError> {
        if bytes.len() > limit || bytes.len() > super::MAX_FRAME_BYTES {
            return Err(TransportError::FrameTooLarge);
        }
        let raw = std::str::from_utf8(&bytes).map_err(|_| TransportError::InvalidUtf8)?;
        let mut deserializer = serde_json::Deserializer::from_str(raw);
        let kind = EnvelopeSeed
            .deserialize(&mut deserializer)
            .map_err(|_| TransportError::InvalidFrame)?;
        deserializer
            .end()
            .map_err(|_| TransportError::InvalidFrame)?;
        Ok(Self { bytes, kind })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn kind(&self) -> EnvelopeKind {
        self.kind
    }
}

pub(super) fn encode_length(frame: &TransportFrame) -> [u8; 4] {
    (frame.bytes.len() as u32).to_be_bytes()
}
pub(super) fn decode_length(header: [u8; 4], limit: usize) -> Result<usize, TransportError> {
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 {
        return Err(TransportError::InvalidFrame);
    }
    if length > limit {
        return Err(TransportError::FrameTooLarge);
    }
    Ok(length)
}
#[cfg(test)]
pub(super) fn decode_wire(wire: &[u8], limit: usize) -> Result<TransportFrame, TransportError> {
    let header: [u8; 4] = wire
        .get(..4)
        .ok_or(TransportError::TruncatedFrame)?
        .try_into()
        .unwrap();
    let length = decode_length(header, limit)?;
    if wire.len() < 4 + length {
        return Err(TransportError::TruncatedFrame);
    }
    if wire.len() != 4 + length {
        return Err(TransportError::InvalidFrame);
    }
    TransportFrame::parse(wire[4..].to_vec(), limit)
}

struct EnvelopeSeed;
impl<'de> DeserializeSeed<'de> for EnvelopeSeed {
    type Value = EnvelopeKind;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for EnvelopeSeed {
    type Value = EnvelopeKind;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("one RPC object")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let mut keys = HashSet::new();
        let (mut method, mut id, mut result, mut error, mut params) =
            (false, false, false, false, false);
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom("duplicate envelope field"));
            }
            match key.as_str() {
                "method" => {
                    let value = map.next_value::<String>()?;
                    if value.is_empty() {
                        return Err(de::Error::custom("empty method"));
                    }
                    method = true;
                }
                "id" => {
                    map.next_value_seed(IdSeed)?;
                    id = true;
                }
                "result" => {
                    map.next_value::<IgnoredAny>()?;
                    result = true;
                }
                "error" => {
                    map.next_value::<IgnoredAny>()?;
                    error = true;
                }
                "params" => {
                    map.next_value::<IgnoredAny>()?;
                    params = true;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        match (method, id, result, error, params) {
            (true, true, false, false, _) => Ok(EnvelopeKind::Request),
            (true, false, false, false, _) => Ok(EnvelopeKind::Notification),
            (false, true, true, false, false) | (false, true, false, true, false) => {
                Ok(EnvelopeKind::Response)
            }
            _ => Err(de::Error::custom("ambiguous envelope")),
        }
    }
}
struct IdSeed;
impl<'de> DeserializeSeed<'de> for IdSeed {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for IdSeed {
    type Value = ();
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a string or integer RPC id")
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
}
