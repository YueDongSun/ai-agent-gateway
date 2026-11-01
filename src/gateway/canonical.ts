/**
 * Canonical internal chat format = OpenAI Chat Completions.
 *
 * Every provider receives this shape and returns it, regardless of the
 * backend's native format. The gateway edges convert between this and the
 * client's requested wire format (OpenAI / Anthropic).
 */

export interface OAIChoice {
  index: number;
  message: { role: string; content: string | null; [k: string]: unknown };
  finish_reason: string | null;
}

export interface OAIChatResponse {
  id: string;
  object: 'chat.completion';
  created: number;
  model: string;
  choices: OAIChoice[];
  usage?: { prompt_tokens: number; completion_tokens: number; total_tokens: number };
  [k: string]: unknown;
}

/** A single streaming chunk (the JSON object inside an SSE `data:` line). */
export interface OAIStreamChunk {
  id: string;
  object: 'chat.completion.chunk';
  created: number;
  model: string;
  choices: Array<{
    index: number;
    delta: { role?: string; content?: string; [k: string]: unknown };
    finish_reason: string | null;
  }>;
  [k: string]: unknown;
}

export interface ProviderError extends Error {
  status?: number;
  retriable: boolean;
  code: string;
  stage: string;
  responseBytes?: number;
  /** Upstream 4xx body classification (see `classifyUpstream4xx`). */
  kind?: UpstreamErrorKind;
}

/**
 * Classification of an upstream 4xx error body, mirroring the Rust proxy's
 * `classify_upstream_4xx` (src-tauri/src/proxy/canonical.rs).
 * - `auth_expired`: the upstream credential is invalid and re-authentication
 *   is required; never retried, never hidden by failover.
 * - `tool_capability`: the hop cannot serve the request's tools; a finite
 *   route may route around it to a later hop.
 */
export type UpstreamErrorKind = 'auth_expired' | 'tool_capability';

/**
 * Inspect a parsed JSON error body for the two classification buckets.
 * Returns undefined when the JSON carries no recognizable structured
 * signal; the caller falls back to substring matching.
 */
function classifyErrorJson(value: Record<string, unknown>): UpstreamErrorKind | undefined {
  // Rust parity: a present-but-non-object `error` field yields NO readable
  // fields (serde .get on a scalar returns None); only its absence falls
  // back to the top-level object.
  let error: Record<string, unknown>;
  if ('error' in value) {
    error =
      value.error && typeof value.error === 'object' && !Array.isArray(value.error)
        ? (value.error as Record<string, unknown>)
        : {};
  } else {
    error = value;
  }
  const code = typeof error.code === 'string' ? error.code : '';
  const kind = typeof error.type === 'string' ? error.type : '';
  const message = typeof error.message === 'string' ? error.message : '';
  const nested = (
    error.error && typeof error.error === 'object' ? error.error : {}
  ) as Record<string, unknown>;
  const nestedCode = typeof nested.code === 'string' ? nested.code : '';

  const lowerCode = code.toLowerCase();
  const lowerKind = kind.toLowerCase();
  const lowerMessage = message.toLowerCase();
  const lowerNested = nestedCode.toLowerCase();

  if (
    lowerCode === 'invalid_grant'
    || lowerNested === 'invalid_grant'
    || lowerCode.includes('refresh_token')
    || lowerCode.includes('invalid_api_key')
    || lowerCode.includes('authentication')
    || lowerMessage.includes('refresh token')
    || lowerMessage.includes('sign in again')
    || lowerMessage.includes('access token could not be refreshed')
  ) {
    return 'auth_expired';
  }
  if (
    lowerCode.includes('tool')
    || lowerKind.includes('tool')
    || lowerMessage.includes('unsupported tool')
    || lowerMessage.includes('tool not supported')
    || lowerMessage.includes('tools are not supported')
    || lowerMessage.includes('invalid function')
    || lowerMessage.includes('invalid schema')
  ) {
    return 'tool_capability';
  }
  return undefined;
}

const AUTH_KEYWORDS = [
  'invalid_grant',
  'access token could not be refreshed',
  'sign in again',
  'signed in to another account',
  // Spaced form only: the underscored `refresh_token` also appears in many
  // request echoes, which are not auth failures (Rust parity).
  'refresh token',
];

const TOOL_KEYWORDS = [
  'unsupported tool',
  'unknown tool',
  'tool not supported',
  'tools are not supported',
  'invalid function',
  'invalid schema',
  'function name',
];

const auth_keyword_encoder = new TextEncoder();
const auth_keyword_decoder = new TextDecoder();

/**
 * Inspect an upstream 4xx body and classify it: JSON error fields first
 * (more reliable than the free-form message), then a lower-cased substring
 * scan over the first 8 KiB. `tools[0]`-style JSON-path strings are
 * deliberately not tool keywords: a request-echo body is not a
 * tool-capability rejection. Ports `classify_upstream_4xx` from the Rust
 * proxy; returns undefined for non-4xx statuses and unclassified bodies.
 *
 * The scan window is the first 8192 BYTES, not code units: the whole
 * (already capped) body is encoded and sliced afterwards so a multibyte
 * character at the window boundary behaves exactly like the Rust
 * byte-window scan.
 */
export function classifyUpstream4xx(
  status: number | undefined,
  body: string,
): UpstreamErrorKind | undefined {
  if (status === undefined || status < 400 || status >= 500) return undefined;
  const headBytes = auth_keyword_encoder.encode(body).subarray(0, 8192);
  const head = auth_keyword_decoder.decode(headBytes);
  try {
    const parsed: unknown = JSON.parse(head);
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
      const classified = classifyErrorJson(parsed as Record<string, unknown>);
      if (classified) return classified;
    }
  } catch {
    // Not JSON - fall through to the substring scan.
  }
  const lower = head.toLowerCase();
  for (const keyword of AUTH_KEYWORDS) {
    if (lower.includes(keyword)) return 'auth_expired';
  }
  for (const keyword of TOOL_KEYWORDS) {
    if (lower.includes(keyword)) return 'tool_capability';
  }
  return undefined;
}

