import { randomBytes } from 'node:crypto';
import type { ChatCompletionRequest, ChatMessage } from '../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../gateway/canonical.js';
import { makeStreamErrorChunk, streamErrorStatus } from '../gateway/canonical.js';
import { anthropic_responses, anthropic_events } from './anthropic_envelope.js';

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

/* ================================================================== */
/*  Tool shape conversion (Rust proxy parity: converter/anthropic.rs  */
/*  `oai_tool_to_anthropic_tool` / `anthropic_tool_to_oai_tool`)      */
/* ================================================================== */

/** Translate the canonical OpenAI function wrapper into the Anthropic
 * tool shape. Native/unknown definitions remain intact so the provider
 * can reject them; never silently remove a requested tool. */
export function oai_tool_to_anthropic_tool(tool: Record<string, unknown>): Record<string, unknown> {
  if (tool['type'] !== 'function') return tool;
  const fn = tool['function'];
  if (!fn || typeof fn !== 'object') return tool;
  const name = (fn as Record<string, unknown>)['name'];
  if (typeof name !== 'string') return tool;
  const converted: Record<string, unknown> = {
    name,
    input_schema:
      (fn as Record<string, unknown>)['parameters'] ?? { type: 'object', properties: {} },
  };
  if ('description' in (fn as Record<string, unknown>)) {
    converted['description'] = (fn as Record<string, unknown>)['description'];
  }
  return converted;
}

/** Translate the Anthropic tool shape into the canonical OpenAI function
 * wrapper — the exact inverse of `oai_tool_to_anthropic_tool`. */
export function anthropic_tool_to_oai_tool(
  tool: Record<string, unknown>,
): Record<string, unknown> {
  if (tool['type'] === 'function') return tool;
  const name = tool['name'];
  if (typeof name !== 'string' || !('input_schema' in tool)) return tool;
  const fn: Record<string, unknown> = { name, parameters: tool['input_schema'] };
  if ('description' in tool) fn['description'] = tool['description'];
  return { type: 'function', function: fn };
}

/** Append a tool_result block to the trailing user message, merging
 * consecutive tool results into one message (Anthropic requires each
 * tool_result message to carry only tool_result blocks). */
function append_anthropic_tool_result(
  msgs: AnthropicRequest['messages'],
  block: Record<string, unknown>,
): void {
  type block_shape = { type: string; text?: string; [k: string]: unknown };
  const last = msgs[msgs.length - 1];
  const is_tool_result_message =
    last &&
    last.role === 'user' &&
    Array.isArray(last.content) &&
    last.content.length > 0 &&
    last.content.every((b) => b.type === 'tool_result');
  if (is_tool_result_message) {
    (last.content as block_shape[]).push(block as block_shape);
  } else {
    msgs.push({ role: 'user', content: [block as block_shape] });
  }
}

