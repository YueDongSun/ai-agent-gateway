import type { ChatCompletionRequest } from '../../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../canonical.js';
import { BaseProvider, makeError } from './base.js';
import { logger } from '../../util/log.js';
import {
  oaiReqToAnthropicReq,
  anthropicRespToOAIResp,
  anthropicStreamToOAIChunks,
  type AnthropicResponse,
} from '../converter.js';

/**
 * Anthropic-native provider.
 * Speaks POST /v1/messages. Converts canonical OpenAI <-> Anthropic.
 */
export class AnthropicProvider extends BaseProvider {
  private endpoint(): string {
    let base = this.config.baseURL.replace(/\/+$/, '');
    if (!base) base = 'https://api.anthropic.com';
    if (!base.endsWith('/v1')) base += '/v1';
    return `${base}/messages`;
  }

  async complete(req: ChatCompletionRequest): Promise<OAIChatResponse> {
    const body = oaiReqToAnthropicReq(req);
    body.stream = false;
    const res = await fetch(this.endpoint(), {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify(body),
      signal: this.timeoutSignal(),
    });
    if (!res.ok) {
      const text = await res.text().catch(() => '');
      throw makeError(`${this.id} HTTP ${res.status}: ${text.slice(0, 300)}`, {
        status: res.status,
        retriable: res.status >= 500 || res.status === 429,
      });
    }
    const resp = (await res.json()) as AnthropicResponse;
    return anthropicRespToOAIResp(resp, req.model);
  }

  async *stream(req: ChatCompletionRequest): AsyncGenerator<OAIStreamChunk> {
    const body = oaiReqToAnthropicReq(req);
    body.stream = true;
    const res = await fetch(this.endpoint(), {
      method: 'POST',
      headers: { ...this.headers(), accept: 'text/event-stream' },
      body: JSON.stringify(body),
      signal: this.timeoutSignal(),
    });
    if (!res.ok || !res.body) {
      const text = await res.text().catch(() => '');
      throw makeError(`${this.id} stream HTTP ${res.status}: ${text.slice(0, 300)}`, {
        status: res.status,
        retriable: res.status >= 500 || res.status === 429,
      });
    }
    // Anthropic SSE: "event: X\ndata: {...}\n\n"
    yield* anthropicStreamToOAIChunks(readLines(res.body), req.model);
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
}

async function* readLines(
  body: ReadableStream<Uint8Array>,
): AsyncGenerator<string> {
  const decoder = new TextDecoder();
  let buf = '';
  for await (const chunk of body as any) {
    buf += decoder.decode(chunk as Uint8Array, { stream: true });
    let idx: number;
    while ((idx = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, idx).replace(/\r$/, '');
      buf = buf.slice(idx + 1);
      yield line;
    }
  }
  if (buf.trim()) yield buf.replace(/\r$/, '');
  logger.debug('anthropic', 'stream ended');
}
