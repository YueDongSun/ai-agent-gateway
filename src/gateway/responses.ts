/**
 * OpenAI Responses API <-> canonical Chat Completions conversion and the
 * `/v1/responses` SSE event surface (Rust proxy parity: the flat
 * conversions in src-tauri/src/proxy/converter/responses.rs and the wire
 * envelope/event shapes in src-tauri/src/proxy/responses.rs). The Node
 * runtime has no Responses-native providers, so no relay sidecars or
 * namespace maps exist here — only the Chat-shaped conversion.
 */
import { randomBytes } from 'node:crypto';
import type { ChatCompletionRequest, ChatMessage } from '../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from './canonical.js';

/** Streaming completion sentinel for the Responses surface. */
export const RESPONSES_STREAM_DONE = 'data: [DONE]\n\n';

/* ------------------------------------------------------------------ */
/*  Request: Responses -> canonical Chat                               */
/* ------------------------------------------------------------------ */

/**
 * Flatten a Responses request into the canonical Chat Completions shape.
 * Input items are converted by `type`, never flattened indiscriminately;
 * gateway-opaque item types (`reasoning`, `item_reference`, ...) are
 * dropped because they are stateless for a chat-completions upstream.
 */
export function responses_request_to_chat(body: Record<string, unknown>): ChatCompletionRequest {
  const messages: ChatMessage[] = [];

  const instructions = body.instructions;
  if (typeof instructions === 'string' && instructions) {
    messages.push({ role: 'system', content: instructions });
  }

  if (typeof body.input === 'string') {
    messages.push({ role: 'user', content: body.input });
  } else if (Array.isArray(body.input)) {
    for (const item of body.input) {
      if (item && typeof item === 'object') {
        const message = responses_input_item_to_chat(item as Record<string, unknown>);
        if (message) append_responses_input_message(messages, message);
      }
    }
  } else {
    messages.push({ role: 'user', content: '' });
  }

  const chat_request: ChatCompletionRequest = {
    model: typeof body.model === 'string' ? body.model : '',
    messages,
  };
  if (body.stream === true) chat_request.stream = true;
  if (typeof body.temperature === 'number') chat_request.temperature = body.temperature;
  if (typeof body.max_output_tokens === 'number') chat_request.max_tokens = body.max_output_tokens;
  const tools = responses_tools_to_chat(body.tools);
  if (tools) chat_request.tools = tools;
  const tool_choice = responses_tool_choice_to_chat(body.tool_choice);
  if (tool_choice !== undefined) chat_request.tool_choice = tool_choice;
  return chat_request;
}

/**
 * Merge a converted assistant item into the previous assistant message
 * when it only carries tool calls — adjacent Responses `function_call`
 * items belong to the same assistant turn.
 */
function append_responses_input_message(messages: ChatMessage[], message: ChatMessage): void {
  if (message.role === 'assistant' && message.content === null && Array.isArray(message.tool_calls)) {
    const previous = messages[messages.length - 1];
    if (previous && previous.role === 'assistant') {
      const merged = Array.isArray(previous.tool_calls) ? previous.tool_calls : [];
      previous.tool_calls = [...merged, ...message.tool_calls];
      return;
    }
  }
  messages.push(message);
}

/**
 * Translate one Responses input item into a canonical chat message.
 * Returns null for item types without a Chat Completions equivalent —
 * dropping them is safe; forwarding them as bogus messages is not
 * (`function_call` has no `role`/`content` and would flatten into a
 * guaranteed-400 `{"role":"user","content":null}`).
 */
