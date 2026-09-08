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
}

export function makeError(
  message: string,
  opts: { status?: number; retriable?: boolean } = {},
): ProviderError {
  const e = new Error(message) as ProviderError;
  e.status = opts.status;
  e.retriable = opts.retriable ?? (opts.status === undefined || opts.status >= 500);
  return e;
}
