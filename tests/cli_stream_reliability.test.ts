import { createServer, type ServerResponse } from 'node:http';
import { once } from 'node:events';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { AddressInfo } from 'node:net';
import { testCmd } from '../src/cli/commands.js';
import { consume_test_stream } from '../src/cli/test_stream.js';

const state = vi.hoisted(() => ({ port: 0 }));
vi.mock('../src/config/store.js', () => ({
  loadConfig: () => ({ gateway: { host: '127.0.0.1', port: state.port, authToken: '' }, routes: [{ alias: 'fixture' }] }),
}));

const content = (text: string) => `data: ${JSON.stringify({ choices: [{ delta: { content: text }, finish_reason: null }] })}\n\n`;
const terminal = 'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n';
let cleanup: (() => Promise<void>) | undefined;
afterEach(async () => { await cleanup?.(); cleanup = undefined; vi.restoreAllMocks(); process.exitCode = 0; });

async function run_fixture(write: (response: ServerResponse) => void, options: { signal?: AbortSignal; stream?: boolean } = {}) {
  const server = createServer((_request, response) => write(response));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  state.port = (server.address() as AddressInfo).port;
  cleanup = () => new Promise<void>((resolve) => { server.closeAllConnections(); server.close(() => resolve()); });
  const output: string[] = [];
  const errors: string[] = [];
  vi.spyOn(process.stdout, 'write').mockImplementation(((value: unknown) => { output.push(String(value)); return true; }) as typeof process.stdout.write);
  vi.spyOn(console, 'log').mockImplementation((...values) => { output.push(values.join(' ')); });
  vi.spyOn(console, 'error').mockImplementation((...values) => { errors.push(values.join(' ')); });
  process.exitCode = 0;
  await testCmd('fixture', options);
  return { output: output.join(''), errors: errors.join(''), exit_code: process.exitCode };
}

describe('CLI test stream outcomes through local HTTP', () => {
  it('decodes byte-by-byte chunks independently of HTTP coalescing', async () => {
    const bytes = new TextEncoder().encode(content('你好，世界') + terminal);
    const stream = new ReadableStream<Uint8Array>({ start(controller) { for (const byte of bytes) controller.enqueue(Uint8Array.of(byte)); controller.close(); } });
    let output = '';
    await consume_test_stream(stream, (text) => { output += text; });
    expect(output).toBe('你好，世界');
  });
  it('preserves arbitrary event and Chinese UTF-8 chunk boundaries', async () => {
    const result = await run_fixture((response) => {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      const bytes = Buffer.from(content('你好，世界') + terminal);
      for (const byte of bytes) response.write(Buffer.from([byte]));
      response.end();
    });
    expect(result.output).toContain('你好，世界');
    expect(result.output).toContain('stream complete');
    expect(result.exit_code).toBe(0);
  });

  it.each([
    ['first error', 'event: error\ndata: {"error":{"message":"fixture"}}\n\ndata: [DONE]\n\n'],
    ['midstream error', content('useful partial') + 'data: {"error":{"code":"failed"}}\n\ndata: [DONE]\n\n'],
    ['truncated EOF', content('useful partial')],
    ['malformed event', 'data: {broken}\n\n' + terminal],
    ['unterminated terminal event', content('useful partial') + 'data: [DONE]'],
    ['usage-only response', 'data: {"choices":[],"usage":{}}\n\ndata: [DONE]\n\n'],
    ['array delta', 'data: {"choices":[{"delta":[],"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n'],
    ['array choice', 'data: {"choices":[[]]}\n\ndata: [DONE]\n\n'],
    ['missing finish reason', content('useful partial') + 'data: [DONE]\n\n'],
    ['length-limited output', content('useful partial') + 'data: {"choices":[{"delta":{},"finish_reason":"length"}]}\n\ndata: [DONE]\n\n'],
    ['content-filtered output', content('useful partial') + 'data: {"choices":[{"delta":{},"finish_reason":"content_filter"}]}\n\ndata: [DONE]\n\n'],
  ])('never reports success for %s', async (_name, stream) => {
    const result = await run_fixture((response) => { response.writeHead(200, { 'content-type': 'text/event-stream' }); response.end(stream); });
    expect(result.output).not.toContain('stream complete');
    expect(result.exit_code).toBe(1);
    expect(result.errors).toMatch(/failed|incomplete/i);
    if (stream.includes('useful partial')) expect(result.output).toContain('useful partial');
  });

  it('returns a failed exit status for HTTP failure', async () => {
    const result = await run_fixture((response) => { response.writeHead(401); response.end('{"error":"fixture"}'); });
    expect(result.exit_code).toBe(1);
    expect(result.output).not.toContain('stream complete');
    expect(result.errors).toContain('HTTP 401');
  });

  it('rejects malformed UTF-8 rather than reporting replacement characters as successful output', async () => {
    const result = await run_fixture((response) => {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(Buffer.concat([Buffer.from('data: {"choices":[{"delta":{"content":"'), Buffer.from([0xff]), Buffer.from('"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n')]));
    });
    expect(result.exit_code).toBe(1);
    expect(result.output).not.toContain('stream complete');
    expect(result.output).not.toContain('\uFFFD');
  });

  it('accepts a well-formed empty completion when a stop choice and DONE are both present', async () => {
    const result = await run_fixture((response) => { response.writeHead(200, { 'content-type': 'text/event-stream' }); response.end(terminal); });
    expect(result.exit_code).toBe(0);
    expect(result.output).toContain('stream complete');
  });

  it('returns a failed exit status when a response stream is cancelled', async () => {
    const controller = new AbortController();
    const result = await run_fixture((response) => {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.write(content('useful partial'));
      setImmediate(() => controller.abort());
    }, { signal: controller.signal });
    expect(result.exit_code).toBe(1);
    expect(result.output).not.toContain('stream complete');
    expect(result.errors).toMatch(/cancelled/i);
  });

  it('honors Commander --no-stream options and validates the JSON response', async () => {
    const result = await run_fixture((response) => { response.writeHead(200, { 'content-type': 'application/json' }); response.end('{"choices":[{"message":{"content":"fixture complete"}}]}'); }, { stream: false });
    expect(result.output).toContain('fixture complete');
    expect(result.exit_code).toBe(0);
    expect(result.output).not.toContain('stream complete');
  });

  it('reports pre-header cancellation without a success result', async () => {
    const controller = new AbortController();
    const result = await run_fixture(() => { setImmediate(() => controller.abort()); }, { signal: controller.signal });
    expect(result.exit_code).toBe(1);
    expect(result.errors).toMatch(/cancelled/i);
    expect(result.output).not.toContain('stream complete');
  });
});
