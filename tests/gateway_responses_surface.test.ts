/**
 * Node gateway /v1/responses surface (Rust proxy parity: the Responses
 * conversions in src-tauri/src/proxy/converter/responses.rs and the wire
 * envelope/event shapes in src-tauri/src/proxy/responses.rs).
 */
import { describe, expect, it } from 'vitest';
import { createGatewayApp } from '../src/gateway/server.js';
import {
  responses_request_to_chat,
  chat_to_responses,
  chat_chunks_to_responses_stream,
} from '../src/gateway/responses.js';
import type { AppConfig } from '../src/config/schema.js';
import type { OAIStreamChunk } from '../src/gateway/canonical.js';

function config(): AppConfig {
  return {
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'error' },
    providers: [
      {
        id: 'p',
        name: 'p',
        type: 'mock',
        baseURL: '',
        apiKey: '',
        enabled: true,
        models: ['upstream'],
        timeoutMs: 1000,
        weight: 1,
        circuitBreaker: { failureThreshold: 3, cooldownMs: 1000, halfOpenRetries: 1, successThreshold: 1 },
      },
    ],
    routes: [{ alias: 'alias', hops: [{ provider: 'p', model: 'upstream' }], description: '' }],
    profiles: [],
    agents: [],
  };
}

describe('responses_request_to_chat', () => {
  it('flattens instructions, input items, tools and tool_choice', () => {
    const chat = responses_request_to_chat({
      model: 'alias',
      instructions: 'be brief',
      input: [
        { type: 'message', role: 'developer', content: [{ type: 'input_text', text: 'dev rules' }] },
        { type: 'message', role: 'user', content: 'hello' },
        { type: 'function_call', call_id: 'call_1', name: 'lookup', arguments: '{"q":1}' },
        { type: 'function_call', call_id: 'call_2', name: 'store', arguments: '{"x":2}' },
        { type: 'function_call_output', call_id: 'call_1', output: [{ type: 'output_text', text: 'found' }] },
        { type: 'reasoning', content: [] },
      ],
      tools: [
        { type: 'function', name: 'lookup', parameters: { type: 'object' } },
        { type: 'web_search' },
      ],
      tool_choice: { type: 'function', name: 'lookup' },
      max_output_tokens: 128,
      temperature: 0.5,
    });
    expect(chat.model).toBe('alias');
    expect(chat.max_tokens).toBe(128);
    expect(chat.temperature).toBe(0.5);
    expect(chat.messages).toEqual([
      { role: 'system', content: 'be brief' },
      { role: 'system', content: [{ type: 'text', text: 'dev rules' }] },
      { role: 'user', content: 'hello' },
      {
        role: 'assistant',
        content: null,
        tool_calls: [
          { id: 'call_1', type: 'function', function: { name: 'lookup', arguments: '{"q":1}' } },
          { id: 'call_2', type: 'function', function: { name: 'store', arguments: '{"x":2}' } },
        ],
      },
      { role: 'tool', content: 'found', tool_call_id: 'call_1' },
    ]);
    expect(chat.tools).toEqual([
      { type: 'function', function: { name: 'lookup', parameters: { type: 'object' } } },
    ]);
    expect(chat.tool_choice).toEqual({ type: 'function', function: { name: 'lookup' } });
  });

  it('accepts a bare string input and defaults missing tool schemas', () => {
    const chat = responses_request_to_chat({
      model: 'm',
      input: 'just asking',
      tools: [{ name: 'apply_patch' }],
    });
    expect(chat.messages).toEqual([{ role: 'user', content: 'just asking' }]);
    expect(chat.tools).toEqual([
      { type: 'function', function: { name: 'apply_patch', parameters: { type: 'object', properties: {} } } },
    ]);
  });
});

describe('chat_to_responses', () => {
  it('surfaces reasoning, text and tool_calls as output items', () => {
    const envelope = chat_to_responses(
      {
        id: 'chatcmpl_1',
        object: 'chat.completion',
        created: 123,
        model: 'upstream',
        choices: [
          {
            index: 0,
            message: {
              role: 'assistant',
              content: 'answer',
              reasoning_content: 'thinking hard',
              tool_calls: [
                { id: 'c1', type: 'function', function: { name: 'lookup', arguments: '{"q":1}' } },
              ],
            },
            finish_reason: 'tool_calls',
          },
        ],
        usage: { prompt_tokens: 3, completion_tokens: 4, total_tokens: 7 },
      },
      'alias',
    );
    expect(envelope.object).toBe('response');
    expect(envelope.status).toBe('completed');
    expect((envelope.output as Array<Record<string, unknown>>).map((i) => i.type)).toEqual([
      'reasoning',
      'message',
      'function_call',
    ]);
    const tool_item = (envelope.output as Array<Record<string, unknown>>)[2];
    expect(tool_item.call_id).toBe('c1');
    expect(tool_item.arguments).toBe('{"q":1}');
    expect(envelope.usage).toMatchObject({ input_tokens: 3, output_tokens: 4, total_tokens: 7 });
  });

  it('maps a length finish to an incomplete envelope', () => {
    const envelope = chat_to_responses(
      {
        id: 'chatcmpl_2',
        object: 'chat.completion',
        created: 1,
        model: 'upstream',
        choices: [{ index: 0, message: { role: 'assistant', content: 'partial' }, finish_reason: 'length' }],
      },
      'alias',
    );
    expect(envelope.status).toBe('incomplete');
    expect(envelope.incomplete_details).toEqual({ reason: 'max_output_tokens' });
  });
});

