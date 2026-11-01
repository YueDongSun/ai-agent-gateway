/**
 * OpenAI <-> Anthropic tool conversion (Rust proxy parity: the tool shape
 * converters and message mappings in src-tauri/src/proxy/converter/
 * anthropic.rs — `oai_tool_to_anthropic_tool`,
 * `anthropic_tool_to_oai_tool`, tool role / tool_result and tool_calls /
 * tool_use block mapping).
 */
import { describe, expect, it } from 'vitest';
import {
  oai_tool_to_anthropic_tool,
  anthropic_tool_to_oai_tool,
  oaiReqToAnthropicReq,
  anthropicReqToOAIReq,
  anthropicRespToOAIResp,
  oaiRespToAnthropicResp,
  oaiChunksToAnthropicStream,
} from '../src/gateway/converter.js';
import type { ChatCompletionRequest } from '../src/config/schema.js';
import type { OAIStreamChunk } from '../src/gateway/canonical.js';

const oai_tool = {
  type: 'function',
  function: {
    name: 'lookup',
    description: 'look things up',
    parameters: { type: 'object', properties: { q: { type: 'string' } } },
  },
};

describe('tool shape conversion', () => {
  it('round-trips the OpenAI function wrapper through the Anthropic shape', () => {
    const anthropic = oai_tool_to_anthropic_tool(oai_tool) as Record<string, unknown>;
    expect(anthropic).toEqual({
      name: 'lookup',
      description: 'look things up',
      input_schema: { type: 'object', properties: { q: { type: 'string' } } },
    });
    expect(anthropic_tool_to_oai_tool(anthropic)).toEqual(oai_tool);
  });

  it('defaults a missing schema and keeps native definitions intact', () => {
    const minimal = oai_tool_to_anthropic_tool({
      type: 'function',
      function: { name: 'no_schema' },
    }) as Record<string, unknown>;
    expect(minimal['input_schema']).toEqual({ type: 'object', properties: {} });
    const custom = { type: 'custom', foo: 1 };
    expect(oai_tool_to_anthropic_tool(custom)).toBe(custom);
    expect(anthropic_tool_to_oai_tool(custom)).toBe(custom);
  });
});

describe('oaiReqToAnthropicReq tool mapping', () => {
  const req: ChatCompletionRequest = {
    model: 'm',
    messages: [
      { role: 'user', content: 'run the tool' },
      {
        role: 'assistant',
        content: '',
        tool_calls: [
          {
            id: 'call_1',
            type: 'function',
            function: { name: 'lookup', arguments: '{"q":"a"}' },
          },
        ],
      },
      { role: 'tool', tool_call_id: 'call_1', content: 'result one' },
      { role: 'tool', tool_call_id: 'call_1', content: 'result two' },
    ],
    tools: [oai_tool],
  };

  it('maps tool_calls to tool_use blocks and merges tool results', () => {
    const out = oaiReqToAnthropicReq(req);
    expect(out.messages).toHaveLength(3);
    const assistant = out.messages[1];
    expect(assistant.role).toBe('assistant');
    const blocks = assistant.content as Array<Record<string, unknown>>;
    expect(blocks.some((b) => b['type'] === 'tool_use' && b['input'] === null)).toBe(false);
    const tool_use = blocks.find((b) => b['type'] === 'tool_use') as Record<string, unknown>;
    expect(tool_use['id']).toBe('call_1');
    expect(tool_use['name']).toBe('lookup');
    expect(tool_use['input']).toEqual({ q: 'a' });
    // Both tool messages merge into ONE user message with two tool_result
    // blocks — Anthropic rejects consecutive tool_result messages.
    const merged = out.messages[2];
    expect(merged.role).toBe('user');
    const results = merged.content as Array<Record<string, unknown>>;
    expect(results).toHaveLength(2);
    expect(results.every((b) => b['type'] === 'tool_result')).toBe(true);
    expect(results[0]['tool_use_id']).toBe('call_1');
    expect(results[1]['content']).toBe('result two');
  });

  it('converts the tools array instead of forwarding the OpenAI shape', () => {
    const out = oaiReqToAnthropicReq(req);
    expect(out['tools']).toEqual([
      {
        name: 'lookup',
        description: 'look things up',
        input_schema: { type: 'object', properties: { q: { type: 'string' } } },
      },
    ]);
  });
});