export function makeError(
  message: string,
  opts: {
    status?: number;
    retriable?: boolean;
    code?: string;
    stage?: string;
    responseBytes?: number;
    kind?: UpstreamErrorKind;
  } = {},
): ProviderError {
  const code = stableErrorCode(opts.code ?? message);
  const e = new Error(publicProviderErrorMessage(code, opts.status)) as ProviderError;
  e.status = opts.status;
  // Rust parity: the default retriable rule is 429 or any 5xx (and any
  // error without a status at all).
  e.retriable = opts.retriable ?? (opts.status === undefined || opts.status === 429 || opts.status >= 500);
  e.code = code;
  e.stage = stableErrorCode(opts.stage ?? 'provider');
  e.responseBytes = opts.responseBytes;
  e.kind = opts.kind;
  return e;
}

export function makeProviderError(
  code: string,
  opts: {
    status?: number;
    retriable?: boolean;
    stage?: string;
    responseBytes?: number;
    kind?: UpstreamErrorKind;
  } = {},
): ProviderError {
  return makeError(code, { ...opts, code });
}

/**
 * A terminal stream error chunk (port of the Rust proxy's
 * `stream_error_chunk`). The client-supplied model is never echoed: error
 * envelopes use a fixed value so a model identifier cannot leak private
 * material.
 */
export function makeStreamErrorChunk(status: number, kind: string): OAIStreamChunk {
  return {
    id: 'proxy_error',
    object: 'chat.completion.chunk',
    created: Math.floor(Date.now() / 1000),
    model: 'unavailable',
    choices: [],
    error: {
      type: kind,
      message: 'Upstream stream failed before completion',
      status,
    },
  };
}

/**
 * Port of the Rust proxy's `stream_error_status`: map an in-stream error
 * event to an HTTP status. Unknown application errors are terminal; only
 * known overload/rate failures or explicit 5xx statuses can advance the
 * provider chain. A present-but-non-object `error` field still lets the
 * numeric keys consult the whole event (serde `or_else`), but the type
 * table reads nothing in that case — exactly like the reference.
 */
export function streamErrorStatus(event: unknown): number {
  const obj: Record<string, unknown> =
    event && typeof event === 'object' && !Array.isArray(event)
      ? (event as Record<string, unknown>)
      : {};
  const hasError = 'error' in obj && obj.error !== undefined;
  const error: Record<string, unknown> =
    hasError && obj.error && typeof obj.error === 'object' && !Array.isArray(obj.error)
      ? (obj.error as Record<string, unknown>)
      : {};
  for (const key of ['status', 'status_code', 'code']) {
    const value = hasError && error[key] !== undefined ? error[key] : obj[key];
    let numeric = NaN;
    if (typeof value === 'number' && Number.isInteger(value)) {
      numeric = value;
    } else if (typeof value === 'string' && /^\d+$/.test(value)) {
      numeric = Number.parseInt(value, 10);
    }
    if (numeric >= 400 && numeric <= 599) {
      return numeric;
    }
  }
  const type = hasError
    ? typeof error.type === 'string'
      ? error.type
      : ''
    : typeof obj.type === 'string'
      ? obj.type
      : '';
  switch (type) {
    case 'rate_limit_error':
      return 429;
    case 'overloaded_error':
      return 529;
    case 'api_error':
    case 'proxy_stream_error':
      return 502;
    case 'authentication_error':
      return 401;
    case 'permission_error':
      return 403;
    default:
      return 400;
  }
}

/**
 * Port of the Rust proxy's `stream_chunk_error`: detect an upstream error
 * event riding inside a stream chunk. Any present `error` field counts —
 * including scalars, which classify as a terminal 400 — matching the
 * reference. Returns undefined for ordinary chunks. Retriability follows
 * the mapped status (429/5xx retriable, other 4xx terminal).
 */
export function streamChunkError(chunk: OAIStreamChunk): ProviderError | undefined {
  const holder = chunk as { error?: unknown };
  if (!('error' in holder) || holder.error === undefined) return undefined;
  const status = streamErrorStatus(holder.error);
  return makeProviderError('upstream_stream_error', { status, stage: 'stream_event' });
}

export function publicProviderErrorMessage(code: string, status?: number): string {
  switch (code) {
    case 'invalid_model':
      return 'The requested model is unavailable. Choose an available model and try again.';
    case 'no_route':
      return 'No route is available for the requested model.';
    case 'upstream_http_error':
      if (status === 429) return 'The upstream provider is rate limited.';
      if (status !== undefined && status >= 400 && status < 500) {
        return 'The upstream provider rejected the request.';
      }
      return 'The upstream provider is unavailable.';
    case 'upstream_decode_error':
      return 'The upstream provider returned an invalid response.';
    case 'upstream_stream_error':
      return 'The upstream stream failed before completion.';
    case 'upstream_transport_error':
    case 'upstream_timeout':
      return 'The upstream model is temporarily unavailable. Please retry shortly.';
    case 'all_hops_exhausted':
      return 'No configured provider completed the request.';
    case 'invalid_request':
      // Rust parity: public_error_message in src-tauri/src/proxy/canonical.rs.
      return 'The request is invalid.';
    default:
      return 'The gateway could not complete the request.';
  }
}

function stableErrorCode(value: string): string {
  return /^[a-z][a-z0-9_]{0,63}$/.test(value) ? value : 'provider_error';
}
