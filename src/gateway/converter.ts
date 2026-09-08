import { randomBytes } from 'node:crypto';
import type { ChatCompletionRequest, ChatMessage } from '../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../gateway/canonical.js';

/* ================================================================== */
/*  Type helpers (loosely typed — these match the Anthropic Messages   */
/*  API as documented at https://docs.anthropic.com/en/api/messages)  */
/* ================================================================== */

export interface AnthropicRequest {
  model: string;
  max_tokens: number;
  system?: string | Array<{ type: string; text: string }>;
  messages: Array<{
    role: 'user' | 'assistant';
    content: string | Array<{ type: string; text?: string; [k: string]: unknown }>;
  }>;
  temperature?: number;
  stream?: boolean;
  stop_sequences?: string[];
  [k: string]: unknown;
}

export interface AnthropicResponse {
  id: string;
  type: 'message';
  role: 'assistant';
  model: string;
  content: Array<{ type: string; text?: string; [k: string]: unknown }>;
  stop_reason: string | null;
  stop_sequence: string | null;
  usage: { input_tokens: number; output_tokens: number };
}

/* ================================================================== */
/*  Canonical OpenAI  ->  Anthropic backend                            */
/* ================================================================== */

const STOP_MAP: Record<string, string> = {
  stop: 'end_turn',
  length: 'max_tokens',
  tool_calls: 'tool_use',
  content_filter: 'end_turn',
};

export function oaiReqToAnthropicReq(req: ChatCompletionRequest): AnthropicRequest {
  const systemParts: string[] = [];
  const msgs: AnthropicRequest['messages'] = [];

  for (const m of req.messages ?? []) {
    if (m.role === 'system') {
      systemParts.push(textOf(m.content));
    } else if (m.role === 'user' || m.role === 'assistant') {
      const text = textOf(m.content);
      msgs.push({
        role: m.role as 'user' | 'assistant',
        content: [{ type: 'text', text }],
      });
    }
    // tool / function messages are merged as text for simplicity
    else {
      msgs.push({ role: 'user', content: [{ type: 'text', text: textOf(m.content) }] });
    }
  }

  const out: AnthropicRequest = {
    model: req.model,
    max_tokens: (req.max_tokens as number) || 4096,
    messages: msgs,
  };
  if (systemParts.length) out.system = systemParts.join('\n\n');
  if (req.temperature !== undefined) out.temperature = req.temperature;
  if (typeof req.stop === 'string') out.stop_sequences = [req.stop];
  else if (Array.isArray(req.stop)) out.stop_sequences = req.stop as string[];
  // carry over extra fields the backend might understand
  for (const k of Object.keys(req)) {
    if (['model', 'messages', 'max_tokens', 'temperature', 'stop'].includes(k)) continue;
    out[k] = (req as any)[k];
  }
  return out;
}

export function anthropicRespToOAIResp(
  resp: AnthropicResponse,
  requestedModel: string,
): OAIChatResponse {
  const text =
    resp.content
      ?.map((b) => (b.type === 'text' ? b.text ?? '' : ''))
      .join('') ?? '';
  return {
    id: resp.id || 'chatcmpl-' + randomBytes(6).toString('hex'),
    object: 'chat.completion',
    created: Math.floor(Date.now() / 1000),
    model: requestedModel,
    choices: [
      {
        index: 0,
        message: { role: 'assistant', content: text },
        finish_reason: unmapStop(resp.stop_reason),
      },
    ],
    usage: {
      prompt_tokens: resp.usage?.input_tokens ?? 0,
      completion_tokens: resp.usage?.output_tokens ?? 0,
      total_tokens: (resp.usage?.input_tokens ?? 0) + (resp.usage?.output_tokens ?? 0),
    },
  };
}

/** Convert Anthropic SSE events into canonical OpenAI chunks. */
export async function* anthropicStreamToOAIChunks(
  lines: AsyncGenerator<string>,
  model: string,
): AsyncGenerator<OAIStreamChunk> {
  const id = 'chatcmpl-' + randomBytes(6).toString('hex');
  const created = Math.floor(Date.now() / 1000);
  let started = false;

  for await (const line of lines) {
    if (!line.startsWith('data:')) continue;
    const data = line.slice(5).trim();
    if (!data) continue;
    let evt: any;
    try {
      evt = JSON.parse(data);
    } catch {
      continue;
    }
    switch (evt.type) {
      case 'message_start':
        if (!started) {
          started = true;
          yield { id, object: 'chat.completion.chunk', created, model, choices: [{ index: 0, delta: { role: 'assistant' }, finish_reason: null }] };
        }
        break;
      case 'content_block_delta':
        if (evt.delta?.type === 'text_delta' && evt.delta.text) {
          yield { id, object: 'chat.completion.chunk', created, model, choices: [{ index: 0, delta: { content: evt.delta.text }, finish_reason: null }] };
        }
        break;
      case 'message_delta':
        if (evt.delta?.stop_reason) {
          yield { id, object: 'chat.completion.chunk', created, model, choices: [{ index: 0, delta: {}, finish_reason: unmapStop(evt.delta.stop_reason) }] };
        }
        break;
      case 'message_stop':
        return;
    }
  }
}