function responses_input_item_to_chat(item: Record<string, unknown>): ChatMessage | null {
  const item_type = typeof item.type === 'string' ? item.type : 'message';
  const str = (value: unknown): string | undefined => (typeof value === 'string' ? value : undefined);

  if (item_type === 'function_call') {
    const arguments_value = item.arguments;
    return {
      role: 'assistant',
      // Chat Completions permits null content on assistant tool-call
      // messages; every other combination becomes a string below.
      content: null,
      tool_calls: [
        {
          id: str(item.call_id) ?? str(item.id) ?? 'call_0',
          type: 'function',
          function: {
            name: str(item.name) ?? '',
            arguments:
              typeof arguments_value === 'string'
                ? arguments_value
                : arguments_value === undefined
                  ? ''
                  : JSON.stringify(arguments_value),
          },
        },
      ],
    };
  }
  if (item_type === 'function_call_output') {
    return {
      role: 'tool',
      content: function_call_output_text(item.output ?? null),
      tool_call_id: str(item.call_id) ?? str(item.id) ?? 'call_0',
    };
  }
  if (item_type === 'message') {
    const raw_role = str(item.role) ?? 'user';
    // Responses "developer" is the new name for Chat Completions "system";
    // most OpenAI-compatible relays reject "developer" outright.
    const role = raw_role === 'developer' ? 'system' : raw_role;
    return { role, content: input_content_to_chat(item.content ?? null) };
  }
  // reasoning blocks, item references, computer/file calls, ...
  return null;
}

/** `function_call_output.output` is a string or an array of content parts;
 * chat upstreams need a plain string. */
function function_call_output_text(output: unknown): unknown {
  if (typeof output === 'string' || (output !== null && typeof output !== 'object')) return output;
  if (Array.isArray(output)) {
    let text = '';
    for (const part of output) {
      if (part && typeof part === 'object' && typeof (part as Record<string, unknown>).text === 'string') {
        text += (part as Record<string, unknown>).text;
      }
    }
    return text;
  }
  return '';
}

/** Collapse Responses content parts (`input_text` / `output_text` / `text`)
 * into chat text parts; image/file-only or empty arrays collapse to "". */
function input_content_to_chat(content: unknown): unknown {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    const out: Array<{ type: string; text: string }> = [];
    for (const part of content) {
      if (part && typeof part === 'object') {
        const text = (part as Record<string, unknown>).text;
        if (typeof text === 'string') out.push({ type: 'text', text });
      }
    }
    return out.length ? out : '';
  }
  return '';
}

/**
 * Translate Responses tool definitions into the Chat Completions shape:
 * Responses function tools are flat (`{type, name, description?,
 * parameters?, strict?}`); Chat Completions nests them under `function`.
 * Tool types without a Chat Completions equivalent (`web_search`,
 * `local_shell`, `mcp`, ...) are dropped rather than forwarded —
 * forwarding them gets the whole request rejected. Returns undefined when
 * nothing usable remains so we never emit an empty `tools` array.
 */
function responses_tools_to_chat(tools: unknown): unknown[] | undefined {
  if (tools === undefined) return undefined;
  const items: unknown[] = Array.isArray(tools) ? tools : [tools];
  const out: unknown[] = [];
  for (const tool of items) {
    if (!tool || typeof tool !== 'object') continue;
    const obj = tool as Record<string, unknown>;
    if (obj.function && typeof obj.function === 'object') {
      // Already in the Chat Completions shape.
      out.push(tool);
      continue;
    }
    const tool_type = typeof obj.type === 'string' ? obj.type : 'function';
    if (tool_type !== 'function') continue;
    if (typeof obj.name !== 'string') continue;
    const fn: Record<string, unknown> = { name: obj.name };
    if (obj.description !== undefined) fn.description = obj.description;
    // Codex's apply_patch tool ships without `parameters`; many upstreams
    // require one, so default to an empty object schema.
    fn.parameters = obj.parameters ?? { type: 'object', properties: {} };
    if (obj.strict !== undefined) fn.strict = obj.strict;
    out.push({ type: 'function', function: fn });
  }
  return out.length ? out : undefined;
}

/**
 * Translate the Responses `tool_choice` into the Chat Completions shape.
 * Strings pass through; the object form `{type:"function", name}` becomes
 * `{type:"function", function:{name}}`. Anything else is dropped — a
 * missing `tool_choice` is safe, an untranslatable one is a 400.
 */
