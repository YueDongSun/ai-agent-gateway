import { Hono } from 'hono';
import { bodyLimit } from 'hono/body-limit';
import { serve } from '@hono/node-server';
import { randomUUID } from 'node:crypto';
import { createRequire } from 'node:module';
import { isIP } from 'node:net';
import type { AppConfig } from '../config/schema.js';
import type { ChatCompletionRequest } from '../config/schema.js';
import { validate_chat_request, type Field_validation_error } from './request_contract.js';
import { Router } from './router.js';
import { replay_first_chunk, sse_body_stream, stream_abort } from './stream_body.js';
import { anthropic_requests } from './anthropic_envelope.js';
import { logger, setLogLevel } from '../util/log.js';
import {
  oaiRespToAnthropicResp,
  oaiChunksToAnthropicStream,
  anthropicReqToOAIReq,
  type AnthropicRequest,
  type AnthropicResponse,
} from './converter.js';
import {
  responses_request_to_chat,
  chat_to_responses,
  chat_chunks_to_responses_stream,
  responses_stream_failure_frame,
} from './responses.js';
import {
  makeProviderError,
  publicProviderErrorMessage,
  type OAIStreamChunk,
  type ProviderError,
} from './canonical.js';
import { make_public_error_envelope } from './error_contract.js';

/**
 * Hard cap on inbound JSON request bodies, mirroring `LARGE_REQUEST_BODY_BYTES`
 * in `src-tauri/src/proxy/server.rs`. Without it `c.req.json()` buffers an
 * arbitrary client payload whole before any validation. The Rust proxy maps a
 * cap overrun to `400 invalid_request_error` (its `JsonRejection` catch-all),
 * so the Node surface returns the same status and error type 鈥?not the
 * middleware's default 413 鈥?to keep both runtimes wire-identical.
 */
export const LARGE_REQUEST_BODY_BYTES = 32 * 1024 * 1024;

/**
 * Grace window for gateway shutdown before in-flight streams are cut:
 * server.close() waits for open keep-alive connections (an SSE response can
 * hold it open indefinitely), so shutdown needs a bounded force-exit.
 */
export const SHUTDOWN_GRACE_MS = 3000;

export interface GatewayAppOptions {
  /** Request body cap override (tests). Defaults to {@link LARGE_REQUEST_BODY_BYTES}. */
  max_request_body_bytes?: number;
}

function openai_body_too_large(c: any) {
  return validation_error_response(c, [{ field: 'body', code: 'too_large', message: 'request body too large' }]);
}

function anthropic_body_too_large(c: any) {
  const request_id = randomUUID();
  return c.json({ type: 'error', error: { type: 'invalid_request_error', code: 'request_too_large', message: 'request body too large', field: 'body', retryable: false }, request_id }, 400);
}