describe('anthropicReqToOAIReq tool mapping', () => {
  it('splits tool_use and tool_result blocks into the OpenAI shapes', () => {
    const out = anthropicReqToOAIReq({
      model: 'm',
      max_tokens: 128,
      messages: [
        {
          role: 'assistant',
          content: [
            { type: 'text', text: 'checking' },
            { type: 'tool_use', id: 'tu_1', name: 'lookup', input: { q: 'a' } },
          ],
        },
        {
          role: 'user',
          content: [{ type: 'tool_result', tool_use_id: 'tu_1', content: 'found it' }],
        },
      ],
      tools: [{ name: 'lookup', input_schema: { type: 'object' } }],
    });
    const assistant = out.messages[0] as Record<string, unknown>;
    expect(assistant['content']).toBe('checking');
    expect(assistant['tool_calls']).toEqual([
      {
        id: 'tu_1',
        type: 'function',
        function: { name: 'lookup', arguments: '{"q":"a"}' },
      },
    ]);
    const tool_message = out.messages[1] as Record<string, unknown>;
    expect(tool_message['role']).toBe('tool');
    expect(tool_message['tool_call_id']).toBe('tu_1');
    expect(tool_message['content']).toBe('found it');
    expect(out.tools).toEqual([
      { type: 'function', function: { name: 'lookup', parameters: { type: 'object' } } },
    ]);
  });
});

describe('tool conversion round trip', () => {
  it('survives OAI -> Anthropic -> OAI with tools and results', () => {
    const req: ChatCompletionRequest = {
      model: 'm',
      messages: [
        { role: 'user', content: 'go' },
        {
          role: 'assistant',
          content: '',
          tool_calls: [
            { id: 'c1', type: 'function', function: { name: 'lookup', arguments: '{"q":1}' } },
          ],
        },
        { role: 'tool', tool_call_id: 'c1', content: 'done' },
      ],
      tools: [oai_tool],
    };
    const back = anthropicReqToOAIReq(oaiReqToAnthropicReq(req));
    expect(back.messages[1]).toMatchObject({
      role: 'assistant',
      tool_calls: [
        { id: 'c1', type: 'function', function: { name: 'lookup', arguments: '{"q":1}' } },
      ],
    });
    expect(back.messages[2]).toMatchObject({ role: 'tool', tool_call_id: 'c1', content: 'done' });
    expect(back.tools).toEqual([oai_tool]);
  });
});

describe('response tool mapping', () => {
  it('anthropicRespToOAIResp turns tool_use into tool_calls', () => {
    const out = anthropicRespToOAIResp(
      {
        id: 'msg_1',
        type: 'message',
        role: 'assistant',
        model: 'm',
        content: [
          { type: 'text', text: 'using tool' },
          { type: 'tool_use', id: 'tu_9', name: 'lookup', input: { q: 'x' } },
        ],
        stop_reason: 'tool_use',
        stop_sequence: null,
        usage: { input_tokens: 1, output_tokens: 2 },
      },
      'm',
    );
    const message = out.choices[0].message as Record<string, unknown>;
    expect(message['content']).toBe('using tool');
    expect(message['tool_calls']).toEqual([
      { id: 'tu_9', type: 'function', function: { name: 'lookup', arguments: '{"q":"x"}' } },
    ]);
    expect(out.choices[0].finish_reason).toBe('tool_calls');
  });

  it('oaiRespToAnthropicResp turns tool_calls into tool_use blocks', () => {
    const out = oaiRespToAnthropicResp(
      {
        id: 'chatcmpl_1',
        object: 'chat.completion',
        created: 1,
        model: 'm',
        choices: [
          {
            index: 0,
            message: {
              role: 'assistant',
              content: 'answer',
              tool_calls: [
                { id: 'c2', type: 'function', function: { name: 'lookup', arguments: '{"q":2}' } },
              ],
            },
            finish_reason: 'tool_calls',
          },
        ],
        usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
      },
      'm',
    );
    expect(out.content).toEqual([
      { type: 'text', text: 'answer' },
      { type: 'tool_use', id: 'c2', name: 'lookup', input: { q: 2 } },
    ]);
    expect(out.stop_reason).toBe('tool_use');
  });
});

/* ---- streaming tool bridge (Rust parity: anthropic_stream.rs) ------- */

function stream_chunk(delta: Record<string, unknown>, finish_reason: string | null = null): OAIStreamChunk {
  return {
    id: 'chatcmpl_t',
    object: 'chat.completion.chunk',
    created: 1,
    model: 'm',
    choices: [{ index: 0, delta, finish_reason }],
  };
}

async function collect_stream(chunks: OAIStreamChunk[]): Promise<Array<Record<string, any>>> {
  const frames: string[] = [];
  for await (const frame of oaiChunksToAnthropicStream(
    (async function* () { yield* chunks; })(),
    'm',
  )) {
    frames.push(frame);
  }
  return frames.map((frame) => JSON.parse(frame.split('\n').find((l) => l.startsWith('data: '))!.slice(6)));
}