function responses_tool_choice_to_chat(tool_choice: unknown): unknown {
  if (typeof tool_choice === 'string') return tool_choice;
  if (tool_choice && typeof tool_choice === 'object') {
    const obj = tool_choice as Record<string, unknown>;
    if (obj.type === 'function' && typeof obj.name === 'string') {
      return { type: 'function', function: { name: obj.name } };
    }
  }
  return undefined;
}

/* ------------------------------------------------------------------ */
/*  Response: canonical Chat -> Responses                              */
/* ------------------------------------------------------------------ */

/** Map a chat finish reason to Responses `status` + `incomplete_details`. */
function responses_finish_status(
  reason: string | null | undefined,
): { status: string; incomplete_details: unknown } {
  if (reason === 'length') return { status: 'incomplete', incomplete_details: { reason: 'max_output_tokens' } };
  if (reason === 'content_filter') return { status: 'incomplete', incomplete_details: { reason: 'content_filter' } };
  return { status: 'completed', incomplete_details: null };
}

/**
 * Build the `response` object shared by `response.created`,
 * `response.completed`, `response.failed` payloads and the non-streaming
 * body. Emits the documented Response-object fields (with explicit nulls
 * where the gateway is stateless) so strict clients can deserialize the
 * envelope without leniency.
 */
function response_envelope(params: {
  id: string;
  created_at: number;
  model: string;
  status: string;
  output: unknown[];
  usage?: Record<string, unknown>;
  instructions?: string | null;
  incomplete_details?: unknown;
  error?: { code: string; message: string } | null;
}): Record<string, unknown> {
  return {
    id: params.id,
    object: 'response',
    created_at: params.created_at,
    status: params.status,
    error: params.error ?? null,
    incomplete_details: params.incomplete_details ?? null,
    instructions: params.instructions ?? null,
    metadata: {},
    model: params.model,
    output: params.output,
    parallel_tool_calls: true,
    temperature: null,
    tool_choice: 'auto',
    tools: [],
    top_p: null,
    background: false,
    previous_response_id: null,
    reasoning: { effort: null, summary: null },
    store: false,
    usage: params.usage ?? null,
    user: null,
  };
}

/**
 * Convert a canonical chat completion into the Responses envelope (the
 * non-streaming `POST /v1/responses` body). Message text becomes a
 * `message` output item; assistant `tool_calls` become `function_call`
 * output items — Codex CLI executes tools from exactly these items.
 */
export function chat_to_responses(resp: OAIChatResponse, requested_model: string): Record<string, unknown> {
  const model = resp.model ? resp.model : requested_model;
  const output: unknown[] = [];

  for (const choice of resp.choices) {
    const message = choice.message as Record<string, unknown>;
    // DeepSeek/Kimi-style reasoning arrives as a message extension; the
    // envelope must not silently drop it.
    const reasoning =
      typeof message.reasoning_content === 'string' && message.reasoning_content
        ? message.reasoning_content
        : typeof message.reasoning === 'string' && message.reasoning
          ? message.reasoning
          : '';
    if (reasoning) {
      output.push({
        type: 'reasoning',
        id: 'rs_' + randomBytes(12).toString('hex'),
        summary: [],
        content: [{ type: 'reasoning_text', text: reasoning }],
        status: 'completed',
      });
    }
    if (typeof message.content === 'string' && message.content) {
      output.push({
        type: 'message',
        id: 'msg_' + randomBytes(12).toString('hex'),
        role: message.role,
        status: 'completed',
        content: [{ type: 'output_text', text: message.content, annotations: [] }],
      });
    }
    if (Array.isArray(message.tool_calls)) {
      for (const call of message.tool_calls) {
        const fn = (call as Record<string, unknown>).function as Record<string, unknown> | undefined;
        output.push({
          type: 'function_call',
          id: 'fc_' + randomBytes(12).toString('hex'),
          call_id: typeof (call as Record<string, unknown>).id === 'string' ? (call as Record<string, unknown>).id : '',
          name: typeof fn?.name === 'string' ? fn.name : '',
          arguments: typeof fn?.arguments === 'string' ? fn.arguments : '{}',
          status: 'completed',
        });
      }
    }
  }

  const { status, incomplete_details } = responses_finish_status(resp.choices[0]?.finish_reason);
  const usage = resp.usage
    ? {
        input_tokens: resp.usage.prompt_tokens,
        output_tokens: resp.usage.completion_tokens,
        total_tokens: resp.usage.total_tokens,
        ...(resp.usage as Record<string, unknown>).prompt_tokens_details !== undefined
          ? { input_tokens_details: (resp.usage as Record<string, unknown>).prompt_tokens_details }
          : {},
        ...(resp.usage as Record<string, unknown>).completion_tokens_details !== undefined
          ? { output_tokens_details: (resp.usage as Record<string, unknown>).completion_tokens_details }
          : {},
      }
    : undefined;

  return response_envelope({
    id: resp.id,
    created_at: resp.created,
    model,
    status,
    output,
    usage,
    incomplete_details,
  });
}