export function createGatewayApp(cfg: AppConfig, opts: GatewayAppOptions = {}) {
  setLogLevel(cfg.gateway.logLevel);
  // Mutable: `reload_config` swaps the Router for subsequent requests while
  // in-flight requests keep the router they were admitted with.
  let router = new Router(cfg);
  const authToken = cfg.gateway.authToken;
  // Listener identity is pinned at startup (Rust runtime parity): a config
  // reload never rebinds the socket or rotates the auth token.
  const startup_gateway = { host: cfg.gateway.host, port: cfg.gateway.port, authToken };
  let listener_restart_required = false;
  let auth_restart_required = false;
  const max_request_body_bytes = opts.max_request_body_bytes ?? LARGE_REQUEST_BODY_BYTES;
  const gateway_version = (createRequire(import.meta.url)('../../package.json') as { version: string }).version;
  const app = new Hono();

  /**
   * Hot-apply a new config to the routing state (providers, routes,
   * models, log level) for subsequent requests. A config that changes the
   * pinned listener or auth token applies only the routing surface and
   * logs what still needs a restart.
   */
  const reload_config = (next: AppConfig) => {
    router = new Router(next);
    setLogLevel(next.gateway.logLevel);
    listener_restart_required = next.gateway.host !== startup_gateway.host || next.gateway.port !== startup_gateway.port;
    auth_restart_required = next.gateway.authToken !== startup_gateway.authToken;
    logger.info('gateway', 'gateway_reloaded', {
      reload_status: 'applied', provider_count: next.providers.length, route_count: next.routes.length,
    });
    if (listener_restart_required || auth_restart_required) logger.warn('gateway', 'gateway_reload_pinned', {
      reload_status: 'restart_required', listener_restart_required, auth_restart_required,
      reload_reason: listener_restart_required && auth_restart_required ? 'listener_and_auth_changed' : listener_restart_required ? 'listener_changed' : 'auth_changed',
    });
  };

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
  app.get('/health', (c) => {
    const offered_models = new Set([...router.listModels(), ...router.listProviders().filter((provider) => provider.enabled).flatMap((provider) => provider.models())]);
    const available_models = [...offered_models].filter((model) => router.resolveRoute(model)?.hops.some((hop) => router.getProvider(hop.provider)?.enabled));
    return c.json({
      status: 'ok', runtime_kind: 'node', providers: router.health(), models: router.listModels(), available_models, ready: available_models.length > 0,
      readiness: available_models.length ? 'configured_candidates' : 'no_enabled_candidates',
      configuration: { routing: 'effective', restart_required: listener_restart_required || auth_restart_required, listener_restart_required, auth_restart_required },
    });
  });
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
  app.post(
    '/v1/chat/completions',
    bodyLimit({ maxSize: max_request_body_bytes, onError: openai_body_too_large }),
    async (c) => {
    let body: ChatCompletionRequest;
    try {
      body = (await c.req.json()) as ChatCompletionRequest;
    } catch {
      return validation_error_response(c, [{ field: 'body', code: 'invalid_json', message: 'invalid JSON body' }]);
    }
    const validation = validate_chat_request(body);
    if (!validation.ok) {
      return validation_error_response(c, validation.errors);
    }

    const stream = body.stream === true;

    if (stream) {
      // Rust parity: the proxy probes stream setup (route resolution and the
      // first chunk's failover budget) before committing to 200/SSE, so a
      // failed setup returns the JSON error envelope with its real status
      // instead of a 200 stream that only carries an error event.
      const client_abort = stream_abort(c.req.raw.signal);
      const gen = router.stream(body, client_abort.signal);
      let first: IteratorResult<OAIStreamChunk>;
      try {
        first = await gen.next();
      } catch (e) {
        client_abort.dispose();
        return errResponse(c, e, body.model);
      }
      c.header('content-type', 'text/event-stream');
      c.header('cache-control', 'no-cache');
      c.header('connection', 'keep-alive');
      return c.body(sse_body_stream(replay_first_chunk(first, gen), {
        chunk: oai_chunk_frame,
        failure: oai_failure_frames,
        completion: () => { client_abort.dispose(); return ['data: [DONE]\n\n']; },
      }, () => {
        client_abort.abort();
        void gen.return(undefined).catch(() => {});
      }));
    }

    try {
      const resp = await router.complete(body, c.req.raw.signal);
      return c.json(resp);
    } catch (e) {
      return errResponse(c, e, body.model);
    }
  });

  /* ---- Anthropic compatible: /v1/messages ---------------------- */
  app.post(
    '/v1/messages',
    bodyLimit({ maxSize: max_request_body_bytes, onError: anthropic_body_too_large }),
    async (c) => {
    let body: AnthropicRequest;
    try {
      body = (await c.req.json()) as AnthropicRequest;
    } catch {
      const request_id = randomUUID();
      return c.json({ type: 'error', error: { type: 'invalid_request_error', code: 'invalid_json', message: 'invalid JSON body', field: 'body', retryable: false }, request_id }, 400);
    }

    const oaiReq = anthropicReqToOAIReq(body);
    const validation = validate_chat_request(oaiReq);
    if (!validation.ok) {
      return anthropic_validation_error_response(c, validation.errors);
    }
    anthropic_requests.set(oaiReq, body);
    const stream = body.stream === true;

    if (stream) {
      // Same pre-commit probe as the OpenAI surface: a stream setup failure
      // is a JSON Anthropic error envelope, not a committed 200 SSE stream.
      const client_abort = stream_abort(c.req.raw.signal);
      const gen = router.stream(oaiReq, client_abort.signal);
      let first: IteratorResult<OAIStreamChunk>;
      try {
        first = await gen.next();
      } catch (e) {
        client_abort.dispose();
        return anthropicErrResponse(c, e, body.model);
      }
      c.header('content-type', 'text/event-stream');
      c.header('cache-control', 'no-cache');
      c.header('connection', 'keep-alive');
      return c.body(sse_body_stream(
        (async function* () {
          yield* oaiChunksToAnthropicStream(replay_first_chunk(first, gen), body.model);
        })(),
        {
          chunk: anthropic_chunk_frame,
          failure: anthropic_failure_frames,
          completion: () => { client_abort.dispose(); return []; },
        },
        () => {
          client_abort.abort();
          void gen.return(undefined).catch(() => {});
        },
      ));
    }

    try {
      const oaiResp = await router.complete(oaiReq, c.req.raw.signal);
      const aResp = oaiRespToAnthropicResp(oaiResp, body.model) as AnthropicResponse;
      return c.json(aResp);
    } catch (e) {
      return anthropicErrResponse(c, e, body.model);
    }
  });

  /* ---- OpenAI compatible: /v1/responses (Codex CLI) ------------- */
  app.post(
    '/v1/responses',
    bodyLimit({ maxSize: max_request_body_bytes, onError: openai_body_too_large }),
    async (c) => {
    let body: Record<string, unknown>;
    try {
      body = (await c.req.json()) as Record<string, unknown>;
    } catch {
      return validation_error_response(c, [{ field: 'body', code: 'invalid_json', message: 'invalid JSON body' }]);
    }
    if (typeof body.model !== 'string' || !body.model) {
      return validation_error_response(c, [{ field: 'model', code: 'required', message: 'model is required' }]);
    }
    const requested_model = body.model;
    const chat_req = responses_request_to_chat(body);

    if (chat_req.stream) {
      // Same pre-commit probe as the other surfaces: a stream setup failure
      // returns the JSON error envelope, not a committed 200 SSE stream.
      const client_abort = stream_abort(c.req.raw.signal);
      const gen = router.stream(chat_req, client_abort.signal);
      let first: IteratorResult<OAIStreamChunk>;
      try {
        first = await gen.next();
      } catch (e) {
        client_abort.dispose();
        return errResponse(c, e, requested_model);
      }
      c.header('content-type', 'text/event-stream');
      c.header('cache-control', 'no-cache');
      c.header('connection', 'keep-alive');
      return c.body(sse_body_stream(chat_chunks_to_responses_stream(replay_first_chunk(first, gen), requested_model), {
        chunk: (frame) => frame as string,
        failure: (error) => {
          const err = normalizedExternalError(error);
          report_stream_failure(error);
          return [responses_stream_failure_frame(
            requested_model,
            err.code,
            publicProviderErrorMessage(err.code, err.status),
          )];
        },
        // The converter owns terminal ordering and emits `data: [DONE]`.
        completion: () => { client_abort.dispose(); return []; },
      }, () => {
        client_abort.abort();
        void gen.return(undefined).catch(() => {});
      }));
    }

    try {
      const resp = await router.complete(chat_req, c.req.raw.signal);
      return c.json(chat_to_responses(resp, requested_model));
    } catch (e) {
      return errResponse(c, e, requested_model);
    }
  });

  /* ---- service identity (Rust parity: aag/discovery) ------------- */
  app.get('/aag/discovery', (c) => {
    c.header('cache-control', 'no-store');
    return c.json({
      service: 'ai-agent-gateway',
      ...(c.req.query('runtime') === '1' ? { runtime_kind: 'node' } : {}),
      schemaVersion: 1,
      status: 'running',
      version: gateway_version,
    });
  });

  /* ---- Anthropic compatible: /v1/messages/count_tokens ---------- */
  app.post(
    '/v1/messages/count_tokens',
    bodyLimit({ maxSize: max_request_body_bytes, onError: anthropic_body_too_large }),
    async (c) => {
      let body: Record<string, unknown>;
      try {
        body = (await c.req.json()) as Record<string, unknown>;
      } catch {
        return c.json(
          { type: 'error', error: { type: 'invalid_request_error', code: 'invalid_json', message: 'invalid JSON body', field: 'body', retryable: false }, request_id: randomUUID() },
          400,
        );
      }
      if (typeof body.model !== 'string' || !body.model) {
        return c.json(
          { type: 'error', error: { type: 'invalid_request_error', code: 'required', message: 'model is required', field: 'model', retryable: false }, request_id: randomUUID() },
          400,
        );
      }
      try {
        const input_tokens = await router.count_tokens(body.model, body, c.req.raw.signal);
        return c.json({ input_tokens });
      } catch (e) {
        return anthropicErrResponse(c, e);
      }
    },
  );

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

  return {
    app,
    // Getter: `reload_config` replaces the Router, so a captured reference
    // must observe the swap instead of pinning the startup instance.
    get router() {
      return router;
    },
    start: () => start(cfg, app),
    reload_config,
  };
}