export function oaiReqToAnthropicReq(req: ChatCompletionRequest): AnthropicRequest {
  const systemParts: string[] = [];
  const msgs: AnthropicRequest['messages'] = [];

  for (const m of req.messages ?? []) {
    if (m.role === 'system') {
      systemParts.push(textOf(m.content));
    } else if (m.role === 'tool') {
      // OpenAI tool role -> Anthropic user message with a tool_result
      // block; array-shaped content is preserved in array form.
      const block: Record<string, unknown> = {
        type: 'tool_result',
        tool_use_id: (m.tool_call_id as string) ?? '',
        content: Array.isArray(m.content) ? m.content : textOf(m.content),
      };
      append_anthropic_tool_result(msgs, block);
    } else if (m.role === 'user' || m.role === 'assistant') {
      const blocks: Array<Record<string, unknown>> = [];
      const text = textOf(m.content);
      if (text) blocks.push({ type: 'text', text });
      // Assistant tool_calls ride as tool_use blocks on the same message.
      if (m.role === 'assistant' && Array.isArray(m.tool_calls)) {
        for (const tc of m.tool_calls as Array<Record<string, unknown>>) {
          const fn = (tc['function'] ?? {}) as Record<string, unknown>;
          let input: unknown = {};
          try {
            input = JSON.parse(typeof fn['arguments'] === 'string' ? fn['arguments'] : '{}');
          } catch {
            input = {};
          }
          blocks.push({
            type: 'tool_use',
            id: (tc['id'] as string) ?? '',
            name: (fn['name'] as string) ?? '',
            input,
          });
        }
      }
      msgs.push({
        role: m.role as 'user' | 'assistant',
        content: (blocks.length
          ? blocks
          : [{ type: 'text', text: '' }]) as AnthropicRequest['messages'][number]['content'],
      });
    } else {
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
  if (Array.isArray(req.tools)) {
    out.tools = (req.tools as Array<Record<string, unknown>>).map(oai_tool_to_anthropic_tool);
  }
  // carry over extra fields the backend might understand (tools are
  // converted above, never forwarded in the OpenAI shape)
  for (const k of Object.keys(req)) {
    if (['model', 'messages', 'max_tokens', 'temperature', 'stop', 'tools'].includes(k)) continue;
    out[k] = (req as any)[k];
  }
  return out;
}

export function anthropicRespToOAIResp(
  resp: AnthropicResponse,
  requestedModel: string,
): OAIChatResponse {
  const text: string =
    resp.content
      ?.map((b) => (b.type === 'text' ? b.text ?? '' : ''))
      .join('') ?? '';
  // tool_use blocks become assistant tool_calls (input JSON-stringified,
  // the OpenAI wire shape the canonical router speaks).
  const tool_calls = (resp.content ?? [])
    .filter((b) => b.type === 'tool_use')
    .map((b) => ({
      id: (b['id'] as string) ?? '',
      type: 'function',
      function: {
        name: (b['name'] as string) ?? '',
        arguments: JSON.stringify(b['input'] ?? {}),
      },
    }));
  const message: Record<string, unknown> = {
    role: 'assistant',
    content: tool_calls.length && !text ? null : text,
  };
  if (tool_calls.length) message['tool_calls'] = tool_calls;
  return {
    id: resp.id || 'chatcmpl-' + randomBytes(6).toString('hex'),
    object: 'chat.completion',
    created: Math.floor(Date.now() / 1000),
    model: requestedModel,
    choices: [
      {
        index: 0,
        message: message as OAIChatResponse['choices'][number]['message'],
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
  preserve_source = false,
): AsyncGenerator<OAIStreamChunk> {
  const id = 'chatcmpl-' + randomBytes(6).toString('hex');
  const created = Math.floor(Date.now() / 1000);
  let started = false;
  let terminal = false;

  for await (const line of lines) {
    if (!line.startsWith('data:')) continue;
    const data = line.slice(5).trim();
    if (!data) continue;
    let evt: any;
    try {
      evt = JSON.parse(data);
    } catch {
      if (preserve_source) {
        yield makeStreamErrorChunk(502, 'proxy_stream_error');
        return;
      }
      continue;
    }
    if (!evt || typeof evt !== 'object' || typeof evt.type !== 'string'
      || !/^[a-z_]{1,64}$/.test(evt.type)) {
      if (preserve_source) {
        yield makeStreamErrorChunk(502, 'proxy_stream_error');
        return;
      }
      continue;
    }
    if (preserve_source) {
      // Error payloads are normalized even on a same-protocol relay.
      if (evt.type === 'error') {
        yield makeStreamErrorChunk(streamErrorStatus(evt), 'upstream_error');
        return;
      }
      const chunk: OAIStreamChunk = {
        id, object: 'chat.completion.chunk', created, model, choices: [],
      };
      if (evt.type === 'message_start') {
        chunk.choices.push({ index: 0, delta: { role: 'assistant' }, finish_reason: null });
      } else if (evt.type === 'content_block_delta' && evt.delta?.type === 'text_delta') {
        chunk.choices.push({ index: 0, delta: { content: evt.delta.text }, finish_reason: null });
      } else if (evt.type === 'message_stop') {
        chunk.choices.push({ index: 0, delta: {}, finish_reason: 'stop' });
      }
      anthropic_events.set(chunk, evt);
      yield chunk;
      if (evt.type === 'message_stop') return;
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
          terminal = true;
          yield { id, object: 'chat.completion.chunk', created, model, choices: [{ index: 0, delta: {}, finish_reason: unmapStop(evt.delta.stop_reason) }] };
        }
        break;
      case 'message_stop':
        // A clean upstream end is terminal even when the final message_delta
        // carried no stop_reason: without this fallback chunk the router's
        // missing-terminal watchdog would turn a successful stream into a
        // spurious 502 (Rust counts the native message_stop event itself).
        if (!terminal) {
          yield { id, object: 'chat.completion.chunk', created, model, choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] };
        }
        return;
      case 'error': {
        // Upstream error event: surface it as a choice-less error chunk so
        // the router's stream-chunk error gate sees it (failover when
        // retriable, client-visible error otherwise). The chunk is fully
        // normalized (fixed id/model/type/message, mapped status only) so
        // upstream-controlled strings never reach client envelopes.
        yield makeStreamErrorChunk(streamErrorStatus(evt), 'upstream_error');
        return;
      }
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
    if (!Array.isArray(m.content)) {
      messages.push({ role: m.role, content: m.content as string });
      continue;
    }
    // Split content blocks: text joins into the message content, tool_use
    // becomes assistant tool_calls, tool_result becomes tool-role messages
    // (Rust proxy parity: converter/anthropic.rs ingress mapping).
    const texts: string[] = [];
    const tool_calls: Array<Record<string, unknown>> = [];
    for (const b of m.content) {
      if (b.type === 'tool_use') {
        tool_calls.push({
          id: (b['id'] as string) ?? '',
          type: 'function',
          function: {
            name: (b['name'] as string) ?? '',
            arguments: JSON.stringify(b['input'] ?? {}),
          },
        });
      } else if (b.type === 'tool_result') {
        const content = b['content'];
        messages.push({
          role: 'tool',
          tool_call_id: (b['tool_use_id'] as string) ?? '',
          content: typeof content === 'string' ? content : (content ?? ''),
        });
      } else if (typeof b.text === 'string') {
        texts.push(b.text);
      }
    }
    const message: ChatMessage = { role: m.role, content: texts.join('') };
    if (tool_calls.length) {
      message.content = texts.join('') || undefined;
      message['tool_calls'] = tool_calls;
    }
    messages.push(message);
  }
  const out: ChatCompletionRequest = {
    model: body.model,
    messages,
    max_tokens: body.max_tokens,
    temperature: body.temperature,
    stream: body.stream,
  };
  if (Array.isArray(body.tools)) {
    out.tools = (body.tools as Array<Record<string, unknown>>).map(anthropic_tool_to_oai_tool);
  }
  return out;
}

export function oaiRespToAnthropicResp(
  resp: OAIChatResponse,
  requestedModel: string,
): AnthropicResponse {
  const source = anthropic_responses.get(resp);
  if (source) return { ...source, model: requestedModel };
  const choice = resp.choices?.[0];
  const text = (choice?.message?.content as string) ?? '';
  const stop = choice?.finish_reason ?? 'stop';
  const content: Array<{ type: string; text?: string; [k: string]: unknown }> = [];
  if (text) content.push({ type: 'text', text });
  // Assistant tool_calls become tool_use blocks (arguments parsed back to
  // an object — the Anthropic wire shape).
  for (const tc of (choice?.message?.tool_calls as Array<Record<string, unknown>>) ?? []) {
    const fn = (tc['function'] ?? {}) as Record<string, unknown>;
    let input: unknown = {};
    try {
      input = JSON.parse(typeof fn['arguments'] === 'string' ? fn['arguments'] : '{}');
    } catch {
      input = {};
    }
    content.push({
      type: 'tool_use',
      id: (tc['id'] as string) ?? '',
      name: (fn['name'] as string) ?? '',
      input,
    });
  }
  if (!content.length) content.push({ type: 'text', text: '' });
  return {
    id: resp.id,
    type: 'message',
    role: 'assistant',
    model: requestedModel,
    content,
    stop_reason: STOP_MAP[stop] ?? 'end_turn',
    stop_sequence: null,
    usage: {
      input_tokens: resp.usage?.prompt_tokens ?? 0,
      output_tokens: resp.usage?.completion_tokens ?? 0,
    },
  };
}

/**
 * State for one in-flight tool_call delta stream, keyed by OpenAI
 * `tool_calls.index` (Rust parity: `ToolBlockState` in
 * src-tauri/src/proxy/converter/anthropic_stream.rs). The anthropic index is
 * allocated when the upstream first mentions the OpenAI index — before the
 * id/name arrive — so `content_block_start` must wait, and pre-start
 * `function.arguments` fragments buffer in `pending_args`.
 */
type tool_block_state = {
  anthropic_index: number;
  id: string;
  name: string;
  started: boolean;
  pending_args: string;
};

/**
 * Build the Anthropic usage object from a chunk's top-level usage block,
 * undoing OpenAI's "cache tokens counted in prompt_tokens" convention
 * (Rust parity: `build_anthropic_usage_from_stream_extra`). Returns
 * undefined when the chunk carries no usable usage.
 */
function anthropic_usage_from_stream_chunk(chunk: OAIStreamChunk): Record<string, number> | undefined {
  const usage = (chunk as { usage?: unknown }).usage;
  if (!usage || typeof usage !== 'object') return undefined;
  const u = usage as Record<string, unknown>;
  const count = (v: unknown): number =>
    typeof v === 'number' && Number.isFinite(v) && v > 0 ? Math.floor(v) : 0;
  if ('input_tokens' in u || 'output_tokens' in u) {
    const normalized: Record<string, number> = {
      input_tokens: count(u.input_tokens),
      output_tokens: count(u.output_tokens),
    };
    for (const field of ['cache_read_input_tokens', 'cache_creation_input_tokens']) {
      const value = count(u[field]);
      if (value > 0) normalized[field] = value;
    }
    return normalized;
  }
  const details = u.prompt_tokens_details as Record<string, unknown> | undefined;
  const cached = count(details?.cached_tokens);
  const cache_creation = count(details?.cache_write_tokens);
  const input = Math.max(count(u.prompt_tokens) - cached - cache_creation, 0);
  const normalized: Record<string, number> = {
    input_tokens: input,
    output_tokens: count(u.completion_tokens),
  };
  if (cached > 0) normalized.cache_read_input_tokens = cached;
  if (cache_creation > 0) normalized.cache_creation_input_tokens = cache_creation;
  return normalized;
}

/**
 * Convert canonical OpenAI chunks into Anthropic SSE lines. Multi-block
 * state machine (Rust parity: `oai_chunks_to_anthropic_stream` in
 * src-tauri/src/proxy/converter/anthropic_stream.rs): text and thinking
 * blocks plus one tool_use block per `tool_calls.index` share a single
 * content index space; `message_delta` is deferred to stream end so a
 * trailing usage-only chunk can still fill in the final numbers.
 */
export async function* oaiChunksToAnthropicStream(
  chunks: AsyncGenerator<OAIStreamChunk>,
  model: string,
): AsyncGenerator<string> {
  const msgId = 'msg_' + randomBytes(12).toString('hex');
  let message_started = false;
  let text_block_open = false;
  let thinking_block_open = false;
  let next_content_index = 0;
  let has_emitted_message_delta = false;
  let source_message_stop_emitted = false;
  let saw_chunk = false;
  const tool_blocks = new Map<number, tool_block_state>();
  const open_tool_indices = new Set<number>();
  let latest_usage: Record<string, number> | undefined;
  let pending_message_delta:
    | { type: 'message_delta'; delta: { stop_reason: string; stop_sequence: null }; usage: Record<string, number> }
    | undefined;

  // Only one of text/thinking can be open at a time; whichever is open is
  // always the most recently allocated block.
  function* close_trailing_block(): Generator<string> {
    if (text_block_open || thinking_block_open) {
      yield sse('content_block_stop', { type: 'content_block_stop', index: next_content_index - 1 });
      text_block_open = false;
      thinking_block_open = false;
    }
  }

  const message_start_frame = (): string => {
    message_started = true;
    return sse('message_start', {
      type: 'message_start',
      message: { id: msgId, type: 'message', role: 'assistant', model, content: [], stop_reason: null, stop_sequence: null, usage: latest_usage ?? { input_tokens: 0, output_tokens: 0 } },
    });
  };

  for await (const ch of chunks) {
    saw_chunk = true;
    const source = anthropic_events.get(ch);
    if (source) {
      const source_type = typeof source.type === 'string' ? source.type : 'message';
      if (source_type === 'message_start') message_started = true;
      if (source_type === 'message_stop') source_message_stop_emitted = true;
      const event = source_type === 'message_start' && source.message
        ? { ...source, message: { ...source.message, model } }
        : source;
      yield sse(event.type, event);
      // Rust parity: a passthrough error event is terminal.
      if (source_type === 'error') return;
      continue;
    }
    const chunkError = (ch as { error?: unknown }).error;
    if (chunkError && typeof chunkError === 'object') {
      // Error chunks (watchdog or upstream error events) must be visible on
      // the Anthropic surface: close any open blocks, emit a terminal error
      // event, and stop. Without this, Anthropic-protocol clients saw an
      // abruptly ending stream with no error and no message_stop.
      if (!message_started) yield message_start_frame();
      yield* close_trailing_block();
      for (const idx of [...open_tool_indices].sort((a, b) => a - b)) {
        yield sse('content_block_stop', { type: 'content_block_stop', index: idx });
      }
      const err = chunkError as { type?: unknown; message?: unknown };
      yield sse('error', {
        type: 'error',
        error: {
          type: typeof err.type === 'string' ? err.type : 'api_error',
          message:
            typeof err.message === 'string' ? err.message : 'Upstream stream failed before completion',
        },
      });
      return;
    }

    const usage_now = anthropic_usage_from_stream_chunk(ch);
    if (usage_now) {
      latest_usage = usage_now;
      if (pending_message_delta) pending_message_delta.usage = usage_now;
    }

    if (!message_started) yield message_start_frame();

    const finish = ch.choices?.[0]?.finish_reason ?? null;

    for (const choice of ch.choices ?? []) {
      const delta = choice.delta as {
        role?: string;
        content?: string;
        reasoning?: unknown;
        tool_calls?: unknown;
      };

      // Reasoning deltas become thinking blocks (Rust `reasoning_text`).
      const reasoning = typeof delta.reasoning === 'string' ? delta.reasoning : '';
      if (reasoning) {
        if (text_block_open) yield* close_trailing_block();
        if (!thinking_block_open) {
          const idx = next_content_index++;
          yield sse('content_block_start', { type: 'content_block_start', index: idx, content_block: { type: 'thinking', thinking: '' } });
          thinking_block_open = true;
        }
        yield sse('content_block_delta', { type: 'content_block_delta', index: next_content_index - 1, delta: { type: 'thinking_delta', thinking: reasoning } });
      }

      if (delta.content) {
        if (thinking_block_open) yield* close_trailing_block();
        if (!text_block_open) {
          const idx = next_content_index++;
          yield sse('content_block_start', { type: 'content_block_start', index: idx, content_block: { type: 'text', text: '' } });
          text_block_open = true;
        }
        yield sse('content_block_delta', { type: 'content_block_delta', index: next_content_index - 1, delta: { type: 'text_delta', text: delta.content } });
      }

      const tool_calls = delta.tool_calls;
      if (Array.isArray(tool_calls) && tool_calls.length > 0) {
        // A tool block must never share an open text/thinking index: close
        // them before allocating the tool block.
        yield* close_trailing_block();
        for (const tc of tool_calls) {
          const tc_rec = tc as Record<string, unknown>;
          // OpenAI tool_calls entries may carry `index` as a number or as a
          // numeric string depending on the provider.
          const raw_index = tc_rec.index;
          const oai_index =
            typeof raw_index === 'number' && Number.isInteger(raw_index) && raw_index >= 0
              ? raw_index
              : typeof raw_index === 'string' && /^\d+$/.test(raw_index)
                ? Number(raw_index)
                : 0;
          let state = tool_blocks.get(oai_index);
          if (!state) {
            state = { anthropic_index: next_content_index++, id: '', name: '', started: false, pending_args: '' };
            tool_blocks.set(oai_index, state);
          }
          const fn = (tc_rec.function ?? {}) as Record<string, unknown>;
          if (typeof tc_rec.id === 'string' && tc_rec.id) state.id = tc_rec.id;
          if (typeof fn.name === 'string' && fn.name) state.name = fn.name;
          if (!state.started && state.id && state.name) {
            yield sse('content_block_start', {
              type: 'content_block_start',
              index: state.anthropic_index,
              content_block: { type: 'tool_use', id: state.id, name: state.name, input: {} },
            });
            open_tool_indices.add(state.anthropic_index);
            state.started = true;
            if (state.pending_args) {
              const pending = state.pending_args;
              state.pending_args = '';
              yield sse('content_block_delta', { type: 'content_block_delta', index: state.anthropic_index, delta: { type: 'input_json_delta', partial_json: pending } });
            }
          }
          if (typeof fn.arguments === 'string') {
            if (state.started) {
              yield sse('content_block_delta', { type: 'content_block_delta', index: state.anthropic_index, delta: { type: 'input_json_delta', partial_json: fn.arguments } });
            } else {
              state.pending_args += fn.arguments;
            }
          }
        }
      }
    }

    if (finish && !has_emitted_message_delta) {
      has_emitted_message_delta = true;
      yield* close_trailing_block();

      // Late-start tool blocks that never received id/name.
      const late_starts: Array<{ anthropic_index: number; id: string; name: string; pending_args: string }> = [];
      for (const [oai_index, state] of tool_blocks) {
        if (state.started) continue;
        if (!state.pending_args && !state.id && !state.name) continue;
        const late = {
          anthropic_index: state.anthropic_index,
          id: state.id || `tool_call_${oai_index}`,
          name: state.name || 'unknown_tool',
          pending_args: state.pending_args,
        };
        state.started = true;
        state.pending_args = '';
        late_starts.push(late);
      }
      late_starts.sort((a, b) => a.anthropic_index - b.anthropic_index);
      for (const late of late_starts) {
        yield sse('content_block_start', {
          type: 'content_block_start',
          index: late.anthropic_index,
          content_block: { type: 'tool_use', id: late.id, name: late.name },
        });
        open_tool_indices.add(late.anthropic_index);
        if (late.pending_args) {
          yield sse('content_block_delta', { type: 'content_block_delta', index: late.anthropic_index, delta: { type: 'input_json_delta', partial_json: late.pending_args } });
        }
      }

      for (const idx of [...open_tool_indices].sort((a, b) => a - b)) {
        yield sse('content_block_stop', { type: 'content_block_stop', index: idx });
      }
      open_tool_indices.clear();

      // Defer message_delta to stream end so a later usage-only chunk can
      // still fill in the final numbers.
      pending_message_delta = {
        type: 'message_delta',
        delta: { stop_reason: STOP_MAP[finish] ?? 'end_turn', stop_sequence: null },
        usage: { input_tokens: 0, output_tokens: 0, ...(latest_usage ?? {}) },
      };
    }
  }

  if (!saw_chunk) {
    // no chunks at all — emit empty message
    yield message_start_frame();
    yield sse('content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } });
    yield sse('content_block_stop', { type: 'content_block_stop', index: 0 });
    yield sse('message_delta', { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 0 } });
    yield sse('message_stop', { type: 'message_stop' });
    return;
  }
  if (pending_message_delta) yield sse('message_delta', pending_message_delta);
  if (!source_message_stop_emitted) yield sse('message_stop', { type: 'message_stop' });
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
