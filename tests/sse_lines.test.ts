import { describe, expect, it } from 'vitest';
import { max_sse_line_bytes, read_sse_lines } from '../src/gateway/sse_lines.js';

const text_encoder = new TextEncoder();

function stream_from_chunks(chunks: Uint8Array[]): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(chunk);
      controller.close();
    },
  });
}

async function collect_lines(
  body: ReadableStream<Uint8Array>,
  max_line_bytes?: number,
): Promise<string[]> {
  const lines: string[] = [];
  for await (const line of read_sse_lines(body, max_line_bytes)) lines.push(line);
  return lines;
}

describe('SSE line reader', () => {
  it('caps_a_line_at_the_rust_event_limit', async () => {
    // Rust parity: src-tauri/src/proxy/providers/sse.rs `MAX_EVENT_BYTES`.
    expect(max_sse_line_bytes).toBe(8 * 1024 * 1024);
  });

  it('decodes_a_multibyte_character_split_across_chunks', async () => {
    const encoded = text_encoder.encode('data: café\n');
    const leading_byte_index = encoded.indexOf(0xc3);
    const body = stream_from_chunks([
      encoded.slice(0, leading_byte_index + 1),
      encoded.slice(leading_byte_index + 1),
    ]);

    expect(await collect_lines(body)).toEqual(['data: café']);
    expect(body.locked).toBe(false);
  });

  it('handles_crlf_and_a_final_line_without_a_newline', async () => {
    const body = stream_from_chunks([text_encoder.encode('first\r\nsecond\nlast\r')]);

    expect(await collect_lines(body)).toEqual(['first', 'second', 'last']);
    expect(body.locked).toBe(false);
  });

  it('returns_multiple_lines_from_one_chunk', async () => {
    const body = stream_from_chunks([text_encoder.encode('one\ntwo\nthree\n')]);

    expect(await collect_lines(body)).toEqual(['one', 'two', 'three']);
    expect(body.locked).toBe(false);
  });

  it('accepts_a_line_at_the_exact_byte_limit', async () => {
    const body = stream_from_chunks([text_encoder.encode('éé\n')]);

    expect(await collect_lines(body, 4)).toEqual(['éé']);
    expect(body.locked).toBe(false);
  });

  it('rejects_over_limit_bytes_and_cancels_releases_the_reader_on_error', async () => {
    let cancel_count = 0;
    let cancel_reason: unknown;
    const body = new ReadableStream<Uint8Array>({
      pull(controller) {
        controller.enqueue(text_encoder.encode('éé'));
      },
      cancel(reason) {
        cancel_count += 1;
        cancel_reason = reason;
      },
    }, { highWaterMark: 0 });

    await expect(collect_lines(body, 3)).rejects.toMatchObject({
      code: 'upstream_decode_error',
      status: 502,
      stage: 'stream_line_limit',
    });

    expect(cancel_count).toBe(1);
    expect(cancel_reason).toBeUndefined();
    expect(body.locked).toBe(false);
  });

  it('cancels_and_releases_the_reader_when_the_generator_returns', async () => {
    let cancel_count = 0;
    let cancel_reason: unknown;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(text_encoder.encode('one\ntwo\n'));
      },
      cancel(reason) {
        cancel_count += 1;
        cancel_reason = reason;
      },
    });
    const generator = read_sse_lines(body);

    expect(await generator.next()).toEqual({ done: false, value: 'one' });
    await generator.return(undefined);

    expect(cancel_count).toBe(1);
    expect(cancel_reason).toBeUndefined();
    expect(body.locked).toBe(false);
  });
});