describe('streaming tool bridge', () => {
  it('bridges streamed tool_calls into tool_use blocks with input_json_delta', async () => {
    const events = await collect_stream([
      stream_chunk({ role: 'assistant', content: 'searching' }),
      stream_chunk({ tool_calls: [{ index: 0, id: 'c1', type: 'function', function: { name: 'lookup' } }] }),
      stream_chunk({ tool_calls: [{ index: 0, function: { arguments: '{"q":"a"}' } }] }),
      stream_chunk({ tool_calls: [{ index: 1, id: 'c2', type: 'function', function: { name: 'store', arguments: '{"x":1}' } }] }),
      stream_chunk({}, 'tool_calls'),
    ]);
    expect(events.map((e) => e.type)).toEqual([
      'message_start',
      'content_block_start',
      'content_block_delta',
      'content_block_stop',
      'content_block_start',
      'content_block_delta',
      'content_block_start',
      'content_block_delta',
      'content_block_stop',
      'content_block_stop',
      'message_delta',
      'message_stop',
    ]);
    expect(events[1]).toMatchObject({ index: 0, content_block: { type: 'text' } });
    expect(events[4]).toMatchObject({
      index: 1,
      content_block: { type: 'tool_use', id: 'c1', name: 'lookup', input: {} },
    });
    expect(events[5]).toMatchObject({ index: 1, delta: { type: 'input_json_delta', partial_json: '{"q":"a"}' } });
    expect(events[6]).toMatchObject({
      index: 2,
      content_block: { type: 'tool_use', id: 'c2', name: 'store', input: {} },
    });
    expect(events[7]).toMatchObject({ index: 2, delta: { type: 'input_json_delta', partial_json: '{"x":1}' } });
    expect(events[10]).toMatchObject({ delta: { stop_reason: 'tool_use' } });
  });

  it('buffers argument fragments that arrive before id/name (string index)', async () => {
    const events = await collect_stream([
      stream_chunk({ tool_calls: [{ index: '0', function: { arguments: '{"q"' } }] }),
      stream_chunk({ tool_calls: [{ index: '0', id: 'late_1', function: { name: 'lookup', arguments: ':1}' } }] }),
      stream_chunk({}, 'tool_calls'),
    ]);
    const starts = events.filter((e) => e.type === 'content_block_start');
    expect(starts).toHaveLength(1);
    expect(starts[0].content_block).toEqual({ type: 'tool_use', id: 'late_1', name: 'lookup', input: {} });
    const json_deltas = events.filter((e) => e.type === 'content_block_delta');
    expect(json_deltas.map((e) => e.delta.partial_json)).toEqual(['{"q"', ':1}']);
  });

  it('late-starts a tool block that never got id or name', async () => {
    const events = await collect_stream([
      stream_chunk({ tool_calls: [{ index: 0, function: { arguments: '{"a":1}' } }] }),
      stream_chunk({}, 'tool_calls'),
    ]);
    const start = events.find((e) => e.type === 'content_block_start');
    expect(start.content_block).toEqual({ type: 'tool_use', id: 'tool_call_0', name: 'unknown_tool' });
    expect(events.some((e) => e.type === 'content_block_stop' && e.index === 0)).toBe(true);
  });

  it('defers message_delta so a trailing usage chunk fills the final numbers', async () => {
    const usage_chunk = {
      ...stream_chunk({}),
      choices: [],
      usage: { prompt_tokens: 10, completion_tokens: 5, prompt_tokens_details: { cached_tokens: 4 } },
    } as OAIStreamChunk;
    const events = await collect_stream([
      stream_chunk({ role: 'assistant', content: 'hi' }),
      stream_chunk({}, 'stop'),
      usage_chunk,
    ]);
    const delta_events = events.filter((e) => e.type === 'message_delta');
    expect(delta_events).toHaveLength(1);
    expect(delta_events[0].usage).toEqual({
      input_tokens: 6,
      output_tokens: 5,
      cache_read_input_tokens: 4,
    });
    // message_delta must land after the usage chunk, right before message_stop.
    expect(events[events.length - 2].type).toBe('message_delta');
    expect(events[events.length - 1].type).toBe('message_stop');
  });

  it('routes reasoning deltas into thinking blocks before the text block', async () => {
    const events = await collect_stream([
      stream_chunk({ role: 'assistant', reasoning: 'pondering' }),
      stream_chunk({ content: 'answer' }),
      stream_chunk({}, 'stop'),
    ]);
    expect(events.filter((e) => e.type === 'content_block_start').map((e) => e.content_block.type)).toEqual([
      'thinking',
      'text',
    ]);
    expect(events.find((e) => e.type === 'content_block_delta')!.delta).toEqual({
      type: 'thinking_delta',
      thinking: 'pondering',
    });
    expect(events[events.length - 1].type).toBe('message_stop');
  });

  it('an empty stream still produces a complete empty message', async () => {
    const events = await collect_stream([]);
    expect(events.map((e) => e.type)).toEqual([
      'message_start',
      'content_block_start',
      'content_block_stop',
      'message_delta',
      'message_stop',
    ]);
    expect(events[3].delta).toEqual({ stop_reason: 'end_turn', stop_sequence: null });
  });
});