/* ------------------------------------------------------------------ */
/*  Streaming: canonical chunks -> Responses SSE                       */
/* ------------------------------------------------------------------ */

/**
 * Format one Responses SSE frame: named events (not the chat-completions
 * unnamed `data:` blobs); the JSON payload's `type` must equal the event
 * name — clients deserialize the data payload with `type` as the tag.
 */
function responses_event(name: string, payload: Record<string, unknown>): string {
  return `event: ${name}\ndata: ${JSON.stringify({ type: name, ...payload })}\n\n`;
}

/** A `response.failed` frame for a stream that failed mid-flight. */
export function responses_stream_failure_frame(model: string, code: string, message: string): string {
  const response = response_envelope({
    id: 'resp_' + randomBytes(16).toString('hex'),
    created_at: Math.floor(Date.now() / 1000),
    model,
    status: 'failed',
    output: [],
    error: { code, message },
  });
  return responses_event('response.failed', { response });
}

/** One in-flight streamed tool call: the Responses item that accumulates
 * the chat-completions `tool_calls` deltas for a single `index`. */
type stream_tool_call = {
  item_id: string;
  call_id: string;
  name: string;
  arguments: string;
  emitted_argument_bytes: number;
  output_index: number;
  added: boolean;
};

/**
 * Convert a canonical chat-completions chunk stream into the Responses SSE
 * event stream. Items open lazily on their first delta and take output
 * indices in first-seen order, so a tool-only turn carries no phantom
 * empty message item:
 * - `response.created` → (message item added on first text)
 *   `response.output_text.delta` → item/completed events.
 * - Tool-call deltas accumulate per chat `index` into `function_call`
 *   items (`output_item.added` → `function_call_arguments.delta` →
 *   `.done` → `output_item.done`).
 * - Reasoning deltas get their own `reasoning` output item.
 * - Upstream usage populates `response.completed.usage`.
 * - An error chunk becomes `response.failed` (no `[DONE]` after it).
 */
