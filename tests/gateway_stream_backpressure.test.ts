/**
 * Streaming backpressure on the gateway's SSE bodies.
 *
 * The SSE bodies used to drain their upstream generator inside
 * ReadableStream's `start()` with an unbounded `for await ... enqueue`
 * loop. That loop never consulted the consumer, so a slow or stalled client
 * still pulled every chunk the upstream produced and queued it in memory,
 * and a disconnect left the loop running against an upstream nobody was
 * reading. `sse_body_stream` pulls one chunk per `pull()` instead.
 *
 * These tests pin the new contract. Two of them are falsifying anchors:
 * `pulls_exhausted_while_the_reader_stalls` fails against the old eager
 * drain (which pulls every chunk regardless of the reader), and
 * `cancel_propagates_through_every_generator_in_the_chain` fails against a
 * `replay_first_chunk` that does not hand `rest` back on a disconnect. The
 * rest pin the wire framing and the cancellation contract.
 */
import { describe, it, expect, vi } from 'vitest';
import {
  replay_first_chunk,
  sse_body_stream,
  stream_abort,
  type Sse_body_frames,
} from '../src/gateway/stream_body.js';

const CHUNK_COUNT = 40;

/** OpenAI-shaped framing (a `[DONE]` terminator after data and errors). */
const openai_frames: Sse_body_frames = {
  chunk: (chunk) => `data: ${JSON.stringify(chunk)}\n\n`,
  failure: () => ['data: {"error":"upstream_failed"}\n\n', 'data: [DONE]\n\n'],
  completion: () => ['data: [DONE]\n\n'],
};

/** Anthropic-shaped framing (named events, no terminator). */
const anthropic_frames: Sse_body_frames = {
  chunk: (chunk) => `event: message\ndata: ${JSON.stringify(chunk)}\n\n`,
  failure: () => ['event: error\ndata: {"error":"upstream_failed"}\n\n'],
};

async function cancel_and_release(reader: ReadableStreamDefaultReader<Uint8Array>): Promise<void> {
  try {
    await reader.cancel();
  } finally {
    reader.releaseLock();
  }
}

async function drain(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let text = '';
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      text += decoder.decode(value);
    }
    return text;
  } finally {
    await cancel_and_release(reader);
  }
}

/** Let queued microtasks and timers run, so an eager drain completes. */
function settle(): Promise<void> {
  return new Promise((done) => setTimeout(done, 20));
}

