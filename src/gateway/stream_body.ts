/** Demand-driven SSE pacing. Framing remains specific to each HTTP surface. */
export interface Sse_body_frames {
  chunk: (chunk: unknown) => string;
  failure: (error: unknown) => string[];
  completion?: () => string[];
}

export function stream_abort(source: AbortSignal): {
  signal: AbortSignal;
  abort: () => void;
  dispose: () => void;
} {
  const controller = new AbortController();
  const dispose = () => source.removeEventListener('abort', abort);
  const abort = () => {
    controller.abort();
    dispose();
  };
  if (source.aborted) abort();
  else source.addEventListener('abort', abort, { once: true });
  return { signal: controller.signal, abort, dispose };
}

export async function* replay_first_chunk<T>(
  first: IteratorResult<T>,
  rest: AsyncGenerator<T>,
): AsyncGenerator<T> {
  let handed_off = false;
  try {
    if (!first.done) yield first.value;
    handed_off = true;
    yield* rest;
  } finally {
    // Cancellation at the replayed yield never enters `yield* rest`.
    if (!handed_off) await rest.return(undefined);
  }
}

export function sse_body_stream(
  iter: AsyncGenerator<unknown>,
  frames: Sse_body_frames,
  cancel_source?: () => void,
): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  let finished = false;
  const release = () => {
    // A pending next() queues return() behind it; abort the I/O separately.
    cancel_source?.();
    void iter.return(undefined).catch(() => {});
  };
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      if (finished) return;
      try {
        const next = await iter.next();
        if (finished) return;
        if (next.done) {
          finished = true;
          for (const frame of frames.completion?.() ?? []) controller.enqueue(encoder.encode(frame));
          controller.close();
        } else {
          controller.enqueue(encoder.encode(frames.chunk(next.value)));
        }
      } catch (error) {
        if (finished) return;
        finished = true;
        release();
        for (const frame of frames.failure(error)) controller.enqueue(encoder.encode(frame));
        controller.close();
      }
    },
    cancel() {
      if (finished) return;
      finished = true;
      release();
    },
  });
}
