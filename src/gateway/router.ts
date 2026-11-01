import type { AppConfig, RouteConfig } from '../config/schema.js';
import type { ChatCompletionRequest } from '../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk, ProviderError } from './canonical.js';
import { makeProviderError, makeStreamErrorChunk, streamChunkError } from './canonical.js';
import { createProvider, type BaseProvider } from './providers/index.js';
import { AnthropicProvider } from './providers/anthropic.js';
import { estimate_input_tokens } from './count_tokens.js';
import { logger, safeLogRef } from '../util/log.js';
import { anthropic_requests, anthropic_responses } from './anthropic_envelope.js';

/** Production default: a working-but-slow upstream with TTFB > 1.5 s must
 * not be killed; this cap only catches open-but-silent streams (Rust proxy
 * parity, `first_byte_timeout_ms`). */
const DEFAULT_FIRST_BYTE_TIMEOUT_MS = 10_000;

/** Claude Code appends this context-window hint to model ids; it is a client
 * marker, never part of an upstream model id (Rust proxy parity,
 * `ONE_M_CONTEXT_MARKER` in src-tauri/src/proxy/mod.rs). */
const ONE_M_CONTEXT_MARKER = '[1M]';

/** Strip the `[1M]` context-window marker (case-insensitive, surrounding
 * whitespace tolerated) so route resolution and provider dispatch see the
 * real model id. */
export function strip_one_m_suffix(model: string): string {
  const trimmed = model.trimEnd();
  if (trimmed.toLowerCase().endsWith(ONE_M_CONTEXT_MARKER.toLowerCase())) {
    return trimmed.slice(0, -ONE_M_CONTEXT_MARKER.length).trimEnd();
  }
  return model;
}

export interface RouterOptions {
  /** Per-hop budget for the first stream chunk. A hop that opens its stream
   * but sends nothing within this budget is treated as a retriable failure
   * so the route advances instead of hanging for the whole request timeout. */
  firstByteTimeoutMs?: number;
}

/**
 * The router resolves a requested model alias to an ordered fallback chain
 * of (provider, model) hops, then executes them with circuit-breaker
 * protection. On a retriable failure (or open circuit) it moves to the
 * next hop automatically.
 */
export class Router {
  private providers = new Map<string, BaseProvider>();
  private routes = new Map<string, RouteConfig>();
  readonly firstByteTimeoutMs: number;

  constructor(cfg: AppConfig, opts: RouterOptions = {}) {
    for (const p of cfg.providers) {
      this.providers.set(p.id, createProvider(p));
    }
    for (const r of cfg.routes) {
      this.routes.set(r.alias, r);
    }
    this.firstByteTimeoutMs = opts.firstByteTimeoutMs ?? DEFAULT_FIRST_BYTE_TIMEOUT_MS;
  }

  getProvider(id: string): BaseProvider | undefined {
    return this.providers.get(id);
  }

  listProviders(): BaseProvider[] {
    return [...this.providers.values()];
  }

  listModels(): string[] {
    return [...this.routes.keys()];
  }

  /** Resolve a client-requested model to its route (alias match first). */
  resolveRoute(model: string): RouteConfig | undefined {
    if (this.routes.has(model)) return this.routes.get(model);
    // fallback: if a provider directly serves this model, build a single-hop route
    for (const p of this.providers.values()) {
      if (p.models().includes(model) && p.enabled) {
        return { alias: model, hops: [{ provider: p.id, model }], description: 'auto' };
      }
    }
    return undefined;
  }

