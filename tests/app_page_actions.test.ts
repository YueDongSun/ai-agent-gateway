import { describe, expect, it, vi } from 'vitest';
import {
  create_gateway_actions,
  type Gateway_action_options,
} from '../ui/src/components/suppliers/client_management/gateway_flow.js';
import type { ProxyConfig } from '../ui/src/lib/api/proxy.js';

function create_fixture(overrides: Partial<Gateway_action_options> = {}) {
  const events: string[] = [];
  const baseline: ProxyConfig = {
    host: 'localhost', port: 8989, authToken: 'fixture_old_token', enabled: false,
  };
  const commands = {
    start_proxy: vi.fn(async () => { events.push('start'); return 'started'; }),
    stop_proxy: vi.fn(async () => { events.push('stop'); }),
    get_proxy_config: vi.fn(async () => { events.push('load'); return baseline; }),
    save_proxy_config: vi.fn(async (_config: ProxyConfig) => { events.push('save'); }),
    set_pinned_provider: vi.fn(async (_provider_id: string) => { events.push('pin'); }),
    set_responses_cot: vi.fn(async (_enabled: boolean) => { events.push('cot'); }),
  };
  const refresh_page = vi.fn(async () => { events.push('refresh'); });
  const translate = (key: string, vars?: Record<string, string | number>) =>
    vars ? `${key}:${JSON.stringify(vars)}` : key;
  const flash = vi.fn((message: string) => { events.push(`flash:${message}`); });
  const set_busy = vi.fn((action: string | null) => { events.push(`busy:${action}`); });
  const set_error = vi.fn((message: string | null) => { events.push(`error:${message}`); });
  const set_gateway_form = vi.fn();
  const set_settings_open = vi.fn((open: boolean) => { events.push(`settings:${open}`); });
  const set_pinned_id = vi.fn();
  const set_cot_on = vi.fn((enabled: boolean) => { events.push(`local_cot:${enabled}`); });
  const options: Gateway_action_options = {
    commands,
    gateway_form: { host: ' 127.0.0.1 ', port: '9001', auth_token: ' fixture_new_token ' },
    pinned_id: null,
    refresh_page,
    translate,
    flash,
    set_busy,
    set_error,
    set_gateway_form,
    set_settings_open,
    set_pinned_id,
    set_cot_on,
    ...overrides,
  };
  return {
    events, baseline, commands, options, refresh_page, translate, flash,
    set_busy, set_error, set_gateway_form, set_settings_open, set_pinned_id, set_cot_on,
    actions: create_gateway_actions(options),
  };
}

describe('gateway lifecycle flow', () => {
  it('starts then refreshes the page before clearing busy', async () => {
    const fixture = create_fixture();
    await fixture.actions.start_gateway();
    expect(fixture.events).toEqual(['busy:start', 'start', 'refresh', 'busy:null']);
  });

  it('reports a port fallback before refreshing', async () => {
    const fixture = create_fixture();
    fixture.commands.start_proxy.mockResolvedValue('port 8989 was in use; saved as the new default');
    await fixture.actions.start_gateway();
    expect(fixture.events).toEqual([
      'busy:start',
      'flash:app.gateway.portFallback:{"port":"8989"}',
      'refresh',
      'busy:null',
    ]);
  });

  it.each(['proxy_bind_exhausted', 'no free port'])('translates the start conflict %s', async (message) => {
    const fixture = create_fixture();
    fixture.commands.start_proxy.mockRejectedValue(new Error(message));
    await fixture.actions.start_gateway();
    expect(fixture.set_error).toHaveBeenCalledWith(
      fixture.translate('app.gateway.portConflict', { message: `Error: ${message}` }),
    );
    expect(fixture.refresh_page).not.toHaveBeenCalled();
    expect(fixture.events.at(-1)).toBe('busy:null');
  });

  it('keeps other start errors and clears busy', async () => {
    const fixture = create_fixture();
    fixture.commands.start_proxy.mockRejectedValue(new Error('fixture_failure'));
    await fixture.actions.start_gateway();
    expect(fixture.set_error).toHaveBeenCalledWith('Error: fixture_failure');
    expect(fixture.events.at(-1)).toBe('busy:null');
  });

  it('restarts in stop/start/refresh/notice order even if stopping rejects', async () => {
    const fixture = create_fixture();
    fixture.commands.stop_proxy.mockImplementation(async () => {
      fixture.events.push('stop');
      throw new Error('not_running');
    });
    await fixture.actions.restart_gateway();
    expect(fixture.events).toEqual([
      'busy:restart', 'stop', 'start', 'refresh', 'flash:app.gateway.restarted', 'busy:null',
    ]);
    expect(fixture.set_error).not.toHaveBeenCalled();
  });

  it('does not report restart success when start fails', async () => {
    const fixture = create_fixture();
    fixture.commands.start_proxy.mockRejectedValue(new Error('restart_failed'));
    await fixture.actions.restart_gateway();
    expect(fixture.set_error).toHaveBeenCalledWith('Error: restart_failed');
    expect(fixture.refresh_page).not.toHaveBeenCalled();
    expect(fixture.flash).not.toHaveBeenCalled();
    expect(fixture.events.at(-1)).toBe('busy:null');
  });

  it('stops then refreshes before clearing busy', async () => {
    const fixture = create_fixture();
    await fixture.actions.stop_gateway();
    expect(fixture.events).toEqual(['busy:stop', 'stop', 'refresh', 'busy:null']);
  });

  it('keeps stop failure visible without refreshing', async () => {
    const fixture = create_fixture();
    fixture.commands.stop_proxy.mockRejectedValue(new Error('stop_failed'));
    await fixture.actions.stop_gateway();
    expect(fixture.refresh_page).not.toHaveBeenCalled();
    expect(fixture.set_error).toHaveBeenCalledWith('Error: stop_failed');
    expect(fixture.events.at(-1)).toBe('busy:null');
  });

  it.each(['start_gateway', 'restart_gateway', 'stop_gateway'] as const)(
    'clears busy after a refresh rejection in %s', async (action) => {
      const fixture = create_fixture();
      fixture.refresh_page.mockRejectedValue(new Error('refresh_failed'));
      await fixture.actions[action]();
      expect(fixture.set_error).toHaveBeenCalledWith('Error: refresh_failed');
      expect(fixture.events.at(-1)).toBe('busy:null');
      expect(fixture.flash).not.toHaveBeenCalled();
    },
  );
});

