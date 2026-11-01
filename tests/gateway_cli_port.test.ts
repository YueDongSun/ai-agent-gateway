import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { AppConfig } from '../src/config/schema.js';

const base_config = (): AppConfig => ({
  version: 1,
  gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'info' },
  providers: [],
  routes: [],
  profiles: [],
  agents: [],
});

const { create_app_mock, install_shutdown_handlers_mock, load_config_mock } = vi.hoisted(() => {
  const create_app_mock = vi.fn(() => ({ start: vi.fn() }));
  const install_shutdown_handlers_mock = vi.fn();
  const load_config_mock = vi.fn();
  return { create_app_mock, install_shutdown_handlers_mock, load_config_mock };
});

vi.mock('../src/gateway/server.js', () => ({
  createGatewayApp: create_app_mock,
  install_shutdown_handlers: install_shutdown_handlers_mock,
}));
// loadConfig seeds a fresh document through the private-storage ACL adapter;
// this suite targets only the port boundary, so the store is stubbed.
vi.mock('../src/config/store.js', () => ({
  loadConfig: load_config_mock,
  saveConfig: vi.fn(),
  getConfigPath: vi.fn(() => 'fixture_config_path'),
  desktop_database_exists: vi.fn(() => false),
  desktop_database_path: vi.fn(() => 'fixture_aag_db'),
}));

const { startGatewayCmd } = await import('../src/cli/commands.js');

// The mocked server module is restored inside this suite for the boundary
// guard tests at the bottom.
const real_create_gateway_app = vi.importActual<typeof import('../src/gateway/server.js')>(
  '../src/gateway/server.js',
);

describe('gateway CLI port override', () => {
  let error_output: string[];
  let original_error: typeof console.error;

  beforeEach(() => {
    create_app_mock.mockClear();
    install_shutdown_handlers_mock.mockClear();
    load_config_mock.mockReset();
    load_config_mock.mockImplementation(base_config);
    error_output = [];
    original_error = console.error;
    console.error = (...values: unknown[]) => {
      error_output.push(values.join(' '));
    };
  });

  afterEach(() => {
    console.error = original_error;
    process.exitCode = 0;
  });

  it.each(['abc', '0', '99999', '8989.5'])(
    'rejects an invalid --port value %s before starting',
    async (invalid_port) => {
      await startGatewayCmd({ port: invalid_port });
      expect(process.exitCode).toBe(1);
      expect(create_app_mock).not.toHaveBeenCalled();
      expect(error_output.join('\n')).toContain('--port must be an integer between 1 and 65535');
    },
  );

  it('starts the gateway and installs shutdown handlers on a valid port', async () => {
    const server_fixture = { close: () => {} };
    const start_mock = vi.fn(() => server_fixture);
    create_app_mock.mockImplementationOnce(() => ({ start: start_mock }));
    // Not awaited: startGatewayCmd intentionally blocks forever once the
    // gateway is up, and a dangling promise does not block this suite.
    void startGatewayCmd({ port: '8989' });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(create_app_mock).toHaveBeenCalledTimes(1);
    expect(start_mock).toHaveBeenCalledTimes(1);
    expect(install_shutdown_handlers_mock).toHaveBeenCalledTimes(1);
    expect(install_shutdown_handlers_mock).toHaveBeenCalledWith(server_fixture);
  });
});

describe('gateway start port boundary guard', () => {
  it('throws a stable error for a non-integer or out-of-range port', async () => {
    const { createGatewayApp: real_app } = await real_create_gateway_app;
    for (const invalid_port of [Number.NaN, 0, 70_000]) {
      const cfg = base_config();
      cfg.gateway.port = invalid_port;
      const { start } = real_app(cfg);
      expect(() => start()).toThrow('gateway_invalid_port');
    }
  });
});