describe('gateway SSE body backpressure', () => {
  it('pulls_exhausted_while_the_reader_stalls', async () => {
    let pulls = 0;
    async function* source(): AsyncGenerator<unknown> {
      for (let index = 0; index < CHUNK_COUNT; index += 1) {
        pulls += 1;
        yield { index };
      }
    }
    const reader = sse_body_stream(source(), openai_frames).getReader();
    try {
      await reader.read();
      // Give an unbounded drain every chance to run to completion.
      await settle();

      expect(pulls).toBeGreaterThanOrEqual(1);
      expect(pulls).toBeLessThanOrEqual(2);
    } finally {
      await cancel_and_release(reader);
    }
  });

  it('cancelling_stops_pulling', async () => {
    let pulls = 0;
    let released = false;
    let notify_released: () => void = () => {};
    const source_released = new Promise<void>((resolve) => { notify_released = resolve; });
    async function* source(): AsyncGenerator<unknown> {
      try {
        for (let index = 0; index < CHUNK_COUNT; index += 1) {
          pulls += 1;
          yield { index };
        }
      } finally {
        released = true;
        notify_released();
      }
    }
    const reader = sse_body_stream(source(), openai_frames).getReader();
    try {
      await reader.read();
      await reader.cancel('client disconnected');
      await source_released;

      expect(released).toBe(true);
      expect(pulls).toBeLessThan(CHUNK_COUNT);
    } finally {
      await cancel_and_release(reader);
    }
  });

  it('cancel_propagates_through_every_generator_in_the_chain', async () => {
    // The real chain is sse_body_stream -> replay_first_chunk -> [converter]
    // -> Router.stream -> provider generator. `return()` must unwind all of
    // it, or an abandoned stream leaves the provider's body read running.
    // The hard case is a disconnect that lands while the body is suspended
    // on the replayed first chunk, before `yield* rest` is ever entered.
    let upstream_released = false;
    let notify_upstream_released: () => void = () => {};
    const source_released = new Promise<void>((resolve) => { notify_upstream_released = resolve; });
    async function* upstream(): AsyncGenerator<unknown> {
      try {
        for (let index = 0; index < CHUNK_COUNT; index += 1) yield { index };
      } finally {
        upstream_released = true;
        notify_upstream_released();
      }
    }
    const source = upstream();
    const first = await source.next();
    const reader = sse_body_stream(replay_first_chunk(first, source), openai_frames).getReader();
    try {
      await reader.read();
      await reader.cancel('client disconnected');
      await source_released;

      expect(upstream_released).toBe(true);
    } finally {
      await cancel_and_release(reader);
    }
  });

  it('emits_the_completion_terminator_after_a_clean_finish', async () => {
    async function* source(): AsyncGenerator<unknown> {
      yield { index: 0 };
      yield { index: 1, finish_reason: 'stop' };
    }
    const text = await drain(sse_body_stream(source(), openai_frames));

    expect(text).toBe(
      'data: {"index":0}\n\ndata: {"index":1,"finish_reason":"stop"}\n\ndata: [DONE]\n\n',
    );
  });

  it('omits_the_terminator_on_surfaces_that_have_none', async () => {
    async function* source(): AsyncGenerator<unknown> {
      yield { type: 'message_stop' };
    }
    const text = await drain(sse_body_stream(source(), anthropic_frames));

    expect(text).toBe('event: message\ndata: {"type":"message_stop"}\n\n');
    expect(text).not.toContain('[DONE]');
  });

  it('emits_failure_frames_and_closes_when_the_upstream_fails', async () => {
    async function* source(): AsyncGenerator<unknown> {
      yield { index: 0 };
      throw new Error('upstream exploded');
    }
    const text = await drain(sse_body_stream(source(), openai_frames));

    expect(text).toContain('data: {"error":"upstream_failed"}');
    expect(text.endsWith('data: [DONE]\n\n')).toBe(true);
  });

  it('keeps_a_client_that_never_reads_from_blocking_the_upstream_forever', async () => {
    // The complement of backpressure: a stalled reader must not be able to
    // make the source generator spin. One read resolves immediately, which
    // is what proves the stream is not pre-filled.
    let pulls = 0;
    async function* source(): AsyncGenerator<unknown> {
      for (let index = 0; index < CHUNK_COUNT; index += 1) {
        pulls += 1;
        await new Promise((done) => setTimeout(done, 1));
        yield { index };
      }
    }
    const reader = sse_body_stream(source(), openai_frames).getReader();
    try {
      await reader.read();

      expect(pulls).toBeLessThanOrEqual(2);
    } finally {
      await cancel_and_release(reader);
    }
  });
});

describe('gateway request abort bridge', () => {
  it('aborts_the_local_signal_when_the_source_is_already_aborted', () => {
    const source_controller = new AbortController();
    source_controller.abort();
    const add_listener = vi.spyOn(source_controller.signal, 'addEventListener');
    const abort_bridge = stream_abort(source_controller.signal);

    expect(abort_bridge.signal.aborted).toBe(true);
    expect(add_listener).not.toHaveBeenCalled();
  });

  it('forwards_source_abort_and_detaches_its_listener', () => {
    const source_controller = new AbortController();
    const add_listener = vi.spyOn(source_controller.signal, 'addEventListener');
    const remove_listener = vi.spyOn(source_controller.signal, 'removeEventListener');
    const abort_bridge = stream_abort(source_controller.signal);

    expect(abort_bridge.signal.aborted).toBe(false);
    source_controller.abort();

    expect(abort_bridge.signal.aborted).toBe(true);
    expect(add_listener).toHaveBeenCalledOnce();
    expect(remove_listener).toHaveBeenCalledOnce();
    expect(remove_listener).toHaveBeenCalledWith('abort', add_listener.mock.calls[0]?.[1]);
  });

  it('dispose_detaches_the_listener_without_aborting_the_local_signal', () => {
    const source_controller = new AbortController();
    const add_listener = vi.spyOn(source_controller.signal, 'addEventListener');
    const remove_listener = vi.spyOn(source_controller.signal, 'removeEventListener');
    const abort_bridge = stream_abort(source_controller.signal);

    abort_bridge.dispose();
    source_controller.abort();

    expect(abort_bridge.signal.aborted).toBe(false);
    expect(add_listener).toHaveBeenCalledOnce();
    expect(remove_listener).toHaveBeenCalledOnce();
    expect(remove_listener).toHaveBeenCalledWith('abort', add_listener.mock.calls[0]?.[1]);
  });
});
