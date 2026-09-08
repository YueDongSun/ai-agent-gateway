import type { ChatCompletionRequest } from '../../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../canonical.js';
import { BaseProvider, makeError } from './base.js';
import { logger } from '../../util/log.js';

/**
 * OpenAI-compatible provider.
 * Works with any endpoint that speaks /v1/chat/completions
 * (OpenAI, DeepSeek, Kimi/Moonshot, together, ollama, vLLM, ...).
 */
export class OpenAIProvider extends BaseProvider {
  private endpoint(): string {
    let base = this.config.baseURL.replace(/\/+$/, '');
    if (!base) base = 'https://api.openai.com/v1';
    if (!/\/v\d+$/.test(base) && !base.endsWith('/v1')) {
      // don't force /v1 if user already included it
    }
    return `${base}/chat/completions`;
  }

  async complete(req: ChatCompletionRequest): Promise<OAIChatResponse> {
    const url = this.endpoint();
    const res = await fetch(url, {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify({ ...req, stream: false }),
      signal: this.timeoutSignal(),
    });
    if (!res.ok) {
      const body = await res.text().catch(() => '');
      throw makeError(
        `${this.id} HTTP ${res.status}: ${body.slice(0, 300)}`,
        { status: res.status, retriable: res.status >= 500 || res.status === 429 },
      );
    }
    return (await res.json()) as OAIChatResponse;
  }

  async *stream(req: ChatCompletionRequest): AsyncGenerator<OAIStreamChunk> {
    const url = this.endpoint();
    const res = await fetch(url, {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify({ ...req, stream: true }),
      signal: this.timeoutSignal(),
    });
    if (!res.ok || !res.body) {
      const body = await res.text().catch(() => '');
      throw makeError(
        `${this.id} stream HTTP ${res.status}: ${body.slice(0, 300)}`,
        { status: res.status, retriable: res.status >= 500 || res.status === 429 },
      );
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
    try {
      const base = this.config.baseURL.replace(/\/+$/, '');
      const url = `${base}/models`;
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
      } catch (e) {
        logger.debug(tag, `skipping unparseable SSE line: ${data.slice(0, 120)}`);
      }
    }
  }
}
