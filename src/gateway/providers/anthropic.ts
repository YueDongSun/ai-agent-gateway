import type { ChatCompletionRequest } from '../../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../canonical.js';
import { BaseProvider, makeProviderError } from './base.js';
import { logger } from '../../util/log.js';
import {
  oaiReqToAnthropicReq,
  anthropicRespToOAIResp,
  anthropicStreamToOAIChunks,
  type AnthropicResponse,
} from '../converter.js';
import { endpointUrl } from './endpoint.js';

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

  async complete(req: ChatCompletionRequest): Promise<OAIChatResponse> {
    const body = oaiReqToAnthropicReq(req);
    body.stream = false;
    let res: Response;
    try {
      res = await fetch(this.endpoint(), {
        method: 'POST',
        headers: this.headers(),
        body: JSON.stringify(body),
        signal: this.timeoutSignal(),
      });
    } catch {
      throw makeProviderError('upstream_transport_error', {
        retriable: true,
        stage: 'request_send',
      });
    }
    if (!res.ok) {
      const responseBytes = await responseByteLength(res);
      throw makeProviderError('upstream_http_error', {
        status: res.status,
        retriable: res.status >= 500 || res.status === 429,
        stage: 'response_status',
        responseBytes,
      });
    }
    let resp: AnthropicResponse;
    try {
      resp = (await res.json()) as AnthropicResponse;
    } catch {
      throw makeProviderError('upstream_decode_error', {
        status: 502,
        retriable: true,
        stage: 'response_decode',
      });
    }
    return anthropicRespToOAIResp(resp, req.model);
  }

  async *stream(req: ChatCompletionRequest): AsyncGenerator<OAIStreamChunk> {
    const body = oaiReqToAnthropicReq(req);
    body.stream = true;
    let res: Response;
    try {
      res = await fetch(this.endpoint(), {
        method: 'POST',
        headers: { ...this.headers(), accept: 'text/event-stream' },
        body: JSON.stringify(body),
        signal: this.timeoutSignal(),
      });
    } catch {
      throw makeProviderError('upstream_transport_error', {
        retriable: true,
        stage: 'stream_connect',
      });
    }
    if (!res.ok || !res.body) {
      const responseBytes = await responseByteLength(res);
      throw makeProviderError('upstream_http_error', {
        status: res.status,
        retriable: res.status >= 500 || res.status === 429,
        stage: 'stream_status',
        responseBytes,
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
  logger.debug('anthropic', 'stream_ended');
}

async function responseByteLength(response: Response): Promise<number> {
  try {
    return (await response.arrayBuffer()).byteLength;
  } catch {
    return 0;
  }
}
