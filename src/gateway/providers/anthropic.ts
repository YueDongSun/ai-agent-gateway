import type { ChatCompletionRequest } from '../../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../canonical.js';
import { BaseProvider, makeProviderError } from './base.js';
import { upstream_http_error } from './error_body.js';
import { read_sse_lines } from '../sse_lines.js';
import {
  oaiReqToAnthropicReq,
  anthropicRespToOAIResp,
  anthropicStreamToOAIChunks,
  type AnthropicResponse,
} from '../converter.js';
import { endpointUrl, anthropicCountTokensUrl } from './endpoint.js';
import { anthropic_requests, anthropic_responses } from '../anthropic_envelope.js';
import { strip_one_m_suffix } from '../router.js';

/**
 * Anthropic-native provider.
 * Speaks POST /v1/messages. Converts canonical OpenAI <-> Anthropic.
 */
export class AnthropicProvider extends BaseProvider {
  private endpoint(): string {
    // Shared normalization: a base stored as a full `.../v1/messages`
    // endpoint is used as-is instead of being doubled (see endpoint.ts).
    return endpointUrl(this.config.baseURL, 'anthropic');
  }

  async complete(req: ChatCompletionRequest, signal?: AbortSignal): Promise<OAIChatResponse> {
    const source = anthropic_requests.get(req);
    const body = source ? { ...source, model: req.model } : oaiReqToAnthropicReq(req);
    body.stream = false;
    let res: Response;
    try {
      res = await fetch(this.endpoint(), {
        method: 'POST',
        headers: this.headers(),
        body: JSON.stringify(body),
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
    let resp: AnthropicResponse;
    try {
      const decoded: unknown = await res.json();
      if (!is_anthropic_response(decoded)) throw new Error('invalid_anthropic_response');
      resp = decoded;
    } catch (e) {
      // Decode path follows the same client-abort rule as request_send.
      if (signal?.aborted && e instanceof Error && e.name === 'AbortError') throw e;
      throw makeProviderError('upstream_decode_error', {
        status: 502,
        retriable: true,
        stage: 'response_decode',
      });
    }
    const canonical = anthropicRespToOAIResp(resp, req.model);
    if (source) anthropic_responses.set(canonical, resp);
    return canonical;
  }

  async *stream(
    req: ChatCompletionRequest,
    signal?: AbortSignal,
  ): AsyncGenerator<OAIStreamChunk> {
    const source = anthropic_requests.get(req);
    const body = source ? { ...source, model: req.model } : oaiReqToAnthropicReq(req);
    body.stream = true;
    // Header-phase timeout only: cancel once the fetch settles so a healthy
    // long stream is never truncated mid-body (Rust proxy parity; the router
    // bounds time-to-first-chunk instead). The outer signal stays armed for
    // the whole stream so the router can abort an abandoned hop's body
    // read; dispose() detaches it when the stream ends.
    const attempt = this.headerTimeoutController(signal);
    try {
      let res: Response;
      try {
        res = await fetch(this.endpoint(), {
          method: 'POST',
          headers: { ...this.headers(), accept: 'text/event-stream' },
          body: JSON.stringify(body),
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
        throw await upstream_http_error(res, 'stream_status');
      }
      // Anthropic SSE: "event: X\ndata: {...}\n\n"
      yield* anthropicStreamToOAIChunks(read_sse_lines(res.body), req.model, Boolean(source));
    } finally {
      attempt.dispose();
    }
  }

  private headers(): Record<string, string> {
    const h: Record<string, string> = {
      'content-type': 'application/json',
      'anthropic-version': '2023-06-01',
    };
    if (this.config.apiKey) {
      h['x-api-key'] = this.config.apiKey;
    }
    return h;
  }

  /** Exact token count via the upstream count_tokens endpoint (Rust
   * parity: anthropic.rs `count_tokens` — the relayed body carries the
   * hop's upstream model with the local `[1M]` marker stripped). */
  async count_tokens(
    body: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<number> {
    signal?.throwIfAborted();
    const url = anthropicCountTokensUrl(this.endpoint());
    if (!url) {
      throw makeProviderError('count_tokens_endpoint_invalid', {
        status: 502,
        retriable: false,
        stage: 'request_construction',
      });
    }
    const owned = { ...body };
    if (typeof owned.model === 'string') {
      owned.model = strip_one_m_suffix(owned.model);
    }
    // Keep the timer and caller cancellation attached through body decoding;
    // receiving headers alone does not complete a token-count request.
    const cleanup_abort = new AbortController();
    const attempt = this.headerTimeoutController(signal
      ? AbortSignal.any([signal, cleanup_abort.signal]) : cleanup_abort.signal);
    let stage = 'request_dispatch';
    let res: Response | undefined;
    try {
      res = await fetch(url, {
        method: 'POST',
        headers: this.headers(),
        body: JSON.stringify(owned),
        signal: attempt.signal,
      });
      signal?.throwIfAborted();
      stage = 'response_status';
      if (!res.ok) {
        throw makeProviderError('upstream_http_error', {
          status: res.status,
          retriable: res.status >= 500 || res.status === 429,
          stage,
        });
      }
      stage = 'response_parse';
      const parsed: unknown = await res.json();
      signal?.throwIfAborted();
      if (!parsed || typeof parsed !== 'object'
        || !('input_tokens' in parsed)
        || typeof parsed.input_tokens !== 'number'
        || !Number.isSafeInteger(parsed.input_tokens) || parsed.input_tokens < 0) {
        throw makeProviderError('count_tokens_response_invalid', {
          status: 502,
          retriable: true,
          stage,
        });
      }
      return parsed.input_tokens;
    } catch (error) {
      // Preserve caller cancellation even when its reason is not AbortError.
      signal?.throwIfAborted();
      if (attempt.signal.aborted) {
        throw makeProviderError('upstream_timeout', {
          status: 504,
          retriable: true,
          stage,
        });
      }
      if (error instanceof Error && 'retriable' in error) throw error;
      throw makeProviderError(stage === 'response_parse'
        ? 'count_tokens_response_invalid' : 'upstream_transport_error', {
        status: 502,
        retriable: true,
        stage,
      });
    } finally {
      // An HTTP error may arrive with a body that never ends. Reclaim that
      // unread body and abort the owned transport before returning/retrying;
      // do not await an unbounded upstream drain or cancel hook.
      if (res?.body && !res.body.locked) void res.body.cancel().catch(() => {});
      cleanup_abort.abort();
      attempt.dispose();
    }
  }
}

function is_anthropic_response(value: unknown): value is AnthropicResponse {
  if (!value || typeof value !== 'object') return false;
  const body = value as Partial<AnthropicResponse>;
  return body.type === 'message' && body.role === 'assistant'
    && typeof body.id === 'string' && body.id.length > 0
    && typeof body.model === 'string' && body.model.length > 0
    && Array.isArray(body.content)
    && body.content.every((block) => block && typeof block === 'object' && typeof block.type === 'string')
    && (body.stop_reason === null || typeof body.stop_reason === 'string')
    && (body.stop_sequence === null || typeof body.stop_sequence === 'string')
    && typeof body.usage?.input_tokens === 'number' && Number.isFinite(body.usage.input_tokens)
    && typeof body.usage?.output_tokens === 'number' && Number.isFinite(body.usage.output_tokens)
    && body.usage.input_tokens >= 0 && body.usage.output_tokens >= 0;
}
