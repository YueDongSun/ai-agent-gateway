//! SSE line parser - shared by OpenAI and Anthropic streaming providers.
//!
//! Ported from `src/gateway/providers/openai.ts::parseSSE` and the Anthropic
//! `readLines` helper. Reads a byte stream, splits on newlines, yields `data:`
//! payload strings (or `[DONE]` sentinel handled by caller).

use bytes::Bytes;
use futures_core::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Stream of raw `data:` payload strings from an SSE byte stream.
/// Empty `data:` lines are skipped. `[DONE]` is yielded as-is so callers can
/// decide how to handle it.
pub struct SseDataStream<S> {
    inner: S,
    buffer: String,
}

impl<S> SseDataStream<S>
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    pub fn new(inner: S) -> Self {
        Self { inner, buffer: String::new() }
    }
}

impl<S> Stream for SseDataStream<S>
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    type Item = String;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        use futures_util::StreamExt;
        loop {
            // Try to extract a complete line from the buffer first.
            if let Some(nl) = self.buffer.find('\n') {
                let line = self.buffer[..nl].trim_end_matches('\r').to_string();
                self.buffer = self.buffer[nl + 1..].to_string();
                if let Some(data) = line.strip_prefix("data:") {
                    let trimmed = data.trim();
                    if trimmed.is_empty() {
                        continue; // skip empty data lines
                    }
                    return Poll::Ready(Some(trimmed.to_string()));
                }
                // non-data: line (event:, id:, comment) - skip
                continue;
            }
            // Need more bytes from upstream.
            match self.inner.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    self.buffer.push_str(&String::from_utf8_lossy(&chunk));
                    continue;
                }
                Poll::Ready(Some(Err(_))) => {
                    return Poll::Ready(None);
                }
                Poll::Ready(None) => {
                    // Flush any trailing buffered line.
                    let remaining = std::mem::take(&mut self.buffer);
                    let line = remaining.trim_end_matches('\r').to_string();
                    if let Some(data) = line.strip_prefix("data:") {
                        let trimmed = data.trim();
                        if !trimmed.is_empty() {
                            return Poll::Ready(Some(trimmed.to_string()));
                        }
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}