function start(cfg: AppConfig, app: Hono): ReturnType<typeof serve> {
  const { host, port } = cfg.gateway;
  if (!cfg.gateway.authToken && !isLoopbackHost(host)) {
    throw new Error('gateway_auth_required_for_non_loopback_bind');
  }
  // Boundary guard for callers that mutate the parsed config (the CLI
  // --port override) before calling start: an invalid port would otherwise
  // fail asynchronously inside the node server's listen.
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error('gateway_invalid_port');
  }
  return serve({ fetch: app.fetch, hostname: host, port }, (info) => {
    logger.info('gateway', 'gateway_listening', { port: info.port });
    logger.info('gateway', 'gateway_endpoint_ready', { method: 'POST', surface: 'chat' });
    logger.info('gateway', 'gateway_endpoint_ready', { method: 'POST', surface: 'claude' });
    logger.info('gateway', 'gateway_endpoint_ready', { method: 'GET', surface: 'models' });
    logger.info('gateway', 'gateway_endpoint_ready', { method: 'GET', surface: 'health' });
  });
}

/** Minimal lifecycle surface shutdown needs from the gateway server. */
export interface ClosableServer {
  close(callback?: () => void): unknown;
}

/**
 * Wire SIGINT/SIGTERM to a bounded graceful shutdown: stop accepting
 * connections, let in-flight requests finish, and exit 0 through the close
 * callback. Because server.close() waits for open keep-alive connections 鈥? * a streaming response can hold it open indefinitely 鈥?an unref'd timer
 * force-exits after the grace window. A second signal exits immediately
 * (exit 1 marks the shutdown as forced or interrupted).
 */
