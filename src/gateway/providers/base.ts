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

  /** Non-streaming completion. The optional signal lets the caller abort the
   * underlying fetch on client disconnect (same contract as `stream`). */
  abstract complete(
    req: ChatCompletionRequest,
    signal?: AbortSignal,
  ): Promise<OAIChatResponse>;

  /** Streaming completion as an async generator of canonical chunks. The
   * optional signal lets the router abort the underlying fetch when it
   * abandons the stream (first-byte timeout); the generator then ends. */
  abstract stream(
    req: ChatCompletionRequest,
    signal?: AbortSignal,
  ): AsyncGenerator<OAIStreamChunk>;

  /** Lightweight liveness probe. */
  async health(): Promise<boolean> {
    return true;
  }

  protected timeoutSignal(): AbortSignal {
    return AbortSignal.timeout(this.config.timeoutMs);
  }

  /**
   * Abort the request if response headers have not arrived within the
   * configured timeout. `cancel()` stops only the header-phase timer (call
   * it once the fetch settles) so a healthy long stream is never truncated
   * mid-body; `dispose()` additionally detaches the outer abort listener
   * (call it when the stream ends). Keeping the outer listener attached
   * after `cancel()` is what lets the router's first-byte timeout — which
   * fires after headers — actually abort the body read of an abandoned
   * hop, mirroring the Rust reference dropping the upstream future.
   */
  protected headerTimeoutController(outer?: AbortSignal): {
    signal: AbortSignal;
    cancel: () => void;
    dispose: () => void;
  } {
    const controller = new AbortController();
    const onOuterAbort = () => controller.abort();
    if (outer) {
      if (outer.aborted) controller.abort();
      else outer.addEventListener('abort', onOuterAbort, { once: true });
    }
    const timer = setTimeout(() => controller.abort(), this.config.timeoutMs);
    return {
      signal: controller.signal,
      cancel: () => clearTimeout(timer),
      dispose: () => {
        clearTimeout(timer);
        outer?.removeEventListener('abort', onOuterAbort);
      },
    };
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