  /** Non-streaming completion with automatic failover. A client abort
   * (`signal`) is a cancellation, not a provider fault: it never fails over
   * and never counts against the breaker. */
  async complete(req: ChatCompletionRequest, signal?: AbortSignal): Promise<OAIChatResponse> {
    const hops = this.hopsFor(req.model);
    if (!hops.length) {
      throw makeProviderError((this.routes.size > 0) ? 'invalid_model' : 'no_route', {
        status: 404,
        retriable: false,
        stage: 'route_resolution',
      });
    }

    let lastErr: ProviderError | undefined;
    for (const hop of hops) {
      const provider = this.providers.get(hop.provider);
      if (!provider || !provider.enabled) {
        logger.warn('router', 'provider_skipped', {
          provider_ref: safeLogRef(hop.provider),
          stage: 'provider_availability',
        });
        continue;
      }
      const admission = provider.breaker.acquire();
      if (!admission) {
        logger.warn('router', 'provider_skipped', {
          provider_ref: safeLogRef(hop.provider),
          stage: 'circuit_open',
        });
        continue;
      }
      try {
        const subReq: ChatCompletionRequest = { ...req, model: hop.model };
        const source = anthropic_requests.get(req);
        if (source) anthropic_requests.set(subReq, source);
        const resp = await provider.complete(subReq, signal);
        admission.settle('success');
        logger.info('router', 'provider_succeeded', {
          provider_ref: safeLogRef(provider.id),
          model_ref: safeLogRef(req.model),
          stage: 'complete',
        });
        const resolved: OAIChatResponse = { ...resp, resolved_model: hop.model };
        // The opaque Anthropic source envelope is registered by response
        // object identity; the copy above breaks it, so carry the registration
        // over like the request side does for `anthropic_requests`.
        const source_resp = anthropic_responses.get(resp);
        if (source_resp) anthropic_responses.set(resolved, source_resp);
        return resolved;
      } catch (e) {
        // Client-abort classification (before provider-error normalization):
        // when the caller's signal is aborted and the upstream failed with an
        // abort, this is the client disconnecting, not a provider fault. Like
        // a terminal 4xx it settles as 'cancelled' without judging provider
        // health, and it must not fail over - every later hop would see the
        // same aborted signal and reject immediately.
        if (signal?.aborted && e instanceof Error && e.name === 'AbortError') {
          admission.settle('cancelled');
          logger.warn('router', 'provider_failed', {
            provider_ref: safeLogRef(provider.id),
            model_ref: safeLogRef(req.model),
            status: 499,
            retriable: false,
            code: 'client_abort',
            stage: 'complete',
            response_bytes: 0,
          });
          throw makeProviderError('client_abort', {
            status: 499,
            retriable: false,
            stage: 'complete',
          });
        }
        lastErr = normalizedProviderError(e, 'complete');
        const failover = lastErr.retriable || shouldFailoverToolCapabilityRejection(req, lastErr);
        // Only retriable (provider-level) failures count against the
        // breaker - terminal 4xx errors are the client's or the credential's
        // fault, not the provider's health (Rust proxy parity). The admission
        // is still settled: a terminal error releases its half-open probe slot
        // without judging the provider.
        admission.settle(failover ? 'failure' : 'cancelled');
        logger.warn('router', 'provider_failed', {
          provider_ref: safeLogRef(provider.id),
          model_ref: safeLogRef(req.model),
          status: lastErr.status ?? 500,
          retriable: failover,
          code: lastErr.code,
          stage: lastErr.stage,
          response_bytes: lastErr.responseBytes ?? 0,
        });
        if (!failover) throw lastErr;
      }
    }
    throw lastErr ?? makeProviderError('all_hops_exhausted', {
      status: 503,
      retriable: false,
      stage: 'routing',
    });
  }

