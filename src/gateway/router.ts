import type { AppConfig, RouteConfig } from '../config/schema.js';
import type { ChatCompletionRequest } from '../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from './canonical.js';
import { makeProviderError, type ProviderError } from './canonical.js';
import { createProvider, type BaseProvider } from './providers/index.js';
import { logger, safeLogRef } from '../util/log.js';

/**
 * The router resolves a requested model alias to an ordered fallback chain
 * of (provider, model) hops, then executes them with circuit-breaker
 * protection. On a retriable failure (or open circuit) it moves to the
 * next hop automatically.
 */
export class Router {
  private providers = new Map<string, BaseProvider>();
  private routes = new Map<string, RouteConfig>();

  constructor(cfg: AppConfig) {
    for (const p of cfg.providers) {
      this.providers.set(p.id, createProvider(p));
    }
    for (const r of cfg.routes) {
      this.routes.set(r.alias, r);
    }
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

  /** Non-streaming completion with automatic failover. */
  async complete(req: ChatCompletionRequest): Promise<OAIChatResponse> {
    const hops = this.hopsFor(req.model);
    if (!hops.length) {
      throw makeProviderError('no_route', {
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
      if (!provider.breaker.allow()) {
        logger.warn('router', 'provider_skipped', {
          provider_ref: safeLogRef(hop.provider),
          stage: 'circuit_open',
        });
        continue;
      }
      try {
        const subReq: ChatCompletionRequest = { ...req, model: hop.model };
        const resp = await provider.complete(subReq);
        provider.breaker.recordSuccess();
        logger.info('router', 'provider_succeeded', {
          provider_ref: safeLogRef(provider.id),
          model_ref: safeLogRef(req.model),
          stage: 'complete',
        });
        return resp;
      } catch (e) {
        lastErr = normalizedProviderError(e, 'complete');
        const retriable = lastErr.retriable;
        provider.breaker.recordFailure();
        logger.warn('router', 'provider_failed', {
          provider_ref: safeLogRef(provider.id),
          model_ref: safeLogRef(req.model),
          status: lastErr.status ?? 500,
          retriable,
          code: lastErr.code,
          stage: lastErr.stage,
          response_bytes: lastErr.responseBytes ?? 0,
        });
        if (!retriable) throw lastErr;
      }
    }
    throw lastErr ?? makeProviderError('all_hops_exhausted', {
      status: 503,
      retriable: false,
      stage: 'routing',
    });
  }

  /** Streaming completion with automatic failover (only pre-first-byte). */
  async *stream(req: ChatCompletionRequest): AsyncGenerator<OAIStreamChunk> {
    const hops = this.hopsFor(req.model);
    if (!hops.length) {
      throw makeProviderError('no_route', {
        status: 404,
        retriable: false,
        stage: 'route_resolution',
      });
    }

    let lastErr: ProviderError | undefined;
    for (const hop of hops) {
      const provider = this.providers.get(hop.provider);
      if (!provider || !provider.enabled) continue;
      if (!provider.breaker.allow()) {
        logger.warn('router', 'provider_skipped', {
          provider_ref: safeLogRef(hop.provider),
          stage: 'circuit_open',
          stream: true,
        });
        continue;
      }
      let committed = false;
      try {
        const subReq: ChatCompletionRequest = { ...req, model: hop.model };
        const gen = provider.stream(subReq);
        // Probe the first chunk before committing - allows failover if the
        // upstream errors before sending any data.
        const first = await gen.next();
        if (first.done) {
          // empty stream - treat as success with nothing
          provider.breaker.recordSuccess();
          return;
        }
        // We have a first byte - from here on we cannot failover.
        committed = true;
        yield first.value;
        for await (const ch of gen) yield ch;
        provider.breaker.recordSuccess();
        logger.info('router', 'stream_succeeded', {
          provider_ref: safeLogRef(provider.id),
          model_ref: safeLogRef(req.model),
          stage: 'stream_complete',
          stream: true,
        });
        return;
      } catch (e) {
        lastErr = normalizedProviderError(e, committed ? 'stream_read' : 'stream_setup');
        const retriable = lastErr.retriable;
        provider.breaker.recordFailure();
        logger.warn('router', committed ? 'mid_stream_failed' : 'stream_failed', {
          provider_ref: safeLogRef(provider.id),
          model_ref: safeLogRef(req.model),
          status: lastErr.status ?? 500,
          retriable,
          code: lastErr.code,
          stage: lastErr.stage,
          response_bytes: lastErr.responseBytes ?? 0,
          stream: true,
        });
        if (committed || !retriable) throw lastErr;
        // try next hop
      }
    }
    throw lastErr ?? makeProviderError('all_hops_exhausted', {
      status: 503,
      retriable: false,
      stage: 'stream_routing',
    });
  }

  private hopsFor(model: string) {
    const route = this.resolveRoute(model);
    return route?.hops ?? [];
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