export function install_shutdown_handlers(
  server: ClosableServer,
  grace_ms: number = SHUTDOWN_GRACE_MS,
): void {
  let shutting_down = false;
  const shutdown = (signal_name: string): void => {
    // The return matters when exit is intercepted (tests): without it the
    // guard relies solely on process.exit never yielding.
    if (shutting_down) {
      process.exit(1);
      return;
    }
    shutting_down = true;
    logger.info('gateway', 'gateway_shutdown', { signal: signal_name });
    server.close(() => process.exit(0));
    setTimeout(() => process.exit(1), grace_ms).unref();
  };
  process.on('SIGINT', () => shutdown('sigint'));
  process.on('SIGTERM', () => shutdown('sigterm'));
}

/* ---- helpers --------------------------------------------------- */

function errResponse(c: any, error: unknown, upstream_model?: string, field?: string) {
  const err = normalizedExternalError(error);
  const status = err.status ?? 500;
  const request_id = randomUUID();
  logger.warn('gateway', 'request_failed', { request_id, status, code: err.code, stage: err.stage, retriable: err.retriable, response_bytes: err.responseBytes ?? 0 });
  const envelope = make_public_error_envelope(err, { status, request_id, upstream_model, field });
  return c.json({
    ...envelope,
    error: { ...envelope.error, type: status >= 500 ? 'proxy_error' : 'invalid_request_error' },
  }, status);
}

