//! Incremental SSE event decoder shared by upstream providers.
//! UTF-8 is decoded only after a complete line, so arbitrary HTTP chunk
//! boundaries cannot corrupt Unicode text, signatures or tool arguments.

use bytes::{Buf, Bytes, BytesMut};
use futures_core::Stream;
use std::io::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};

const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// One decoded SSE dispatch. Repeated `data:` fields are joined with `\n` as
/// required by the event-stream format. The existing [`SseDataStream`] API
/// intentionally continues to expose only `data` for legacy provider callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub id: Option<String>,
    pub data: String,
}

/// Incremental SSE decoder that retains named-event and id metadata.
pub struct SseEventStream<S> {
    inner: S,
    buffer: BytesMut,
    scan_offset: usize,
    data_lines: Vec<String>,
    event: Option<String>,
    id: Option<String>,
    event_bytes: usize,
    skip_lf: bool,
    finished: bool,
}

impl<S> SseEventStream<S>
where
    S: Stream<Item = Result<Bytes, Error>> + Unpin,
{
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: BytesMut::new(),
            scan_offset: 0,
            data_lines: Vec::new(),
            event: None,
            id: None,
            event_bytes: 0,
            skip_lf: false,
            finished: false,
        }
    }

    fn clear_event(&mut self) {
        self.data_lines.clear();
        self.event = None;
        self.id = None;
        self.event_bytes = 0;
    }

    fn consume_line(&mut self, line: &[u8]) -> Result<Option<SseEvent>, Error> {
        let line = std::str::from_utf8(line).map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                "upstream SSE contains invalid UTF-8",
            )
        })?;
        if line.is_empty() {
            self.event_bytes = 0;
            if self.data_lines.is_empty() {
                self.event = None;
                self.id = None;
                return Ok(None);
            }
            // Common case: a single data line needs no join copy.
            let data = if self.data_lines.len() == 1 {
                self.data_lines.pop().unwrap_or_default()
            } else {
                std::mem::take(&mut self.data_lines).join("\n")
            };
            let event = SseEvent {
                event: self.event.take(),
                id: self.id.take(),
                data,
            };
            self.data_lines.clear();
            self.event_bytes = 0;
            return Ok(Some(event));
        }

        self.event_bytes = self.event_bytes.saturating_add(line.len().saturating_add(1));
        if self.event_bytes > MAX_EVENT_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "upstream SSE event exceeds limit",
            ));
        }

        // A comment is a keepalive and never dispatches an event.
        if line.starts_with(':') {
            return Ok(None);
        }

        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => self.data_lines.push(value.to_owned()),
            "event" => {
                self.event = (!value.is_empty()).then(|| value.to_owned());
            }
            // NUL is forbidden in the SSE last-event-id buffer. Ignore such
            // an id rather than forwarding an invalid field to a client.
            "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
            // `retry` and unknown extension fields do not affect Responses
            // payload semantics and are not part of the retained contract.
            _ => {}
        }
        Ok(None)
    }

    fn fail(&mut self) {
        self.finished = true;
        self.buffer.clear();
        self.scan_offset = 0;
        self.clear_event();
    }
}