/* ================================================================== */
/*  Anthropic client  ->  Canonical OpenAI  (gateway edge)            */
/* ================================================================== */

export function anthropicReqToOAIReq(body: AnthropicRequest): ChatCompletionRequest {
  const messages: ChatMessage[] = [];
  if (body.system) {
    const sysText = typeof body.system === 'string'
      ? body.system
      : body.system.map((b) => b.text ?? '').join('\n');
    if (sysText) messages.push({ role: 'system', content: sysText });
  }
  for (const m of body.messages ?? []) {
    const text = Array.isArray(m.content)
      ? m.content.map((b) => b.text ?? '').join('')
      : (m.content as string);
    messages.push({ role: m.role, content: text });
  }
  return {
    model: body.model,
    messages,
    max_tokens: body.max_tokens,
    temperature: body.temperature,
    stream: body.stream,
  };
}

export function oaiRespToAnthropicResp(
  resp: OAIChatResponse,
  requestedModel: string,
): AnthropicResponse {
  const text = (resp.choices?.[0]?.message?.content as string) ?? '';
  const stop = resp.choices?.[0]?.finish_reason ?? 'stop';
  return {
    id: resp.id,
    type: 'message',
    role: 'assistant',
    model: requestedModel,
    content: [{ type: 'text', text }],
    stop_reason: STOP_MAP[stop] ?? 'end_turn',
    stop_sequence: null,
    usage: {
      input_tokens: resp.usage?.prompt_tokens ?? 0,
      output_tokens: resp.usage?.completion_tokens ?? 0,
    },
  };
}

/** Convert canonical OpenAI chunks into Anthropic SSE lines. */
export async function* oaiChunksToAnthropicStream(
  chunks: AsyncGenerator<OAIStreamChunk>,
  model: string,
): AsyncGenerator<string> {
  const msgId = 'msg_' + randomBytes(12).toString('hex');
  let blockOpen = false;
  let first = true;

  for await (const ch of chunks) {
    const delta = ch.choices?.[0]?.delta;
    const finish = ch.choices?.[0]?.finish_reason;

    if (first) {
      first = false;
      yield sse('message_start', {
        type: 'message_start',
        message: { id: msgId, type: 'message', role: 'assistant', model, content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 0, output_tokens: 0 } },
      });
    }
    if (delta?.role === 'assistant' && !blockOpen) {
      yield sse('content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } });
      blockOpen = true;
    }
    if (delta?.content) {
      if (!blockOpen) {
        yield sse('content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } });
        blockOpen = true;
      }
      yield sse('content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: delta.content } });
    }
    if (finish) {
      if (blockOpen) {
        yield sse('content_block_stop', { type: 'content_block_stop', index: 0 });
        blockOpen = false;
      }
      yield sse('message_delta', { type: 'message_delta', delta: { stop_reason: STOP_MAP[finish] ?? 'end_turn', stop_sequence: null }, usage: { output_tokens: 0 } });
      yield sse('message_stop', { type: 'message_stop' });
    }
  }
  if (first) {
    // no chunks at all — emit empty message
    yield sse('message_start', { type: 'message_start', message: { id: msgId, type: 'message', role: 'assistant', model, content: [], stop_reason: null, usage: { input_tokens: 0, output_tokens: 0 } } });
    yield sse('content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } });
    yield sse('content_block_stop', { type: 'content_block_stop', index: 0 });
    yield sse('message_delta', { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 0 } });
    yield sse('message_stop', { type: 'message_stop' });
  }
}

/* ================================================================== */
/*  Utilities                                                          */
/* ================================================================== */

function textOf(content: unknown): string {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    return content
      .map((p: any) => (typeof p === 'string' ? p : p?.text ?? ''))
      .join('');
  }
  return '';
}

function unmapStop(reason: string | null): string {
  const rev: Record<string, string> = {
    end_turn: 'stop',
    max_tokens: 'length',
    stop_sequence: 'stop',
    tool_use: 'tool_calls',
  };
  return rev[reason ?? ''] ?? 'stop';
}

function sse(event: string, data: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}
