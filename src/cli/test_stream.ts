import { read_sse_events } from '../gateway/sse_lines.js';

export class Test_stream_error extends Error {}

/** OpenAI chat completion SSE used by `aag test`. Output is retained on failure. */
export async function consume_test_stream(
  body: ReadableStream<Uint8Array>,
  write: (text: string) => void,
): Promise<void> {
  let terminal_choice = false;
  let incomplete_choice = false;
  for await (const { event, data } of read_sse_events(body)) {
    if (event === 'error') throw new Test_stream_error('stream failed: gateway returned an error event');
    if (data === '[DONE]') {
      if (incomplete_choice) throw new Test_stream_error('stream incomplete: generation stopped at a limit or content filter');
      if (!terminal_choice) throw new Test_stream_error('stream incomplete: no successful terminal completion choice received');
      return;
    }
    let value: unknown;
    try { value = JSON.parse(data); }
    catch { throw new Test_stream_error('stream failed: malformed completion event'); }
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Test_stream_error('stream failed: invalid completion event');
    const chunk = value as Record<string, unknown>;
    if (chunk.error !== undefined) throw new Test_stream_error('stream failed: gateway returned an error');
    if (!Array.isArray(chunk.choices)) throw new Test_stream_error('stream failed: invalid completion choices');
    // Empty choices are allowed for the final usage-only chunk.
    if (!chunk.choices.length && (!chunk.usage || typeof chunk.usage !== 'object' || Array.isArray(chunk.usage))) throw new Test_stream_error('stream failed: empty completion event');
    for (const choice of chunk.choices) {
      if (!choice || typeof choice !== 'object' || Array.isArray(choice) || !choice.delta || typeof choice.delta !== 'object' || Array.isArray(choice.delta)) throw new Test_stream_error('stream failed: invalid completion delta');
      const content: unknown = choice.delta.content;
      if (content !== undefined && content !== null && typeof content !== 'string') throw new Test_stream_error('stream failed: invalid completion text');
      const finish_reason: unknown = choice.finish_reason;
      if (finish_reason !== undefined && finish_reason !== null) {
        if (finish_reason === 'length' || finish_reason === 'content_filter') incomplete_choice = true;
        else if (finish_reason === 'stop' || finish_reason === 'tool_calls' || finish_reason === 'function_call') terminal_choice = true;
        else throw new Test_stream_error('stream failed: invalid completion finish reason');
      }
      if (typeof content === 'string') write(content);
    }
  }
  throw new Test_stream_error('stream incomplete: connection ended before the completion marker');
}