  /** Streaming completion with automatic failover (only pre-first-byte).
   * Mirrors the Rust proxy's `stream_hops`: the first-chunk probe is bounded
   * by `firstByteTimeoutMs`, an empty stream fails over instead of surfacing
   * a blank success, a stream that ends without a terminal chunk surfaces an
   * explicit error chunk, and only retriable failures count against the
   * breaker. Once the first chunk is yielded, mid-stream errors surface to
   * the client (no transparent failover). */
  async *stream(req: ChatCompletionRequest, signal?: AbortSignal): AsyncGenerator<OAIStreamChunk> {
    const hops = this.hopsFor(req.model);
    if (!hops.length) {
      throw makeProviderError((this.routes.size > 0) ? 'invalid_model' : 'no_route', {
        status: 404,
        retriable: false,
        stage: 'route_resolution',
      });
    }

    let lastErr: ProviderError | undefined;
    for (const hop of hops) {
      const provider = this.providers.get(hop.provider);
      if (!provider || !provider.enabled) continue;
      const admission = provider.breaker.acquire();
      if (!admission) {
        logger.warn('router', 'provider_skipped', {
          provider_ref: safeLogRef(hop.provider),
          stage: 'circuit_open',
          stream: true,
        });
        continue;
      }
      let committed = false;
      let terminalSeen = false;
      // The admission settles exactly once (idempotent, generation-guarded).
      // Rust releases the half-open probe slot from a Drop guard so an
      // abandoned attempt cannot strand it; the equivalent here is the
      // `finally` below, which covers both a terminal outcome and a client
      // that stops consuming the generator mid-stream.
      //
      // The upstream attempt handles live above the try so the `finally` can
      // reclaim them: without that, a client disconnect mid-stream would leave
      // the upstream body read running until the provider's own timeout.
      // `upstream_reclaimed` marks paths that already closed the attempt (the
      // explicit reclaims below) or exhausted it (a completed for-await).
      let upstream_reclaimed = false;
      const attemptAbort = new AbortController();
      const abort_attempt = () => attemptAbort.abort();
      if (signal?.aborted) {
        admission.settle('cancelled');
        return;
      }
      signal?.addEventListener('abort', abort_attempt, { once: true });
      let gen: AsyncGenerator<OAIStreamChunk> | undefined;
      try {
        const subReq: ChatCompletionRequest = { ...req, model: hop.model };
        const source = anthropic_requests.get(req);
        if (source) anthropic_requests.set(subReq, source);
        gen = provider.stream(subReq, attemptAbort.signal);
        // Probe the first chunk before committing, bounded by the
        // first-byte budget: a hop that opens a stream but never sends data
        // cannot burn the whole request timeout. Node's async-generator API
        // has no header/chunk phase boundary, so this budget also covers
        // connection setup (a deliberate conservative divergence from the
        // Rust reference, which bounds only time-to-first-chunk).
        const nextPromise = gen.next();
        let timer: ReturnType<typeof setTimeout> | undefined;
        let first: IteratorResult<OAIStreamChunk> | 'timeout';
        try {
          first = await Promise.race([
            nextPromise,
            new Promise<'timeout'>((resolve) => {
              timer = setTimeout(() => resolve('timeout'), this.firstByteTimeoutMs);
            }),
          ]);
        } finally {
          if (timer) clearTimeout(timer);
        }
        if (signal?.aborted) return;
        if (first === 'timeout') {
          // Abort the abandoned attempt and swallow the probe's late (or
          // never-arriving) settlement so it cannot surface as an
          // unhandled rejection.
          attemptAbort.abort();
          nextPromise.catch(() => {});
          void Promise.resolve(gen.return(undefined)).catch(() => {});
          upstream_reclaimed = true;
          admission.settle('failure');
          lastErr = makeProviderError('upstream_timeout', {
            status: 504,
            retriable: true,
            stage: 'stream_first_event',
          });
          logger.warn('router', 'stream_first_event_timeout', {
            provider_ref: safeLogRef(provider.id),
            model_ref: safeLogRef(req.model),
            budget_ms: this.firstByteTimeoutMs,
            code: lastErr.code,
            stage: lastErr.stage,
            stream: true,
          });
          continue;
        }
        if (first.done) {
          // An upstream that opened the stream but yielded zero chunks must
          // not surface as a successful empty completion (a blank 200):
          // record a failure and fail over (Rust proxy parity). The upstream
          // generator has already completed; nothing to reclaim.
          upstream_reclaimed = true;
          admission.settle('failure');
          lastErr = makeProviderError('upstream_stream_error', {
            status: 502,
            retriable: true,
            stage: 'stream_empty',
          });
          logger.warn('router', 'stream_empty', {
            provider_ref: safeLogRef(provider.id),
            model_ref: safeLogRef(req.model),
            code: lastErr.code,
            stage: lastErr.stage,
            stream: true,
          });
          continue;
        }
        const firstError = streamChunkError(first.value);
        if (firstError) {
          // The upstream sent an error event as its first chunk. Retriable
          // (429/5xx) failures count against the breaker and fail over;
          // application errors are terminal and leave the breaker untouched
          // (Rust proxy parity).
          if (firstError.retriable) {
            // The generator sits at its first yield: abort + return reclaims
            // the upstream connection instead of leaving it open.
            attemptAbort.abort();
            void Promise.resolve(gen.return(undefined)).catch(() => {});
            upstream_reclaimed = true;
            admission.settle('failure');
            lastErr = firstError;
            logger.warn('router', 'stream_failed', {
              provider_ref: safeLogRef(provider.id),
              model_ref: safeLogRef(req.model),
              status: firstError.status ?? 500,
              retriable: true,
              code: firstError.code,
              stage: firstError.stage,
              stream: true,
            });
            continue;
          }
          throw firstError;
        }
        // We have a first byte - from here on we cannot failover.
        committed = true;
        // A consumer-side throw into a suspended `yield` is a client unwind,
        // not an upstream fault: it must release the admission without
        // judging the provider. The Rust reference settles Cancelled on any
        // client-side unwind of the response future.
        const settle_client_unwind = (injected: unknown): never => {
          admission.settle('cancelled');
          throw injected;
        };
        terminalSeen = chunkHasTerminal(first.value);
        try {
          yield first.value;
        } catch (injected) {
          settle_client_unwind(injected);
        }
        for await (const ch of gen) {
          if (signal?.aborted) return;
          const chunkError = streamChunkError(ch);
          if (chunkError) {
            // Mid-stream error events surface to the client as-is; only
            // retriable ones count against the breaker, and the watchdog
            // below must not append a second error (Rust proxy parity).
            if (chunkError.retriable) admission.settle('failure');
            try {
              yield ch;
            } catch (injected) {
              settle_client_unwind(injected);
            }
            return;
          }
          terminalSeen = chunkHasTerminal(ch) || terminalSeen;
          try {
            yield ch;
          } catch (injected) {
            settle_client_unwind(injected);
          }
        }
        // The upstream generator is exhausted; there is nothing left to
        // reclaim in the `finally`.
        upstream_reclaimed = true;
        if (signal?.aborted) return;
        if (!terminalSeen) {
          // A stream that ends without a terminal chunk (finish_reason) is
          // an upstream fault: surface an explicit error chunk instead of a
          // silent success and count it against the breaker.
          admission.settle('failure');
          try {
            yield makeStreamErrorChunk(502, 'proxy_stream_error');
          } catch (injected) {
            settle_client_unwind(injected);
          }
          return;
        }
        admission.settle('success');
        logger.info('router', 'stream_succeeded', {
          provider_ref: safeLogRef(provider.id),
          model_ref: safeLogRef(req.model),
          stage: 'stream_complete',
          stream: true,
        });
        return;
      } catch (e) {
        if (signal?.aborted) return;
        lastErr = normalizedProviderError(e, committed ? 'stream_read' : 'stream_setup');
        const failover = !committed
          && (lastErr.retriable || shouldFailoverToolCapabilityRejection(req, lastErr));
        // Only retriable failures count against the breaker; terminal
        // errors are construction or credential problems, not provider
        // health (Rust proxy parity). Anything unsettled below is released
        // by the `finally` as a cancellation.
        if (lastErr.retriable || failover) admission.settle('failure');
        logger.warn('router', committed ? 'mid_stream_failed' : 'stream_failed', {
          provider_ref: safeLogRef(provider.id),
          model_ref: safeLogRef(req.model),
          status: lastErr.status ?? 500,
          retriable: failover,
          code: lastErr.code,
          stage: lastErr.stage,
          response_bytes: lastErr.responseBytes ?? 0,
          stream: true,
        });
        if (committed || !failover) throw lastErr;
        // try next hop
      } finally {
        signal?.removeEventListener('abort', abort_attempt);
        // Reclaim the upstream attempt on any exit that did not already do
        // so: a client that abandons the generator mid-stream, or throws
        // into it, would otherwise leave the upstream body read running
        // until the provider's own timeout (the Rust reference closes the
        // upstream future on drop).
        if (!upstream_reclaimed) {
          attemptAbort.abort();
          if (gen) void Promise.resolve(gen.return(undefined)).catch(() => {});
        }
        // Runs on every exit including a client that abandons the generator:
        // an admission that never reached a verdict releases its half-open
        // probe slot instead of leaking it. Already-settled hops are no-ops.
        admission.settle('cancelled');
      }
    }
    throw lastErr ?? makeProviderError('all_hops_exhausted', {
      status: 503,
      retriable: false,
      stage: 'stream_routing',
    });
  }

