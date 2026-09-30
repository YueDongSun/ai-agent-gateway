import { Hono } from 'hono';
import { serve } from '@hono/node-server';
import { randomUUID } from 'node:crypto';
import { isIP } from 'node:net';
import type { AppConfig } from '../config/schema.js';
import type { ChatCompletionRequest } from '../config/schema.js';
import { Router } from './router.js';
import { logger, setLogLevel } from '../util/log.js';
import {
  oaiRespToAnthropicResp,
  oaiChunksToAnthropicStream,
  anthropicReqToOAIReq,
  type AnthropicRequest,
  type AnthropicResponse,
} from './converter.js';
import {
  makeProviderError,
  publicProviderErrorMessage,
  type ProviderError,
} from './canonical.js';

export function createGatewayApp(cfg: AppConfig) {
  setLogLevel(cfg.gateway.logLevel);
  const router = new Router(cfg);
  const authToken = cfg.gateway.authToken;
  const app = new Hono();

  /* ---- local-browser and auth boundary -------------------------- */
  app.use('*', async (c, next) => {
    const origin = c.req.header('origin');
    if (origin && !authToken) {
      return authFailureResponse(c, 403, 'browser_auth_required');
    }
    if (origin && authToken) {
      c.header('vary', 'Origin');
      c.header('access-control-allow-origin', origin);
      c.header('access-control-allow-methods', 'GET, POST, OPTIONS');
      c.header('access-control-allow-headers', 'authorization, content-type, x-api-key');
      if (c.req.method === 'OPTIONS') return c.body(null, 204);
    }

    const sensitive = c.req.path === '/health' || c.req.path.startsWith('/v1/');
    if (sensitive && authToken) {
      const auth = c.req.header('authorization') ?? '';
      const token = auth.replace(/^Bearer\s+/i, '') || (c.req.header('x-api-key') ?? '');
      if (token !== authToken) {
        return authFailureResponse(c, 401, 'authentication_failed');
      }
    }
    await next();
  });

  /* ---- health --------------------------------------------------- */
  app.get('/health', (c) =>
    c.json({ status: 'ok', providers: router.health(), models: router.listModels() }),
  );
  app.get('/', (c) => c.redirect('/health'));

  /* ---- models list --------------------------------------------- */
  app.get('/v1/models', (c) => {
    const models = router.listModels().map((id) => ({
      id,
      object: 'model',
      created: 1_700_000_000,
      owned_by: 'ai-agent-gateway',
    }));
    return c.json({ object: 'list', data: models });
  });

  /* ---- OpenAI compatible: /v1/chat/completions ----------------- */
  app.post('/v1/chat/completions', async (c) => {
    let body: ChatCompletionRequest;
    try {
      body = (await c.req.json()) as ChatCompletionRequest;
    } catch {
      return c.json({ error: { message: 'invalid JSON body', type: 'invalid_request_error' } }, 400);
    }
    if (!body.model || !Array.isArray(body.messages)) {
      return c.json({ error: { message: 'model and messages are required', type: 'invalid_request_error' } }, 400);
    }

    const stream = body.stream === true;

    if (stream) {
      c.header('content-type', 'text/event-stream');
      c.header('cache-control', 'no-cache');
      c.header('connection', 'keep-alive');
      return c.body(streamToReadable(router.stream(body)));
    }

    try {
      const resp = await router.complete(body);
      return c.json(resp);
    } catch (e) {
      return errResponse(c, e);
    }
  });

  /* ---- Anthropic compatible: /v1/messages ---------------------- */
  app.post('/v1/messages', async (c) => {
    let body: AnthropicRequest;
    try {
      body = (await c.req.json()) as AnthropicRequest;
    } catch {
      return c.json({ type: 'error', error: { type: 'invalid_request_error', message: 'invalid JSON body' } }, 400);
    }
    if (!body.model || !Array.isArray(body.messages)) {
      return c.json({ type: 'error', error: { type: 'invalid_request_error', message: 'model and messages are required' } }, 400);
    }

    const oaiReq = anthropicReqToOAIReq(body);
    const stream = body.stream === true;

    if (stream) {
      c.header('content-type', 'text/event-stream');
      c.header('cache-control', 'no-cache');
      c.header('connection', 'keep-alive');
      return c.body(anthropicStreamToReadable(
        (async function* () {
          yield* oaiChunksToAnthropicStream(router.stream(oaiReq), body.model);
        })(),
      ));
    }

    try {
      const oaiResp = await router.complete(oaiReq);
      const aResp = oaiRespToAnthropicResp(oaiResp, body.model) as AnthropicResponse;
      return c.json(aResp);
    } catch (e) {
      return anthropicErrResponse(c, e);
    }
  });

  /* ---- 404 ----------------------------------------------------- */
  app.notFound((c) => c.json({
    error: {
      message: 'The requested gateway endpoint was not found.',
      type: 'not_found',
      code: 'endpoint_not_found',
    },
  }, 404));

  app.onError((_err, c) => {
    const correlationId = randomUUID();
    logger.error('gateway', 'gateway_unhandled_error', {
      request_id: correlationId,
      status: 500,
      code: 'gateway_internal_error',
      stage: 'http_edge',
      retriable: false,
    });
    return c.json({
      error: {
        message: 'The gateway encountered an internal error.',
        type: 'internal_error',
        code: 'gateway_internal_error',
        correlation_id: correlationId,
      },
    }, 500);
  });

  return { app, router, start: () => start(cfg, app) };
}

