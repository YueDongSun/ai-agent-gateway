import type { ProviderConfig, ChatCompletionRequest } from '../../config/schema.js';
import { CircuitBreaker } from '../circuit-breaker.js';
import type { OAIChatResponse, OAIStreamChunk, ProviderError } from '../canonical.js';
import { makeError, makeProviderError } from '../canonical.js';

/** A provider speaks the canonical OpenAI chat-completion format. */
export abstract class BaseProvider {
  readonly config: ProviderConfig;
  readonly breaker: CircuitBreaker;

  constructor(config: ProviderConfig) {
    this.config = config;
    this.breaker = new CircuitBreaker(config.circuitBreaker);
  }

  get id(): string {
    return this.config.id;
  }

  get enabled(): boolean {
    return this.config.enabled;
  }

  models(): string[] {
    return this.config.models;
  }

  /** Non-streaming completion. */
  abstract complete(req: ChatCompletionRequest): Promise<OAIChatResponse>;

  /** Streaming completion as an async generator of canonical chunks. */
  abstract stream(req: ChatCompletionRequest): AsyncGenerator<OAIStreamChunk>;

  /** Lightweight liveness probe. */
  async health(): Promise<boolean> {
    return true;
  }

  protected timeoutSignal(): AbortSignal {
    return AbortSignal.timeout(this.config.timeoutMs);
  }

  protected static extractText(req: ChatCompletionRequest): string {
    const last = req.messages?.[req.messages.length - 1];
    if (!last) return '';
    const c = last.content;
    if (typeof c === 'string') return c;
    if (Array.isArray(c)) {
      return c
        .map((p: any) => (typeof p === 'string' ? p : p?.text ?? ''))
        .join('');
    }
    return '';
  }
}

export { makeError, makeProviderError };
export type { ProviderError };
