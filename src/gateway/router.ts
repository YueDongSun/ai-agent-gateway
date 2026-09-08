import type { AppConfig, RouteConfig } from '../config/schema.js';
import type { ChatCompletionRequest } from '../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from './canonical.js';
import { makeError, type ProviderError } from './canonical.js';
import { createProvider, type BaseProvider } from './providers/index.js';
import { logger } from '../util/log.js';

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
    if (!hops.length) throw makeError(`no route / provider for model "${req.model}"`, { status: 404, retriable: false });

    let lastErr: ProviderError | undefined;
    for (const hop of hops) {
      const provider = this.providers.get(hop.provider);
      if (!provider || !provider.enabled) {
        logger.warn('router', `skip disabled/unknown provider "${hop.provider}"`);
        continue;
      }
      if (!provider.breaker.allow()) {
        logger.warn('router', `circuit OPEN for "${hop.provider}", skipping`);
        continue;
      }
      try {
        const subReq: ChatCompletionRequest = { ...req, model: hop.model };
        const resp = await provider.complete(subReq);
        provider.breaker.recordSuccess();
        logger.info('router', `OK  ${req.model} <- ${provider.id}/${hop.model}`);
        return resp;
      } catch (e) {
        lastErr = e as ProviderError;
        const retriable = lastErr.retriable;
        provider.breaker.recordFailure();
        logger.warn('router', `FAIL ${provider.id}/${hop.model}: ${lastErr.message} (retriable=${retriable})`);
        if (!retriable) throw lastErr;
      }
    }
    throw lastErr ?? makeError('all hops exhausted', { status: 503, retriable: false });
  }

  /** Streaming completion with automatic failover (only pre-first-byte). */
  async *stream(req: ChatCompletionRequest): AsyncGenerator<OAIStreamChunk> {
    const hops = this.hopsFor(req.model);
    if (!hops.length) throw makeError(`no route / provider for model "${req.model}"`, { status: 404, retriable: false });

    let lastErr: ProviderError | undefined;
    for (const hop of hops) {
      const provider = this.providers.get(hop.provider);
      if (!provider || !provider.enabled) continue;
      if (!provider.breaker.allow()) {
        logger.warn('router', `circuit OPEN for "${hop.provider}", skipping (stream)`);
        continue;
      }
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
        yield first.value;
        provider.breaker.recordSuccess();
        try {
          for await (const ch of gen) yield ch;
        } catch (e) {
          // mid-stream error: surface to client (can't transparently failover)
          logger.error('router', `mid-stream error on ${provider.id}: ${(e as Error).message}`);
          throw e;
        }
        logger.info('router', `STREAM OK  ${req.model} <- ${provider.id}/${hop.model}`);
        return;
      } catch (e) {
        lastErr = e as ProviderError;
        const retriable = lastErr.retriable;
        if (!provider.breaker.allow()) {
          // already recorded failure above path? ensure recorded
        }
        provider.breaker.recordFailure();
        logger.warn('router', `STREAM FAIL ${provider.id}/${hop.model}: ${lastErr.message} (retriable=${retriable})`);
        if (!retriable) throw lastErr;
        // try next hop
      }
    }
    throw lastErr ?? makeError('all stream hops exhausted', { status: 503, retriable: false });
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