function anthropicErrResponse(c: any, error: unknown, upstream_model?: string, field?: string) {
  const err = normalizedExternalError(error);
  const status = err.status ?? 500;
  const request_id = randomUUID();
  logger.warn('gateway', 'request_failed', { request_id, status, code: err.code, stage: err.stage, retriable: err.retriable, response_bytes: err.responseBytes ?? 0 });
  const envelope = make_public_error_envelope(err, { status, request_id, upstream_model, field });
  return c.json({
    type: 'error',
    error: { ...envelope.error, type: 'api_error' },
    request_id,
    upstream_model: envelope.upstream_model,
  }, status);
}
function validation_error_response(c: any, errors: Field_validation_error[]) {
  const request_id = randomUUID();
  const first = errors[0] ?? { field: 'request', code: 'invalid_request', message: 'The request is invalid.' };
  return c.json({
    error: {
      type: 'invalid_request_error',
      code: first.code,
      message: first.message,
      field: first.field,
      retryable: false,
      errors,
      correlation_id: request_id,
      retriable: false,
    },
    request_id,
  }, 400);
}

function anthropic_validation_error_response(c: any, errors: Field_validation_error[]) {
  const request_id = randomUUID();
  const first = errors[0] ?? { field: 'request', code: 'invalid_request', message: 'The request is invalid.' };
  return c.json({
    type: 'error',
    error: { type: 'invalid_request_error', code: first.code, message: first.message, field: first.field, retryable: false, errors },
    request_id,
  }, 400);
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

/**
 * OpenAI SSE framing. Already-formatted strings (provider passthrough
 * text) pass through untouched; chunks are serialized as `data:` frames.
 */
function oai_chunk_frame(chunk: unknown): string {
  return typeof chunk === 'string' ? chunk : `data: ${JSON.stringify(chunk)}\n\n`;
}

/**
 * OpenAI failure framing: the error frame plus the `[DONE]` terminator, so
 * a client waiting for the terminator is never left hanging.
 */
function oai_failure_frames(error: unknown): string[] {
  const { err, correlationId } = report_stream_failure(error);
  return [
    `data: ${JSON.stringify({
      error: {
        message: publicProviderErrorMessage(err.code, err.status),
        type: 'stream_error',
        code: err.code,
        correlation_id: correlationId,
        retriable: err.retriable,
      },
    })}\n\n`,
    'data: [DONE]\n\n',
  ];
}

/** Anthropic SSE framing: every event carries its event name. */
function anthropic_chunk_frame(chunk: unknown): string {
  return typeof chunk === 'string'
    ? chunk
    : `event: message\ndata: ${JSON.stringify(chunk)}\n\n`;
}

/**
 * Anthropic failure framing: a single `error` event, no terminator (this
 * surface has no `[DONE]`).
 */
function anthropic_failure_frames(error: unknown): string[] {
  const { err, correlationId } = report_stream_failure(error);
  return [
    `event: error\ndata: ${JSON.stringify({
      type: 'error',
      error: {
        type: 'api_error',
        message: publicProviderErrorMessage(err.code, err.status),
      },
      request_id: correlationId,
    })}\n\n`,
  ];
}

/**
 * Normalize a mid-stream failure, record it as metadata only (never the
 * payload), and hand the caller the correlation id its wire frame carries.
 */
function report_stream_failure(error: unknown): { err: ProviderError; correlationId: string } {
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
  return { err, correlationId };
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
