import { makeProviderError } from './canonical.js';

/**
 * Mirrors `MAX_EVENT_BYTES` in `src-tauri/src/proxy/providers/sse.rs`: the
 * Rust decoder bounds an entire SSE event's data at 8 MiB, and a single
 * line can never legitimately exceed the event that contains it. This is a
 * per-line bound only, not an overall response-size or process-memory cap.
 */
export const max_sse_line_bytes = 8 * 1024 * 1024;

/** Dispatch only complete SSE events and bound multi-line data cumulatively. */
export async function* read_sse_events(
  body: ReadableStream<Uint8Array>,
  max_event_bytes = max_sse_line_bytes,
): AsyncGenerator<{ event: string; data: string }> {
  let event = 'message';
  let data: string[] = [];
  let event_bytes = 0;
  for await (const line of read_sse_lines(body, max_event_bytes)) {
    if (!line) {
      if (data.length || event !== 'message') yield { event, data: data.join('\n') };
      event = 'message';
      data = [];
      event_bytes = 0;
      continue;
    }
    if (line.startsWith(':')) continue;
    const separator = line.indexOf(':');
    const field = separator < 0 ? line : line.slice(0, separator);
    const value = separator < 0 ? '' : line.slice(separator + 1).replace(/^ /, '');
    if (field !== 'data' && field !== 'event') continue;
    event_bytes += Buffer.byteLength(value, 'utf8') + 1;
    if (event_bytes > max_event_bytes) throw new Error('SSE event exceeds the supported size');
    if (field === 'event') event = value;
    else data.push(value);
  }
  // An event lacking its terminating empty line is incomplete at EOF.
}

export async function* read_sse_lines(
  body: ReadableStream<Uint8Array>,
  max_line_bytes = max_sse_line_bytes,
): AsyncGenerator<string> {
  const reader = body.getReader();
  const decoder = new TextDecoder('utf-8', { fatal: true });
  let pending = '';
  let pending_bytes = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      let offset = 0;
      while (offset < value.length) {
        const newline = value.indexOf(10, offset);
        const end = newline === -1 ? value.length : newline;
        pending_bytes += end - offset;
        if (pending_bytes > max_line_bytes) {
          throw makeProviderError('upstream_decode_error', {
            status: 502, retriable: true, stage: 'stream_line_limit',
          });
        }
        pending += decoder.decode(value.subarray(offset, end), { stream: true });
        if (newline === -1) break;
        pending += decoder.decode();
        yield pending.replace(/\r$/, '');
        pending = '';
        pending_bytes = 0;
        offset = end + 1;
      }
    }
    pending += decoder.decode();
    if (pending) yield pending.replace(/\r$/, '');
  } finally {
    try { await reader.cancel(); } finally { reader.releaseLock(); }
  }
}
