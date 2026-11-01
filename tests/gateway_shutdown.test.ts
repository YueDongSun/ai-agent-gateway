import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from 'vitest';
import { createServer as create_tcp_server } from 'node:net';
import type { AppConfig } from '../src/config/schema.js';
import { createGatewayApp, install_shutdown_handlers } from '../src/gateway/server.js';

const base_config = (): AppConfig => ({
  version: 1,
  gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'info' },
  providers: [],
  routes: [],
  profiles: [],
  agents: [],
});

/** Reserve an ephemeral port and release it, so start() gets a concrete port. */
async function free_port(): Promise<number> {
  const probe = create_tcp_server();
  await new Promise<void>((resolve) => probe.listen(0, '127.0.0.1', resolve));
  const { port } = probe.address() as { port: number };
  await new Promise<void>((resolve) => probe.close(() => resolve()));
  return port;
}

describe('gateway start returns a closeable server', () => {
  it('serves /health and stops accepting connections after close', async () => {
    const cfg = base_config();
    cfg.gateway.port = await free_port();
    const { start } = createGatewayApp(cfg);
    const server = start();

    await new Promise<void>((resolve, reject) => {
      server.once('listening', resolve);
      server.once('error', reject);
    });
    const port = (server.address() as { port: number }).port;

    const res = await fetch(`http://127.0.0.1:${port}/health`);
    expect(res.status).toBe(200);

    // Undici keep-alive sockets hold close() open; drop them like the
    // gateway_http_cancel suite does before the callback can fire.
    await new Promise<void>((resolve) => server.close(() => resolve()));
    server.closeAllConnections();
    await expect(fetch(`http://127.0.0.1:${port}/health`)).rejects.toThrow();
  }, 10_000);
});

describe('install_shutdown_handlers', () => {
  let on_spy: MockInstance<(signal: string, listener: (...args: never[]) => void) => typeof process>;
  let exit_spy: MockInstance<(code?: number) => never>;

  beforeEach(() => {
    // mockImplementation (not callThrough) keeps real signal listeners off
    // the vitest worker process.
    on_spy = vi.spyOn(process, 'on').mockImplementation(() => process);
    exit_spy = vi.spyOn(process, 'exit').mockImplementation(() => undefined as never);
  });

  afterEach(() => {
    on_spy.mockRestore();
    exit_spy.mockRestore();
  });

  const registered_handler = (signal: string) => {
    const call = on_spy.mock.calls.find(([name]) => name === signal);
    if (!call) throw new Error(`handler_not_registered:${signal}`);
    return call[1] as () => void;
  };

  it('registers SIGINT and SIGTERM', () => {
    install_shutdown_handlers({ close: () => {} });
    expect(() => registered_handler('SIGINT')).not.toThrow();
    expect(() => registered_handler('SIGTERM')).not.toThrow();
  });

  it('closes the server and exits 0 through the close callback', () => {
    let close_callback: (() => void) | undefined;
    // Large grace: this test never waits for the timer, and the pending
    // timer must not fire into a later test's spies.
    install_shutdown_handlers({ close: (cb?: () => void) => { close_callback = cb; } }, 60_000);

    registered_handler('SIGINT')();
    expect(close_callback).toBeTypeOf('function');
    expect(exit_spy).not.toHaveBeenCalled();

    close_callback!();
    expect(exit_spy).toHaveBeenCalledTimes(1);
    expect(exit_spy).toHaveBeenCalledWith(0);
  });

  it('exits immediately with 1 on a second signal', () => {
    const close_calls: number[] = [];
    install_shutdown_handlers(
      {
        close: (cb?: () => void) => { close_calls.push(1); void cb; },
      },
      60_000,
    );

    registered_handler('SIGINT')();
    expect(exit_spy).not.toHaveBeenCalled();

    registered_handler('SIGTERM')();
    expect(exit_spy).toHaveBeenCalledTimes(1);
    expect(exit_spy).toHaveBeenCalledWith(1);
    expect(close_calls).toHaveLength(1);
  });

  it('force-exits with 1 after the grace window when close never completes', async () => {
    install_shutdown_handlers({ close: () => {} }, 50);

    registered_handler('SIGTERM')();
    expect(exit_spy).not.toHaveBeenCalled();

    await new Promise((resolve) => setTimeout(resolve, 200));
    expect(exit_spy).toHaveBeenCalledTimes(1);
    expect(exit_spy).toHaveBeenCalledWith(1);
  });
});
