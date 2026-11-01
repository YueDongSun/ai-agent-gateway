import type { ChatCompletionRequest } from '../../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../canonical.js';
import { BaseProvider, makeProviderError } from './base.js';
import { upstream_http_error } from './error_body.js';
import { makeStreamErrorChunk } from '../canonical.js';
import { logger, safeLogRef } from '../../util/log.js';
import { endpointUrl, modelsUrls } from './endpoint.js';
import { read_sse_lines } from '../sse_lines.js';

/**
 * OpenAI-compatible provider.
 * Works with any endpoint that speaks /v1/chat/completions
 * (OpenAI, DeepSeek, Kimi/Moonshot, together, ollama, vLLM, ...).
 */
export class OpenAIProvider extends BaseProvider {
  private endpoint(): string {
    // Shared normalization: a base stored as a full
    // `.../v1/chat/completions` endpoint is used as-is instead of being
    // doubled, and a bare root gains `/v1` (see endpoint.ts).
    return endpointUrl(this.config.baseURL, 'openai');
  }

  async complete(req: ChatCompletionRequest, signal?: AbortSignal): Promise<OAIChatResponse> {
    const url = this.endpoint();
    let res: Response;
    try {
      res = await fetch(url, {
        method: 'POST',
        headers: this.headers(),
        body: JSON.stringify({ ...req, stream: false }),
        signal: signal ? AbortSignal.any([signal, this.timeoutSignal()]) : this.timeoutSignal(),
      });
    } catch (e) {
      // A client abort must reach the router as an abort, not as a retriable
      // transport failure: the router settles it as a cancellation and never
      // fails over for it. Provider timeouts keep the transport_error wrap.
      if (signal?.aborted && e instanceof Error && e.name === 'AbortError') throw e;
      throw makeProviderError('upstream_transport_error', {
        retriable: true,
        stage: 'request_send',
      });
    }
    if (!res.ok) {
      throw await upstream_http_error(res, 'response_status');
    }
    try {
      return (await res.json()) as OAIChatResponse;
    } catch (e) {
      // Decode path follows the same client-abort rule as request_send: an
      // AbortError from the body read is a client disconnect, not a provider
      // fault, and must not trigger failover or count against the breaker.
      if (signal?.aborted && e instanceof Error && e.name === 'AbortError') throw e;
      throw makeProviderError('upstream_decode_error', {
        status: 502,
        retriable: true,
        stage: 'response_decode',
      });
    }
  }

  async *stream(
    req: ChatCompletionRequest,
    signal?: AbortSignal,
  ): AsyncGenerator<OAIStreamChunk> {
    const url = this.endpoint();
    // Header-phase timeout only: cancel once the fetch settles so a healthy
    // long stream is never truncated mid-body (Rust proxy parity; the router
    // bounds time-to-first-chunk instead). The outer signal stays armed for
    // the whole stream so the router can abort an abandoned hop's body
    // read; dispose() detaches it when the stream ends.
    const attempt = this.headerTimeoutController(signal);
    try {
      let res: Response;
      try {
        res = await fetch(url, {
          method: 'POST',
          headers: this.headers(),
          body: JSON.stringify({ ...req, stream: true }),
          signal: attempt.signal,
        });
      } catch {
        throw makeProviderError('upstream_transport_error', {
          retriable: true,
          stage: 'stream_connect',
        });
      } finally {
        attempt.cancel();
      }
      if (!res.ok || !res.body) {
        throw await upstream_http_error(res, 'stream_status', 502);
      }
      yield* parseSSE<OAIStreamChunk>(res.body, this.id);
    } finally {
      attempt.dispose();
    }
  }

  private headers(): Record<string, string> {
    const h: Record<string, string> = {
      'content-type': 'application/json',
    };
    if (this.config.apiKey) {
      h['authorization'] = `Bearer ${this.config.apiKey}`;
    }
    return h;
  }

  async health(): Promise<boolean> {
    // Probe the first model-catalog candidate; full-endpoint bases are
    // reduced to their root before /models is appended (see endpoint.ts).
    const url = modelsUrls(this.config.baseURL || 'https://api.openai.com/v1', 'openai')[0];
    if (!url) return false;
    try {
      const res = await fetch(url, {
        headers: this.headers(),
        signal: AbortSignal.timeout(5000),
      });
      return res.ok;
    } catch {
      return false;
    }
  }
}

/** Malformed data lines terminate explicitly rather than looking like clean completion. */
export async function* parseSSE<T>(
  body: ReadableStream<Uint8Array>,
  tag: string,
): AsyncGenerator<T> {
  for await (const line of read_sse_lines(body)) {
    if (!line.startsWith('data:')) continue;
    const data = line.slice(5).trim();
    if (data === '[DONE]') return;
    if (!data) continue;
    try {
      yield JSON.parse(data) as T;
    } catch {
      logger.debug('openai', 'sse_parse_failed', {
        provider_ref: safeLogRef(tag),
        data_bytes: Buffer.byteLength(data),
        stage: 'stream_decode',
      });
      yield makeStreamErrorChunk(502, 'proxy_stream_error') as unknown as T;
      return;
    }
  }
}