  private hopsFor(model: string) {
    const route = this.resolveRoute(strip_one_m_suffix(model));
    return route?.hops ?? [];
  }

  /** Anthropic count_tokens surface with hop failover (Rust parity:
   * router.rs `count_tokens_for`). Anthropic-family hops relay upstream
   * for exact counts; every other hop falls back to the deterministic
   * local estimate. */
  async count_tokens(
    model: string,
    body: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<number> {
    const client_abort = () => makeProviderError('client_abort', {
      status: 499,
      retriable: false,
      stage: 'count_tokens',
    });
    if (signal?.aborted) throw client_abort();
    const hops = this.hopsFor(model);
    if (!hops.length) {
      throw makeProviderError((this.routes.size > 0) ? 'invalid_model' : 'no_route', {
        status: 404,
        retriable: false,
        stage: 'route_resolution',
      });
    }
    let last_error: ProviderError | undefined;
    for (const hop of hops) {
      if (signal?.aborted) throw client_abort();
      const provider = this.providers.get(hop.provider);
      if (!provider || !provider.enabled) {
        logger.warn('router', 'provider_skipped', {
          provider_ref: safeLogRef(hop.provider),
          stage: 'provider_availability',
        });
        continue;
      }
      const admission = provider.breaker.acquire();
      if (!admission) {
        logger.warn('router', 'provider_skipped', {
          provider_ref: safeLogRef(hop.provider),
          stage: 'circuit_open',
        });
        continue;
      }
      try {
        // The hop's upstream model rides on the body exactly like the
        // messages surface (count accuracy follows the routed upstream).
        const hop_body = { ...body, model: hop.model };
        const tokens =
          provider instanceof AnthropicProvider
            ? await provider.count_tokens(hop_body, signal)
            : estimate_input_tokens(hop_body);
        signal?.throwIfAborted();
        admission.settle('success');
        return tokens;
      } catch (e) {
        // Abort reasons may be custom Errors, and transport/body readers may
        // wrap them. The caller's signal owns cancellation classification.
        if (signal?.aborted) throw client_abort();
        last_error = normalizedProviderError(e, 'count_tokens');
        if (last_error.retriable) {
          admission.settle('failure');
          continue;
        }
        throw last_error;
      } finally {
        // Terminal client/auth errors release a half-open slot without
        // judging provider health. Settled success/failure are idempotent.
        admission.settle('cancelled');
      }
    }
    throw (
      last_error ??
      makeProviderError('all_hops_exhausted', {
        status: 503,
        retriable: false,
        stage: 'count_tokens',
      })
    );
  }

  /** Health snapshot of all providers. */
  health() {
    return this.listProviders().map((p) => ({
      id: p.id,
      name: p.config.name,
      type: p.config.type,
      enabled: p.config.enabled,
      breaker: p.breaker.snapshot(),
    }));
  }
}

/**
 * Some OpenAI-shaped upstreams accept plain requests but reject function
 * tools with a pre-stream HTTP 400. That is a hop-capability mismatch, so a
 * finite route should try its next provider (Rust proxy parity).
 *
 * Only TRUE tool-capability rejections are retried: the upstream must have
 * been classified as such by `classifyUpstream4xx`. All other 400s 鈥?
 * authentication failures, malformed user input 鈥?remain terminal, matching
 * the invariant that auth and malformed input must never be replayed or
 * hidden by failover. An unclassified 400 is also terminal (legacy
 * default). Unlike the Rust gate (which matches the single
 * `response_status` stage), both Node stage spellings are accepted because
 * the stream surface raises the same classified error under
 * `stream_status`.
 */
function shouldFailoverToolCapabilityRejection(
  req: ChatCompletionRequest,
  error: ProviderError,
): boolean {
  const tools = req.tools;
  const hasTools = Array.isArray(tools) && tools.length > 0;
  return (
    hasTools
    && error.status === 400
    && error.code === 'upstream_http_error'
    && (error.stage === 'response_status' || error.stage === 'stream_status')
    && error.kind === 'tool_capability'
  );
}

/** A terminal chunk carries a finish_reason on any choice. Raw SSE objects
 * without a choices array (e.g. upstream error events) are not terminal. */
function chunkHasTerminal(chunk: OAIStreamChunk): boolean {
  return Array.isArray(chunk.choices) && chunk.choices.some((choice) => choice.finish_reason != null);
}

function normalizedProviderError(error: unknown, stage: string): ProviderError {
  if (
    error instanceof Error
    && 'code' in error
    && typeof (error as Partial<ProviderError>).code === 'string'
    && typeof (error as Partial<ProviderError>).retriable === 'boolean'
  ) {
    return error as ProviderError;
  }
  return makeProviderError('upstream_transport_error', {
    status: 502,
    retriable: true,
    stage,
  });
}