describe('gateway settings flow', () => {
  it('opens before loading the config and adapts its external fields', async () => {
    const fixture = create_fixture();
    await fixture.actions.open_gateway_settings();
    expect(fixture.events).toEqual(['settings:true', 'load']);
    expect(fixture.set_gateway_form).toHaveBeenCalledWith({
      host: 'localhost', port: '8989', auth_token: 'fixture_old_token',
    });
  });

  it('leaves the modal open with an error if loading fails', async () => {
    const fixture = create_fixture();
    fixture.commands.get_proxy_config.mockRejectedValue(new Error('load_failed'));
    await fixture.actions.open_gateway_settings();
    expect(fixture.set_settings_open).toHaveBeenCalledOnce();
    expect(fixture.set_settings_open).toHaveBeenCalledWith(true);
    expect(fixture.set_gateway_form).not.toHaveBeenCalled();
    expect(fixture.set_error).toHaveBeenCalledWith('Error: load_failed');
  });

  it('re-reads the latest baseline and preserves unrelated fields when saving', async () => {
    const fixture = create_fixture();
    await fixture.actions.open_gateway_settings();
    fixture.commands.get_proxy_config.mockImplementation(async () => {
      fixture.events.push('load');
      return { ...fixture.baseline, enabled: true };
    });
    fixture.events.length = 0;
    await fixture.actions.save_gateway_settings();
    expect(fixture.commands.get_proxy_config).toHaveBeenCalledTimes(2);
    expect(fixture.commands.save_proxy_config).toHaveBeenCalledWith({
      host: '127.0.0.1', port: 9001, authToken: 'fixture_new_token', enabled: true,
    });
    expect(fixture.events).toEqual([
      'load', 'save', 'settings:false', 'flash:app.gateway.savedRestartHint', 'refresh',
    ]);
    expect(fixture.commands.start_proxy).not.toHaveBeenCalled();
    expect(fixture.commands.stop_proxy).not.toHaveBeenCalled();
  });

  it.each(['', '0', '65536', '1.5', 'invalid'])(
    'rejects port %j without writing or closing', async (port) => {
      const fixture = create_fixture({ gateway_form: { host: 'localhost', port, auth_token: '' } });
      await fixture.actions.save_gateway_settings();
      expect(fixture.commands.get_proxy_config).toHaveBeenCalledOnce();
      expect(fixture.set_error).toHaveBeenCalledWith('app.gateway.invalidPort');
      expect(fixture.commands.save_proxy_config).not.toHaveBeenCalled();
      expect(fixture.set_settings_open).not.toHaveBeenCalled();
      expect(fixture.refresh_page).not.toHaveBeenCalled();
    },
  );

  it('rejects an invalid host without writing', async () => {
    const fixture = create_fixture({
      gateway_form: { host: 'bad host', port: '9001', auth_token: '' },
    });
    await fixture.actions.save_gateway_settings();
    expect(fixture.set_error).toHaveBeenCalledWith('app.gateway.invalidHost');
    expect(fixture.commands.save_proxy_config).not.toHaveBeenCalled();
  });

  it('does not overwrite config when the fresh baseline cannot be loaded', async () => {
    const fixture = create_fixture();
    fixture.commands.get_proxy_config.mockRejectedValue(new Error('baseline_failed'));
    await fixture.actions.save_gateway_settings();
    expect(fixture.set_error).toHaveBeenCalledWith('Error: baseline_failed');
    expect(fixture.commands.save_proxy_config).not.toHaveBeenCalled();
  });

  it('keeps the modal open if persistence fails', async () => {
    const fixture = create_fixture();
    fixture.commands.save_proxy_config.mockRejectedValue(new Error('save_failed'));
    await fixture.actions.save_gateway_settings();
    expect(fixture.set_error).toHaveBeenCalledWith('Error: save_failed');
    expect(fixture.set_settings_open).not.toHaveBeenCalled();
    expect(fixture.flash).not.toHaveBeenCalled();
    expect(fixture.refresh_page).not.toHaveBeenCalled();
  });

  it('reports a refresh error after saving rather than losing its context', async () => {
    const fixture = create_fixture();
    fixture.refresh_page.mockRejectedValue(new Error('refresh_failed'));
    await fixture.actions.save_gateway_settings();
    expect(fixture.commands.save_proxy_config).toHaveBeenCalledOnce();
    expect(fixture.set_settings_open).toHaveBeenCalledWith(false);
    expect(fixture.set_error).toHaveBeenCalledWith('Error: refresh_failed');
  });
});

