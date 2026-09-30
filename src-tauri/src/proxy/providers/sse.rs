//! Incremental SSE event decoder shared by upstream providers.
//! UTF-8 is decoded only after a complete line, so arbitrary HTTP chunk
//! boundaries cannot corrupt Unicode text, signatures or tool arguments.

use bytes::Bytes;
use futures_core::Stream;
use std::io::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};

const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

pub struct SseDataStream<S> {
    inner: S,
    buffer: Vec<u8>,
    data_lines: Vec<String>,
    event_bytes: usize,
    skip_lf: bool,
    finished: bool,
}

impl<S> SseDataStream<S>
where
    S: Stream<Item = Result<Bytes, Error>> + Unpin,
{
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
            data_lines: Vec::new(),
            event_bytes: 0,
            skip_lf: false,
            finished: false,
        }
    }

    fn consume_line(&mut self, line: &[u8]) -> Result<Option<String>, Error> {
        let line = std::str::from_utf8(line).map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                "upstream SSE contains invalid UTF-8",
            )
        })?;
        if line.is_empty() {
            self.event_bytes = 0;
            if self.data_lines.is_empty() {
                return Ok(None);
            }
            return Ok(Some(std::mem::take(&mut self.data_lines).join("\n")));
        }
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.strip_prefix(' ').unwrap_or(data);
            self.event_bytes += data.len() + 1;
            if self.event_bytes > MAX_EVENT_BYTES {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "upstream SSE event exceeds limit",
                ));
            }
            self.data_lines.push(data.to_owned());
        }
        Ok(None)
    }
}

impl<S> Stream for SseDataStream<S>
where
    S: Stream<Item = Result<Bytes, Error>> + Unpin,
{
    type Item = Result<String, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        use futures_util::StreamExt;
        loop {
            if self.skip_lf && !self.buffer.is_empty() {
                if self.buffer[0] == b'\n' {
                    self.buffer.remove(0);
                }
                self.skip_lf = false;
            }
            if let Some(index) = self
                .buffer
                .iter()
                .position(|byte| *byte == b'\r' || *byte == b'\n')
            {
                let line: Vec<u8> = self.buffer.drain(..=index).collect();
                self.skip_lf = line[index] == b'\r';
                match self.consume_line(&line[..index]) {
                    Ok(Some(event)) => return Poll::Ready(Some(Ok(event))),
                    Ok(None) => continue,
                    Err(error) => {
                        self.finished = true;
                        self.buffer.clear();
                        self.data_lines.clear();
                        return Poll::Ready(Some(Err(error)));
                    }
                }
            }
            if self.finished {
                // SSE requires a blank line to commit the event. A partial
                // terminal event is a protocol failure, never a successful EOF.
                if !self.buffer.is_empty() || !self.data_lines.is_empty() {
                    self.buffer.clear();
                    self.data_lines.clear();
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
                        self.finished = true;
                        self.buffer.clear();
                        self.data_lines.clear();
                        return Poll::Ready(Some(Err(Error::new(
                            ErrorKind::InvalidData,
                            "upstream SSE buffer exceeds limit",
                        ))));
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    self.finished = true;
                    self.buffer.clear();
                    self.data_lines.clear();
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(None) => self.finished = true,
                Poll::Pending => return Poll::Pending,
            }
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