impl<S> Stream for SseEventStream<S>
where
    S: Stream<Item = Result<Bytes, Error>> + Unpin,
{
    type Item = Result<SseEvent, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        use futures_util::StreamExt;
        loop {
            if self.skip_lf && !self.buffer.is_empty() {
                if self.buffer[0] == b'\n' {
                    self.buffer.advance(1);
                }
                self.skip_lf = false;
            }
            if let Some(relative_index) = self.buffer[self.scan_offset..]
                .iter()
                .position(|byte| *byte == b'\r' || *byte == b'\n')
            {
                let index = self.scan_offset + relative_index;
                // Splitting advances the buffer without shifting the unread
                // suffix or allocating a copy for every SSE line.
                let line = self.buffer.split_to(index + 1);
                self.scan_offset = 0;
                self.skip_lf = line[index] == b'\r';
                match self.consume_line(&line[..index]) {
                    Ok(Some(event)) => return Poll::Ready(Some(Ok(event))),
                    Ok(None) => continue,
                    Err(error) => {
                        self.fail();
                        return Poll::Ready(Some(Err(error)));
                    }
                }
            }
            // Already inspected bytes cannot contain a delimiter. Resume at
            // the next fragment rather than rescanning a growing partial line.
            self.scan_offset = self.buffer.len();
            if self.finished {
                // SSE requires a blank line to commit the event. A partial
                // terminal event is a protocol failure, never a successful EOF.
                if !self.buffer.is_empty() || !self.data_lines.is_empty() {
                    self.fail();
                    return Poll::Ready(Some(Err(Error::new(
                        ErrorKind::UnexpectedEof,
                        "upstream SSE event is incomplete",
                    ))));
                }
                return Poll::Ready(None);
            }
            match self.inner.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    self.buffer.extend_from_slice(&chunk);
                    if self.buffer.len() > MAX_EVENT_BYTES {
                        self.fail();
                        return Poll::Ready(Some(Err(Error::new(
                            ErrorKind::InvalidData,
                            "upstream SSE buffer exceeds limit",
                        ))));
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    self.fail();
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(None) => self.finished = true,
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Backward-compatible data-only SSE decoder used by Anthropic, Gemini and
/// Chat Completions providers. Named event/id fields are parsed safely but
/// intentionally discarded at this compatibility boundary.
pub struct SseDataStream<S> {
    inner: SseEventStream<S>,
}

impl<S> SseDataStream<S>
where
    S: Stream<Item = Result<Bytes, Error>> + Unpin,
{
    pub fn new(inner: S) -> Self {
        Self {
            inner: SseEventStream::new(inner),
        }
    }
}

impl<S> Stream for SseDataStream<S>
where
    S: Stream<Item = Result<Bytes, Error>> + Unpin,
{
    type Item = Result<String, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.get_mut().inner).poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => Poll::Ready(Some(Ok(event.data))),
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{stream, StreamExt};

    #[tokio::test]
    async fn fragmented_unicode_and_multiline_events_are_lossless() {
        let wire = ":keepalive\r\nevent: content\r\ndata: {\"thinking\":\"推理😀\",\r\ndata: \"signature\":\"opaque\"}\r\n\r\ndata: [DONE]\r\n\r\n";
        let bytes = wire
            .as_bytes()
            .iter()
            .map(|byte| Ok(Bytes::from(vec![*byte])))
            .collect::<Vec<_>>();
        let events = SseDataStream::new(stream::iter(bytes))
            .collect::<Vec<_>>()
            .await;
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].as_ref().unwrap(),
            "{\"thinking\":\"推理😀\",\n\"signature\":\"opaque\"}"
        );
        assert_eq!(events[1].as_ref().unwrap(), "[DONE]");
    }

    #[tokio::test]
    async fn event_name_id_and_multiline_data_survive_every_fragment_boundary() {
        let wire = "event: response.output_text.delta\r\nid: resp_opaque\r\ndata: {\"type\":\"response.output_text.delta\",\r\ndata: \"delta\":\"推理😀\"}\r\n\r\n";
        for chunk_bytes in 1..=wire.len() {
            let chunks = wire
                .as_bytes()
                .chunks(chunk_bytes)
                .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
                .collect::<Vec<_>>();
            let events = SseEventStream::new(stream::iter(chunks))
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].event.as_deref(), Some("response.output_text.delta"));
            assert_eq!(events[0].id.as_deref(), Some("resp_opaque"));
            assert_eq!(
                events[0].data,
                "{\"type\":\"response.output_text.delta\",\n\"delta\":\"推理😀\"}"
            );
        }
    }

    #[tokio::test]
    async fn body_error_and_incomplete_event_are_explicit() {
        let events = SseDataStream::new(stream::iter(vec![
            Ok(Bytes::from_static(b"data: partial")),
            Err(Error::new(ErrorKind::ConnectionReset, "fixture")),
        ]))
        .collect::<Vec<_>>()
        .await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].as_ref().unwrap_err().kind(),
            ErrorKind::ConnectionReset
        );
        let events = SseDataStream::new(stream::iter(vec![Ok(Bytes::from_static(
            b"data: partial\n",
        ))]))
        .collect::<Vec<_>>()
        .await;
        assert_eq!(
            events[0].as_ref().unwrap_err().kind(),
            ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn coalesced_events_preserve_order_without_reading_ahead() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let expected_events = (0..4096)
            .map(|index| format!("{{\"index\":{index},\"signature\":\"opaque_😀\"}}"))
            .collect::<Vec<_>>();
        let wire = expected_events
            .iter()
            .map(|event| format!("data: {event}\r\n\r\n"))
            .collect::<String>();
        let read_count = Arc::new(AtomicUsize::new(0));
        let observed_reads = Arc::clone(&read_count);
        let inner = stream::iter(vec![
            Ok(Bytes::from(wire)),
            Ok(Bytes::from_static(b"data: tail\n\n")),
        ])
        .map(move |chunk| {
            observed_reads.fetch_add(1, Ordering::SeqCst);
            chunk
        });
        let mut decoded = SseDataStream::new(inner);
        for expected_event in expected_events {
            assert_eq!(decoded.next().await.unwrap().unwrap(), expected_event);
            assert_eq!(read_count.load(Ordering::SeqCst), 1);
        }
        assert_eq!(decoded.next().await.unwrap().unwrap(), "tail");
        assert_eq!(read_count.load(Ordering::SeqCst), 2);
        assert!(decoded.next().await.is_none());
    }

    #[tokio::test]
    async fn mixed_line_endings_are_lossless_at_every_chunk_boundary() {
        let wire =
            ":ping\r\nevent: delta\rdata: 推理😀\ndata: opaque\r\n\r\ndata:\r\rdata: tail\n\n";
        for chunk_bytes in 1..=wire.len() {
            let chunks = wire
                .as_bytes()
                .chunks(chunk_bytes)
                .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
                .collect::<Vec<_>>();
            let decoded = SseDataStream::new(stream::iter(chunks))
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(decoded, ["推理😀\nopaque", "", "tail"]);
        }
    }

    #[tokio::test]
    async fn long_fragmented_tool_data_is_lossless() {
        let expected_event = format!("{{\"arguments\":\"{}\"}}", "工具😀".repeat(8192));
        let wire = format!("data: {expected_event}\r\n\r\n");
        let chunks = wire
            .as_bytes()
            .chunks(127)
            .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
            .collect::<Vec<_>>();
        let mut decoded = SseDataStream::new(stream::iter(chunks));
        assert_eq!(decoded.next().await.unwrap().unwrap(), expected_event);
        assert!(decoded.next().await.is_none());
    }

    #[tokio::test]
    async fn pending_fragments_and_cr_only_eof_are_lossless() {
        let mut poll_step = 0;
        let inner = stream::poll_fn(move |context| {
            poll_step += 1;
            match poll_step {
                1 => Poll::Ready(Some(Ok(Bytes::from_static(b"data: tool_")))),
                2 | 4 => {
                    context.waker().wake_by_ref();
                    Poll::Pending
                }
                3 => Poll::Ready(Some(Ok(Bytes::from_static(b"arguments\r")))),
                5 => Poll::Ready(Some(Ok(Bytes::from_static(b"\n\r")))),
                _ => Poll::Ready(None),
            }
        });
        let mut decoded = SseDataStream::new(inner);
        assert_eq!(decoded.next().await.unwrap().unwrap(), "tool_arguments");
        assert!(decoded.next().await.is_none());
    }

    #[tokio::test]
    async fn body_error_after_pending_fragments_is_terminal() {
        let mut poll_step = 0;
        let inner = stream::poll_fn(move |context| {
            poll_step += 1;
            match poll_step {
                1 => Poll::Ready(Some(Ok(Bytes::from_static(b"data: partial")))),
                2 => {
                    context.waker().wake_by_ref();
                    Poll::Pending
                }
                3 => Poll::Ready(Some(Ok(Bytes::from_static(b"_continued")))),
                _ => Poll::Ready(Some(Err(Error::new(ErrorKind::ConnectionReset, "fixture")))),
            }
        });
        let mut decoded = SseDataStream::new(inner);
        assert_eq!(
            decoded.next().await.unwrap().unwrap_err().kind(),
            ErrorKind::ConnectionReset
        );
        assert!(decoded.next().await.is_none());
    }

    #[tokio::test]
    async fn multiline_event_limit_and_errors_remain_terminal() {
        let line = Bytes::from(format!("data: {}\n", "x".repeat(1024 * 1024)));
        let chunks = (0..9)
            .map(|_| Ok(line.clone()))
            .chain(std::iter::once(Ok(Bytes::from_static(b"\ndata: tail\n\n"))))
            .collect::<Vec<_>>();
        let mut decoded = SseDataStream::new(stream::iter(chunks));
        assert_eq!(
            decoded.next().await.unwrap().unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        assert!(decoded.next().await.is_none());

        let mut invalid = SseDataStream::new(stream::iter(vec![Ok(Bytes::from_static(
            b"data: \xff\n\ndata: tail\n\n",
        ))]));
        assert_eq!(
            invalid.next().await.unwrap().unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        assert!(invalid.next().await.is_none());
    }

    #[tokio::test]
    async fn invalid_utf8_and_oversize_events_fail_explicitly() {
        let invalid = SseDataStream::new(stream::iter(vec![Ok(Bytes::from_static(
            b"data: \xff\n\n",
        ))]))
        .collect::<Vec<_>>()
        .await;
        assert_eq!(
            invalid[0].as_ref().unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        let oversized = SseDataStream::new(stream::iter(vec![Ok(Bytes::from(vec![
            b'x';
            MAX_EVENT_BYTES
                + 1
        ]))]))
        .collect::<Vec<_>>()
        .await;
        assert_eq!(
            oversized[0].as_ref().unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }
}