function start(cfg: AppConfig, app: Hono) {
  const { host, port } = cfg.gateway;
  if (!cfg.gateway.authToken && !isLoopbackHost(host)) {
    throw new Error('gateway_auth_required_for_non_loopback_bind');
  }
  serve({ fetch: app.fetch, hostname: host, port }, (info) => {
    logger.info('gateway', 'gateway_listening', { port: info.port });
    logger.info('gateway', 'gateway_endpoint_ready', { method: 'POST', surface: 'chat' });
    logger.info('gateway', 'gateway_endpoint_ready', { method: 'POST', surface: 'claude' });
    logger.info('gateway', 'gateway_endpoint_ready', { method: 'GET', surface: 'models' });
    logger.info('gateway', 'gateway_endpoint_ready', { method: 'GET', surface: 'health' });
  });
}

/* ---- helpers --------------------------------------------------- */

function errResponse(c: any, error: unknown, overrideType?: string) {
  const err = normalizedExternalError(error);
  const status = err.status ?? 500;
  const correlationId = randomUUID();
  logger.warn('gateway', 'request_failed', {
    request_id: correlationId,
    status,
    code: err.code,
    stage: err.stage,
    retriable: err.retriable,
    response_bytes: err.responseBytes ?? 0,
  });
  return c.json({
    error: {
      message: publicProviderErrorMessage(err.code, status),
      type: overrideType ?? (status >= 500 ? 'server_error' : 'invalid_request_error'),
      code: err.code,
      correlation_id: correlationId,
      retriable: err.retriable,
    },
  }, status);
}

function anthropicErrResponse(c: any, error: unknown) {
  const err = normalizedExternalError(error);
  const status = err.status ?? 500;
  const correlationId = randomUUID();
  logger.warn('gateway', 'request_failed', {
    request_id: correlationId,
    status,
    code: err.code,
    stage: err.stage,
    retriable: err.retriable,
    response_bytes: err.responseBytes ?? 0,
  });
  return c.json({
    type: 'error',
    error: {
      type: status >= 500 ? 'api_error' : 'invalid_request_error',
      message: publicProviderErrorMessage(err.code, status),
    },
    request_id: correlationId,
  }, status);
}

function authFailureResponse(c: any, status: 401 | 403, code: string) {
  const message = status === 401
    ? 'Gateway authentication failed.'
    : 'Browser access requires gateway authentication.';
  if (c.req.path === '/v1/messages') {
    return c.json({
      type: 'error',
      error: { type: 'authentication_error', message },
    }, status);
  }
  return c.json({
    error: { message, type: 'auth_error', code },
  }, status);
}

function anthropicStreamToReadable(iter: AsyncGenerator<unknown>): ReadableStream {
  const encoder = new TextEncoder();
  return new ReadableStream({
    async start(controller) {
      try {
        for await (const item of iter) {
          const data = typeof item === 'string'
            ? item
            : `event: message\ndata: ${JSON.stringify(item)}\n\n`;
          controller.enqueue(encoder.encode(data));
        }
      } catch (error) {
        const err = normalizedExternalError(error);
        const correlationId = randomUUID();
        logger.warn('gateway', 'stream_failed', {
          request_id: correlationId,
          status: err.status ?? 500,
          code: err.code,
          stage: err.stage,
          retriable: err.retriable,
          response_bytes: err.responseBytes ?? 0,
          stream: true,
        });
        controller.enqueue(encoder.encode(
          `event: error\ndata: ${JSON.stringify({
            type: 'error',
            error: {
              type: 'api_error',
              message: publicProviderErrorMessage(err.code, err.status),
            },
            request_id: correlationId,
          })}\n\n`,
        ));
      } finally {
        controller.close();
      }
    },
  });
}

function streamToReadable(iter: AsyncGenerator<unknown>): ReadableStream {
  const encoder = new TextEncoder();
  return new ReadableStream({
    async start(controller) {
      try {
        for await (const item of iter) {
          const data = typeof item === 'string' ? item : `data: ${JSON.stringify(item)}\n\n`;
          controller.enqueue(encoder.encode(data));
        }
        controller.enqueue(encoder.encode('data: [DONE]\n\n'));
      } catch (e) {
        const err = normalizedExternalError(e);
        const correlationId = randomUUID();
        logger.warn('gateway', 'stream_failed', {
          request_id: correlationId,
          status: err.status ?? 500,
          code: err.code,
          stage: err.stage,
          retriable: err.retriable,
          response_bytes: err.responseBytes ?? 0,
          stream: true,
        });
        controller.enqueue(encoder.encode(`data: ${JSON.stringify({
          error: {
            message: publicProviderErrorMessage(err.code, err.status),
            type: 'stream_error',
            code: err.code,
            correlation_id: correlationId,
            retriable: err.retriable,
          },
        })}\n\n`));
        controller.enqueue(encoder.encode('data: [DONE]\n\n'));
      } finally {
        controller.close();
      }
    },
  });
}

function normalizedExternalError(error: unknown): ProviderError {
  if (
    error instanceof Error
    && 'code' in error
    && typeof (error as Partial<ProviderError>).code === 'string'
    && typeof (error as Partial<ProviderError>).retriable === 'boolean'
  ) {
    return error as ProviderError;
  }
  return makeProviderError('gateway_internal_error', {
    status: 500,
    retriable: false,
    stage: 'http_edge',
  });
}

function isLoopbackHost(host: string): boolean {
  const normalized = host.trim().replace(/^\[|\]$/g, '').toLowerCase();
  if (normalized === 'localhost' || normalized === '::1') return true;
  if (isIP(normalized) !== 4) return false;
  return normalized.split('.')[0] === '127';
}