export async function* chat_chunks_to_responses_stream(
  chunks: AsyncGenerator<OAIStreamChunk>,
  requested_model: string,
): AsyncGenerator<string> {
  const id = 'resp_' + randomBytes(16).toString('hex');
  const message_id = 'msg_' + randomBytes(12).toString('hex');
  const reasoning_id = 'rs_' + randomBytes(12).toString('hex');
  const created_at = Math.floor(Date.now() / 1000);
  let first = true;

  let next_output_index = 0;
  let message_index: number | null = null;
  let accumulated_text = '';
  let message_role = 'assistant';
  let reasoning_index: number | null = null;
  const tool_calls: stream_tool_call[] = [];
  let usage: Record<string, unknown> | undefined;
  let upstream_model = '';
  let final_finish_reason: string | null | undefined;

  for await (const chunk of chunks) {
    const chunk_error = (chunk as { error?: unknown }).error;
    if (chunk_error && typeof chunk_error === 'object') {
      const err = chunk_error as { type?: unknown; message?: unknown };
      const failed = response_envelope({
        id,
        created_at,
        model: requested_model,
        status: 'failed',
        output: [],
        usage,
        error: {
          code: typeof err.type === 'string' ? err.type : 'proxy_stream_error',
          message: typeof err.message === 'string' ? err.message : 'Upstream stream failed before completion',
        },
      });
      // Terminal: no [DONE] success terminator after a failure.
      yield responses_event('response.failed', { response: failed });
      return;
    }
    if (chunk.model) upstream_model = chunk.model;
    if (first) {
      const role = chunk.choices?.[0]?.delta?.role;
      if (typeof role === 'string') message_role = role;
      yield responses_event('response.created', {
        response: response_envelope({
          id,
          created_at,
          model: requested_model,
          status: 'in_progress',
          output: [],
        }),
      });
      first = false;
    }

    const chunk_usage = (chunk as { usage?: Record<string, unknown> }).usage;
    if (chunk_usage && typeof chunk_usage === 'object') {
      const input =
        typeof chunk_usage.prompt_tokens === 'number' ? chunk_usage.prompt_tokens
          : typeof chunk_usage.input_tokens === 'number' ? chunk_usage.input_tokens
            : undefined;
      const output_tokens =
        typeof chunk_usage.completion_tokens === 'number' ? chunk_usage.completion_tokens
          : typeof chunk_usage.output_tokens === 'number' ? chunk_usage.output_tokens
            : undefined;
      if (input !== undefined || output_tokens !== undefined) {
        const in_tokens = input ?? 0;
        const out_tokens = output_tokens ?? 0;
        usage = {
          input_tokens: in_tokens,
          output_tokens: out_tokens,
          total_tokens: in_tokens + out_tokens,
          ...(chunk_usage.prompt_tokens_details !== undefined
            ? { input_tokens_details: chunk_usage.prompt_tokens_details }
            : {}),
          ...(chunk_usage.completion_tokens_details !== undefined
            ? { output_tokens_details: chunk_usage.completion_tokens_details }
            : {}),
        };
      }
    }

    for (const choice of chunk.choices ?? []) {
      if (choice.finish_reason) final_finish_reason = choice.finish_reason;
      const delta = choice.delta as { content?: string; reasoning?: unknown; tool_calls?: unknown };

      const reasoning = typeof delta.reasoning === 'string' ? delta.reasoning : '';
      if (reasoning) {
        let index: number | null = reasoning_index;
        if (index === null) {
          index = next_output_index++;
          reasoning_index = index;
          yield responses_event('response.output_item.added', {
            output_index: index,
            item: { type: 'reasoning', id: reasoning_id, summary: [], status: 'in_progress' },
          });
        }
        yield responses_event('response.reasoning_text.delta', {
          item_id: reasoning_id,
          output_index: index,
          content_index: 0,
          delta: reasoning,
        });
      }

      if (delta.content) {
        let index: number | null = message_index;
        if (index === null) {
          index = next_output_index++;
          message_index = index;
          yield responses_event('response.output_item.added', {
            output_index: index,
            item: { type: 'message', id: message_id, role: message_role, content: [], status: 'in_progress' },
          });
        }
        accumulated_text += delta.content;
        yield responses_event('response.output_text.delta', {
          item_id: message_id,
          output_index: index,
          content_index: 0,
          delta: delta.content,
        });
      }

      if (Array.isArray(delta.tool_calls)) {
        for (const tool_delta of delta.tool_calls) {
          const tc = tool_delta as Record<string, unknown>;
          const fn = (tc.function ?? {}) as Record<string, unknown>;
          const index =
            typeof tc.index === 'number' && Number.isInteger(tc.index) && tc.index >= 0
              ? tc.index
              : tool_calls.length;
          if (index >= tool_calls.length) {
            // Grow with placeholders so indices stay aligned.
            for (let i = tool_calls.length; i <= index; i++) {
              tool_calls.push({
                item_id: 'fc_' + randomBytes(12).toString('hex'),
                call_id: '',
                name: '',
                arguments: '',
                emitted_argument_bytes: 0,
                output_index: next_output_index++,
                added: false,
              });
            }
          }
          const state = tool_calls[index];
          if (typeof tc.id === 'string' && tc.id) state.call_id = tc.id;
          if (typeof fn.name === 'string' && fn.name) state.name = fn.name;
          if (!state.added && state.name && state.call_id) {
            state.added = true;
            yield responses_event('response.output_item.added', {
              output_index: state.output_index,
              item: {
                type: 'function_call',
                id: state.item_id,
                call_id: state.call_id,
                name: state.name,
                arguments: '',
                status: 'in_progress',
              },
            });
          }
          if (typeof fn.arguments === 'string') state.arguments += fn.arguments;
          // Identity can arrive after argument fragments. Open the item
          // first, then flush every previously buffered byte.
          if (state.added && state.emitted_argument_bytes < state.arguments.length) {
            const fragment = state.arguments.slice(state.emitted_argument_bytes);
            state.emitted_argument_bytes = state.arguments.length;
            yield responses_event('response.function_call_arguments.delta', {
              item_id: state.item_id,
              output_index: state.output_index,
              delta: fragment,
            });
          }
        }
      }
    }
  }

  // A payload without both identity fields cannot become an executable
  // Responses function call. Do not hide it behind a successful response.
  if (tool_calls.some((state) => !state.added && (state.name || state.call_id || state.arguments))) {
    const failed = response_envelope({
      id,
      created_at,
      model: requested_model,
      status: 'failed',
      output: [],
      usage,
      error: { code: 'proxy_stream_error', message: 'Upstream tool call ended without a name or call ID' },
    });
    yield responses_event('response.failed', { response: failed });
    return;
  }

  if (!first) {
    // Close opened items in output-index order, then assemble the final
    // output array the same way. Items that never opened are absent.
    const items: Array<{ output_index: number; item: Record<string, unknown> }> = [];
    if (message_index !== null) {
      yield responses_event('response.output_text.done', {
        item_id: message_id,
        output_index: message_index,
        content_index: 0,
        text: accumulated_text,
      });
      const message_item = {
        type: 'message',
        id: message_id,
        role: message_role,
        status: 'completed',
        content: [{ type: 'output_text', text: accumulated_text, annotations: [] }],
      };
      yield responses_event('response.output_item.done', { output_index: message_index, item: message_item });
      items.push({ output_index: message_index, item: message_item });
    }
    if (reasoning_index !== null) {
      const reasoning_item = { type: 'reasoning', id: reasoning_id, summary: [], status: 'completed' };
      yield responses_event('response.output_item.done', { output_index: reasoning_index, item: reasoning_item });
      items.push({ output_index: reasoning_index, item: reasoning_item });
    }
    for (const state of tool_calls) {
      if (!state.added) continue;
      yield responses_event('response.function_call_arguments.done', {
        item_id: state.item_id,
        output_index: state.output_index,
        arguments: state.arguments,
      });
      const tool_item = {
        type: 'function_call',
        id: state.item_id,
        call_id: state.call_id,
        name: state.name,
        arguments: state.arguments,
        status: 'completed',
      };
      yield responses_event('response.output_item.done', { output_index: state.output_index, item: tool_item });
      items.push({ output_index: state.output_index, item: tool_item });
    }
    items.sort((a, b) => a.output_index - b.output_index);

    const { status, incomplete_details } = responses_finish_status(final_finish_reason);
    yield responses_event('response.completed', {
      response: response_envelope({
        id,
        created_at,
        model: upstream_model || requested_model,
        status,
        output: items.map((entry) => entry.item),
        usage,
        incomplete_details,
      }),
    });
    yield RESPONSES_STREAM_DONE;
  }
}