describe('POST /v1/responses', () => {
  it('returns a completed envelope for a non-streaming request', async () => {
    const { app } = createGatewayApp(config());
    const response = await app.request('/v1/responses', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        model: 'alias',
        input: [{ type: 'message', role: 'user', content: [{ type: 'input_text', text: 'hello there' }] }],
      }),
    });
    expect(response.status).toBe(200);
    const body = (await response.json()) as Record<string, any>;
    expect(body.object).toBe('response');
    expect(body.status).toBe('completed');
    const message_item = body.output.find((item: Record<string, unknown>) => item.type === 'message');
    expect(message_item.content[0].text).toContain('You said: hello there');
    expect(body.usage.input_tokens).toBeGreaterThan(0);
  });

  it('rejects a missing model and invalid JSON', async () => {
    const { app } = createGatewayApp(config());
    const no_model = await app.request('/v1/responses', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ input: 'hi' }),
    });
    expect(no_model.status).toBe(400);
    const bad_json = await app.request('/v1/responses', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: '{nope',
    });
    expect(bad_json.status).toBe(400);
  });

  it('streams named Responses events and terminates with [DONE]', async () => {
    const { app } = createGatewayApp(config());
    const response = await app.request('/v1/responses', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ model: 'alias', stream: true, input: 'stream me' }),
    });
    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toBe('text/event-stream');
    const text = await response.text();
    for (const event_name of [
      'response.created',
      'response.output_item.added',
      'response.output_text.delta',
      'response.output_text.done',
      'response.output_item.done',
      'response.completed',
    ]) {
      expect(text).toContain(`event: ${event_name}`);
    }
    expect(text.trimEnd().endsWith('data: [DONE]')).toBe(true);
  });

  it('returns a JSON 404 for an unknown model instead of a broken stream', async () => {
    const { app } = createGatewayApp(config());
    const response = await app.request('/v1/responses', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ model: 'missing', stream: true, input: 'x' }),
    });
    expect(response.status).toBe(404);
    expect(response.headers.get('content-type')).not.toBe('text/event-stream');
  });
});

describe('chat_chunks_to_responses_stream', () => {
  function chunk(delta: Record<string, unknown>, finish_reason: string | null = null): OAIStreamChunk {
    return {
      id: 'chatcmpl_r',
      object: 'chat.completion.chunk',
      created: 1,
      model: 'upstream',
      choices: [{ index: 0, delta, finish_reason }],
    };
  }

  async function frames(chunks: OAIStreamChunk[]): Promise<string[]> {
    const out: string[] = [];
    for await (const frame of chat_chunks_to_responses_stream(
      (async function* () { yield* chunks; })(),
      'alias',
    )) {
      out.push(frame);
    }
    return out;
  }

  function event_names(stream_frames: string[]): string[] {
    return stream_frames
      .filter((frame) => frame.startsWith('event: '))
      .map((frame) => frame.split('\n')[0].replace(/^event: /, ''));
  }

  it('emits function_call items with argument deltas for streamed tool calls', async () => {
    const stream_frames = await frames([
      chunk({ role: 'assistant' }),
      chunk({ tool_calls: [{ index: 0, id: 'c1', function: { name: 'lookup' } }] }),
      chunk({ tool_calls: [{ index: 0, function: { arguments: '{"q":' } }] }),
      chunk({ tool_calls: [{ index: 0, function: { arguments: '1}' } }] }),
      chunk({}, 'tool_calls'),
    ]);
    expect(event_names(stream_frames)).toEqual([
      'response.created',
      'response.output_item.added',
      'response.function_call_arguments.delta',
      'response.function_call_arguments.delta',
      'response.function_call_arguments.done',
      'response.output_item.done',
      'response.completed',
    ]);
    const added = JSON.parse(stream_frames[1].split('\ndata: ')[1]) as { item: Record<string, unknown> };
    expect(added.item).toMatchObject({ type: 'function_call', call_id: 'c1', name: 'lookup' });
    const completed = JSON.parse(
      stream_frames[stream_frames.length - 2].split('\ndata: ')[1],
    ) as { response: { output: Array<Record<string, unknown>> } };
    expect(completed.response.output).toHaveLength(1);
    expect(completed.response.output[0]).toMatchObject({ type: 'function_call', arguments: '{"q":1}' });
    expect(stream_frames[stream_frames.length - 1]).toBe('data: [DONE]\n\n');
  });

  it('buffers argument fragments that arrive before call identity', async () => {
    const stream_frames = await frames([
      chunk({ tool_calls: [{ index: 0, function: { arguments: '{"q"' } }] }),
      chunk({ tool_calls: [{ index: 0, id: 'late', function: { name: 'lookup', arguments: ':1}' } }] }),
      chunk({}, 'tool_calls'),
    ]);
    const deltas = stream_frames
      .filter((f) => f.startsWith('event: response.function_call_arguments.delta'))
      .map((f) => (JSON.parse(f.split('\ndata: ')[1]) as { delta: string }).delta);
    expect(deltas).toEqual(['{"q":1}']);
  });

  it('converts an error chunk into a terminal response.failed without [DONE]', async () => {
    const error_chunk = { ...chunk({}), error: { type: 'proxy_stream_error', message: 'boom' } } as OAIStreamChunk;
    const stream_frames = await frames([chunk({ content: 'partial' }), error_chunk]);
    expect(event_names(stream_frames)).toEqual([
      'response.created',
      'response.output_item.added',
      'response.output_text.delta',
      'response.failed',
    ]);
    expect(stream_frames.some((f) => f.includes('[DONE]'))).toBe(false);
  });
});
