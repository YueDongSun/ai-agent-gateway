import { Hono } from 'hono';
import { serve } from '@hono/node-server';
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
import { makeError } from './canonical.js';

export function createGatewayApp(cfg: AppConfig) {
  setLogLevel(cfg.gateway.logLevel);
  const router = new Router(cfg);
  const authToken = cfg.gateway.authToken;
  const app = new Hono();

  /* ---- auth middleware ------------------------------------------ */
  app.use('/v1/*', async (c, next) => {
    if (authToken) {
      const auth = c.req.header('authorization') ?? '';
      const token = auth.replace(/^Bearer\s+/i, '') || (c.req.header('x-api-key') ?? '');
      if (token !== authToken) {
        return c.json({ error: { message: 'invalid auth token', type: 'auth_error' } }, 401);
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
      return c.body(streamToReadable(
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
      const msg = (e as Error).message;
      const status = (e as any).status ?? 500;
      return c.json({ type: 'error', error: { type: 'api_error', message: msg } }, status);
    }
  });

  /* ---- 404 ----------------------------------------------------- */
  app.notFound((c) => c.json({ error: { message: 'not found', path: c.req.path } }, 404));

  app.onError((err, c) => {
    logger.error('gateway', `unhandled: ${err.message}`);
    return c.json({ error: { message: err.message, type: 'internal_error' } }, 500);
  });

  return { app, router, start: () => start(cfg, app) };
}

function start(cfg: AppConfig, app: Hono) {
  const { host, port } = cfg.gateway;
  serve({ fetch: app.fetch, hostname: host, port }, (info) => {
    const addrs = [`http://${host}:${info.port}`];
    logger.info('gateway', `listening on ${addrs.join(', ')}`);
    logger.info('gateway', `endpoints:`);
    logger.info('gateway', `  POST ${addrs[0]}/v1/chat/completions  (OpenAI compatible)`);
    logger.info('gateway', `  POST ${addrs[0]}/v1/messages          (Anthropic compatible)`);
    logger.info('gateway', `  GET  ${addrs[0]}/v1/models`);
    logger.info('gateway', `  GET  ${addrs[0]}/health`);
  });
}

/* ---- helpers --------------------------------------------------- */

function errResponse(c: any, e: unknown) {
  const err = e as { message: string; status?: number };
  const status = err.status ?? 500;
  const message = err.message ?? 'unknown error';
  return c.json({ error: { message, type: status >= 500 ? 'server_error' : 'invalid_request_error' } }, status);
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
        const msg = (e as Error).message ?? 'stream error';
        controller.enqueue(encoder.encode(`data: ${JSON.stringify({ error: { message: msg } })}\n\n`));
        controller.enqueue(encoder.encode('data: [DONE]\n\n'));
      } finally {
        controller.close();
      }
    },
  });
}