describe('gateway live controls', () => {
  it('sets a pin after persistence and clears it when toggled again', async () => {
    const fixture = create_fixture();
    await fixture.actions.toggle_gateway_pin('provider_fixture');
    expect(fixture.commands.set_pinned_provider).toHaveBeenLastCalledWith('provider_fixture');
    expect(fixture.set_pinned_id).toHaveBeenLastCalledWith('provider_fixture');
    const current_actions = create_gateway_actions({ ...fixture.options, pinned_id: 'provider_fixture' });
    await current_actions.toggle_gateway_pin('provider_fixture');
    expect(fixture.commands.set_pinned_provider).toHaveBeenLastCalledWith('');
    expect(fixture.set_pinned_id).toHaveBeenLastCalledWith(null);
    expect(fixture.flash).toHaveBeenLastCalledWith('providers.unpinned');
  });

  it('does not change local pin state if persistence fails', async () => {
    const fixture = create_fixture();
    fixture.commands.set_pinned_provider.mockRejectedValue(new Error('pin_failed'));
    await fixture.actions.toggle_gateway_pin('provider_fixture');
    expect(fixture.set_pinned_id).not.toHaveBeenCalled();
    expect(fixture.flash).not.toHaveBeenCalled();
    expect(fixture.set_error).toHaveBeenCalledWith('Error: pin_failed');
  });

  it.each([true, false])('optimistically sets CoT to %s', async (enabled) => {
    const fixture = create_fixture();
    await fixture.actions.toggle_gateway_cot(enabled);
    expect(fixture.events).toEqual([
      `local_cot:${enabled}`, 'cot',
      `flash:${enabled ? 'app.gateway.cot.onFlash' : 'app.gateway.cot.offFlash'}`,
    ]);
    expect(fixture.commands.set_responses_cot).toHaveBeenCalledWith(enabled);
  });

  it('rolls CoT back without a success notice on failure', async () => {
    const fixture = create_fixture();
    fixture.commands.set_responses_cot.mockRejectedValue(new Error('cot_failed'));
    await fixture.actions.toggle_gateway_cot(true);
    expect(fixture.set_cot_on.mock.calls).toEqual([[true], [false]]);
    expect(fixture.flash).not.toHaveBeenCalled();
    expect(fixture.set_error).toHaveBeenCalledWith('Error: cot_failed');
  });

  it('uses the newest page callback and edited form when actions are rebuilt', async () => {
    const fixture = create_fixture();
    const latest_refresh = vi.fn(async () => {});
    const current_actions = create_gateway_actions({
      ...fixture.options,
      refresh_page: latest_refresh,
      gateway_form: { host: 'localhost', port: '9002', auth_token: 'fixture_latest' },
    });
    await current_actions.save_gateway_settings();
    expect(fixture.commands.save_proxy_config).toHaveBeenCalledWith({
      ...fixture.baseline, host: 'localhost', port: 9002, authToken: 'fixture_latest',
    });
    expect(latest_refresh).toHaveBeenCalledOnce();
    expect(fixture.refresh_page).not.toHaveBeenCalled();
  });
});
