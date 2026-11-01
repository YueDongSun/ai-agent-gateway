import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { instance_endpoint, tailnet_discovery_error, get_tailnet_discovery_status, scan_tailnet_instances, cancel_tailnet_discovery } from '../ui/src/lib/api/tailnet_discovery.js';
import { mock_tailnet_discovery_command } from '../ui/src/lib/mock/tailnet_discovery.js';

const invoke = vi.fn();
beforeEach(() => vi.stubGlobal('window', { __TAURI_INTERNALS__: { invoke } }));
afterEach(() => vi.unstubAllGlobals());

describe('Tailnet discovery presentation contract', () => {
  it('formats literal IPv4 and IPv6 addresses without changing the selected port', () => {
    const common = { deviceName: 'fixture', port: 9001, version: '0.2.0', latencyMs: 1, isSelf: false };
    expect(instance_endpoint({ ...common, address: '192.0.2.2' })).toBe('http://192.0.2.2:9001');
    expect(instance_endpoint({ ...common, address: '2001:db8::2' })).toBe('http://[2001:db8::2]:9001');
  });
  it('only renders known stable errors, never arbitrary metadata', () => {
    expect(tailnet_discovery_error('tailnet_discovery_stopped')).toBe('tailnet_discovery_stopped');
    expect(tailnet_discovery_error('private host credential')).toBe('tailnet_discovery_failed');
  });
  it('uses the three native commands with only its owned request identifier', async () => {
    invoke.mockReset();
    invoke.mockResolvedValue(null);
    await get_tailnet_discovery_status();
    await scan_tailnet_instances('fixture_a');
    await cancel_tailnet_discovery('fixture_a');
    expect(invoke.mock.calls.map(([command, args]) => [command, args])).toEqual([
      ['tailnet_discovery_status', {}],
      ['tailnet_discovery_scan', { requestId: 'fixture_a' }],
      ['tailnet_discovery_cancel', { requestId: 'fixture_a' }],
    ]);
  });
  it('synthetic scanning uses the configured port, reports proof and remains request owned', async () => {
    const pending = mock_tailnet_discovery_command('tailnet_discovery_scan', { requestId: 'fixture_scan' }, 9001);
    await expect(mock_tailnet_discovery_command('tailnet_discovery_scan', { requestId: 'fixture_other' }, 9001)).rejects.toThrow('tailnet_discovery_busy');
    expect(await mock_tailnet_discovery_command('tailnet_discovery_cancel', { requestId: 'fixture_other' })).toBe(false);
    const report = await pending as { port: number; synthetic: boolean; instances: Array<{ port: number }> };
    expect(report.port).toBe(9001);
    expect(report.synthetic).toBe(true);
    expect(report.instances.every((instance) => instance.port === 9001)).toBe(true);
  });
  it('an explicit matching cancel has a terminal cancelled outcome', async () => {
    const pending = mock_tailnet_discovery_command('tailnet_discovery_scan', { requestId: 'fixture_cancel' });
    expect(await mock_tailnet_discovery_command('tailnet_discovery_cancel', { requestId: 'fixture_cancel' })).toBe(true);
    await expect(pending).rejects.toThrow('tailnet_discovery_cancelled');
  });
});
