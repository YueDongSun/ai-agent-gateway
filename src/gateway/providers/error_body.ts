/**
 * Bounded upstream error-body reads, mirroring
 * `read_capped_error_body` / `MAX_ERROR_BODY_BYTES` in
 * `src-tauri/src/proxy/providers/http_client.rs`.
 *
 * Error bodies are read only for 4xx classification (whose scan window is
 * the first 8 KiB) and for the recorded size. The inbound request-size
 * limit does not apply to upstream responses, so a hostile or
 * misconfigured upstream returning a giant 4xx/5xx page must never be
 * buffered whole. The recorded size is the observed (capped) byte count,
 * never a fabricated total.
 */
import { classifyUpstream4xx, makeProviderError } from '../canonical.js';

export const max_error_body_bytes = 64 * 1024;

export interface Capped_error_body {
  text: string;
  response_bytes: number;
}

export async function read_capped_error_body(response: Response): Promise<Capped_error_body> {
  if (!response.body) return { text: '', response_bytes: 0 };
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let observed = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done || !value.length) break;
      const take = Math.min(value.length, max_error_body_bytes - observed);
      chunks.push(value.subarray(0, take));
      observed += take;
      if (observed >= max_error_body_bytes) break;
    }
  } catch {
    // Keep what was observed; a truncated read classifies as unclassified.
  } finally {
    try {
      await reader.cancel();
    } catch {
      // The upstream body is already gone; nothing to release.
    }
  }
  const merged = new Uint8Array(observed);
  let offset = 0;
  for (const chunk of chunks) {
    merged.set(chunk, offset);
    offset += chunk.length;
  }
  // Lossy decode: non-UTF-8 bodies stay unclassified instead of throwing.
  return { text: new TextDecoder().decode(merged), response_bytes: observed };
}

/**
 * Shared non-2xx handling for both Node providers: cap the error body,
 * classify the 4xx from the capped text, and report the observed size.
 * `fallback_status` is used when the platform reports status 0 on an
 * already-lost connection.
 */
export async function upstream_http_error(
  response: Response,
  stage: string,
  fallback_status = response.status,
): Promise<never> {
  const status = response.status || fallback_status;
  const { text, response_bytes } = await read_capped_error_body(response);
  throw makeProviderError('upstream_http_error', {
    status,
    retriable: status >= 500 || status === 429,
    stage,
    responseBytes: response_bytes,
    kind: classifyUpstream4xx(status, text),
  });
}
