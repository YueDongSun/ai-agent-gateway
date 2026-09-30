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
}

export function makeError(
  message: string,
  opts: {
    status?: number;
    retriable?: boolean;
    code?: string;
    stage?: string;
    responseBytes?: number;
  } = {},
): ProviderError {
  const code = stableErrorCode(opts.code ?? message);
  const e = new Error(publicProviderErrorMessage(code, opts.status)) as ProviderError;
  e.status = opts.status;
  e.retriable = opts.retriable ?? (opts.status === undefined || opts.status >= 500);
  e.code = code;
  e.stage = stableErrorCode(opts.stage ?? 'provider');
  e.responseBytes = opts.responseBytes;
  return e;
}

export function makeProviderError(
  code: string,
  opts: {
    status?: number;
    retriable?: boolean;
    stage?: string;
    responseBytes?: number;
  } = {},
): ProviderError {
  return makeError(code, { ...opts, code });
}

export function publicProviderErrorMessage(code: string, status?: number): string {
  switch (code) {
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
      return 'The upstream provider could not be reached.';
    case 'all_hops_exhausted':
      return 'No configured provider completed the request.';
    default:
      return 'The gateway could not complete the request.';
  }
}

function stableErrorCode(value: string): string {
  return /^[a-z][a-z0-9_]{0,63}$/.test(value) ? value : 'provider_error';
}
