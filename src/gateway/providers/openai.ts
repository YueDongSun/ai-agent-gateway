import type { ChatCompletionRequest } from '../../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../canonical.js';
import { BaseProvider, makeProviderError } from './base.js';
import { logger, safeLogRef } from '../../util/log.js';
import { endpointUrl, modelsUrls } from './endpoint.js';

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

  async complete(req: ChatCompletionRequest): Promise<OAIChatResponse> {
    const url = this.endpoint();
    let res: Response;
    try {
      res = await fetch(url, {
        method: 'POST',
        headers: this.headers(),
        body: JSON.stringify({ ...req, stream: false }),
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
    try {
      return (await res.json()) as OAIChatResponse;
    } catch {
      throw makeProviderError('upstream_decode_error', {
        status: 502,
        retriable: true,
        stage: 'response_decode',
      });
    }
  }

  async *stream(req: ChatCompletionRequest): AsyncGenerator<OAIStreamChunk> {
    const url = this.endpoint();
    let res: Response;
    try {
      res = await fetch(url, {
        method: 'POST',
        headers: this.headers(),
        body: JSON.stringify({ ...req, stream: true }),
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
        status: res.status || 502,
        retriable: res.status >= 500 || res.status === 429,
        stage: 'stream_status',
        responseBytes,
      });
    }
    yield* parseSSE<OAIStreamChunk>(res.body, this.id);
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

/** Parse a Server-Sent-Events stream into JSON objects, one per `data:` line. */
export async function* parseSSE<T>(
  body: ReadableStream<Uint8Array>,
  tag: string,
): AsyncGenerator<T> {
  const decoder = new TextDecoder();
  let buffer = '';
  for await (const chunk of body as any) {
    buffer += decoder.decode(chunk as Uint8Array, { stream: true });
    let idx: number;
    while ((idx = buffer.indexOf('\n')) >= 0) {
      let line = buffer.slice(0, idx);
      buffer = buffer.slice(idx + 1);
      line = line.replace(/\r$/, '');
      if (!line.startsWith('data:')) continue;
      const data = line.slice(5).trim();
      if (data === '[DONE]') return;
      if (!data) continue;
      try {
        yield JSON.parse(data) as T;
      } catch {
        logger.debug('openai', 'sse_parse_failed', {
          provider_ref: safeLogRef(tag),
          data_bytes: new TextEncoder().encode(data).byteLength,
          stage: 'stream_decode',
        });
      }
    }
  }
}

async function responseByteLength(response: Response): Promise<number> {
  try {
    return (await response.arrayBuffer()).byteLength;
  } catch {
    return 0;
  }
}
