import { describe, expect, it, vi } from 'vitest';
import { run_cc_switch_import } from '../ui/src/components/suppliers/client_management/import_flow.js';
import type { Run_cc_switch_import_options } from '../ui/src/components/suppliers/client_management/import_flow.js';
import type { ImportReport } from '../ui/src/lib/api/types.js';

function make_report(overrides: Partial<ImportReport> = {}): ImportReport {
  return {
    sourceDb: 'fixture.db',
    imported: [],
    skippedExisting: [],
    skippedNoKey: [],
    totalInSource: 0,
    importedRoutes: [],
    ...overrides,
  };
}

function create_fixture(report: ImportReport, running = false) {
  const events: string[] = [];
  const options = {
    running,
    import_providers: vi.fn(async () => {
      events.push('import_providers');
      return report;
    }),
    stop_proxy: vi.fn(async () => {
      events.push('stop_proxy');
    }),
    start_proxy: vi.fn(async () => {
      events.push('start_proxy');
      return 'started';
    }),
    refresh_page: vi.fn(async () => {
      events.push('refresh_page');
    }),
    translate: vi.fn((key: string, vars?: Record<string, string | number>) => {
      events.push(`translate:${key}`);
      return vars === undefined ? key : `${key}:${JSON.stringify(vars)}`;
    }),
    flash: vi.fn((message: string) => {
      events.push(`flash:${message}`);
    }),
    set_importing: vi.fn((value: boolean) => {
      events.push(`set_importing:${value}`);
    }),
    set_error: vi.fn((value: string | null) => {
      events.push(`set_error:${value === null ? 'null' : value}`);
    }),
  } satisfies Run_cc_switch_import_options;
  return { options, events };
}

function expect_importing_cleared(options: Run_cc_switch_import_options): void {
  expect(options.set_importing).toHaveBeenNthCalledWith(1, true);
  expect(options.set_importing).toHaveBeenLastCalledWith(false);
  expect(options.set_importing).toHaveBeenCalledTimes(2);
}

describe('cc-switch import flow', () => {
  it('reports an empty import and refreshes without restarting', async () => {
    const fixture = create_fixture(make_report(), true);

    await run_cc_switch_import(fixture.options);

    expect(fixture.options.flash).toHaveBeenCalledWith('providers.importCcSwitch.empty');
    expect(fixture.options.stop_proxy).not.toHaveBeenCalled();
    expect(fixture.options.start_proxy).not.toHaveBeenCalled();
    expect(fixture.options.refresh_page).toHaveBeenCalledOnce();
    expect(fixture.options.set_error).toHaveBeenCalledOnce();
    expect(fixture.options.set_error).toHaveBeenCalledWith(null);
    expect_importing_cleared(fixture.options);
    expect(fixture.events).toEqual([
      'set_importing:true',
      'set_error:null',
      'import_providers',
      'translate:providers.importCcSwitch.empty',
      'flash:providers.importCcSwitch.empty',
      'refresh_page',
      'set_importing:false',
    ]);
  });

  it('reports skipped existing providers instead of treating the report as empty', async () => {
    const fixture = create_fixture(
      make_report({
        skippedExisting: [{ id: 'existing', appType: 'claude_code', name: 'Existing' }],
        totalInSource: 1,
      }),
      true,
    );

    await run_cc_switch_import(fixture.options);

    expect(fixture.options.translate).toHaveBeenCalledWith('providers.importCcSwitch.report', {
      imported: '0',
      skipped: '1',
      total: '1',
    });
    expect(fixture.options.flash).toHaveBeenCalledWith(
      'providers.importCcSwitch.report:{"imported":"0","skipped":"1","total":"1"}',
    );
    expect(fixture.options.stop_proxy).not.toHaveBeenCalled();
    expect(fixture.options.start_proxy).not.toHaveBeenCalled();
    expect_importing_cleared(fixture.options);
  });

  it('reports no-key rows and does not restart when nothing was imported', async () => {
    const fixture = create_fixture(
      make_report({
        skippedNoKey: [
          { id: 'no-key-1', appType: 'claude_code', name: 'No key 1' },
          { id: 'no-key-2', appType: 'codex', name: 'No key 2' },
        ],
        totalInSource: 2,
      }),
      true,
    );

    await run_cc_switch_import(fixture.options);

    expect(fixture.options.translate).toHaveBeenNthCalledWith(1, 'providers.importCcSwitch.report', {
      imported: '0',
      skipped: '0',
      total: '2',
    });
    expect(fixture.options.translate).toHaveBeenNthCalledWith(2, 'providers.importCcSwitch.noKey', {
      n: '2',
    });
    expect(fixture.options.stop_proxy).not.toHaveBeenCalled();
    expect(fixture.options.start_proxy).not.toHaveBeenCalled();
    expect(fixture.options.flash).toHaveBeenCalledWith(
      'providers.importCcSwitch.report:{"imported":"0","skipped":"0","total":"2"} · providers.importCcSwitch.noKey:{"n":"2"}',
    );
    expect_importing_cleared(fixture.options);
  });

  it('reports all counts in order and counts duplicate route aliases once', async () => {
    const fixture = create_fixture(
      make_report({
        imported: [
          { id: 'provider-1', appType: 'claude_code', name: 'Provider 1' },
          { id: 'provider-2', appType: 'claude_code', name: 'Provider 2' },
        ],
        skippedExisting: [{ id: 'existing', appType: 'claude_code', name: 'Existing' }],
        skippedNoKey: [
          { id: 'no-key-1', appType: 'claude_code', name: 'No key 1' },
          { id: 'no-key-2', appType: 'codex', name: 'No key 2' },
        ],
        totalInSource: 5,
        importedRoutes: [
          { alias: 'claude-sonnet', provider: 'provider-1', model: 'sonnet' },
          { alias: 'claude-sonnet', provider: 'provider-2', model: 'sonnet-latest' },
          { alias: 'claude-opus', provider: 'provider-1', model: 'opus' },
        ],
        queuedProviders: [{ id: 'provider-1', appType: 'claude_code', name: 'Provider 1' }],
        passthroughProviders: [
          { id: 'provider-2', appType: 'claude_code', name: 'Provider 2' },
        ],
      }),
      true,
    );

    await run_cc_switch_import(fixture.options);

    const expected_flash =
      'providers.importCcSwitch.report:{"imported":"2","skipped":"1","total":"5"}' +
      ' · providers.importCcSwitch.routes:{"n":"2"}' +
      ' · providers.importCcSwitch.noKey:{"n":"2"}' +
      ' · providers.importCcSwitch.queued:{"n":"1"}' +
      ' · providers.importCcSwitch.passthrough:{"n":"1"}' +
      ' · providers.importCcSwitch.restarted';
    expect(fixture.options.stop_proxy).toHaveBeenCalledOnce();
    expect(fixture.options.start_proxy).toHaveBeenCalledOnce();
    expect(fixture.options.translate.mock.calls.map(([key]) => key)).toEqual([
      'providers.importCcSwitch.report',
      'providers.importCcSwitch.routes',
      'providers.importCcSwitch.noKey',
      'providers.importCcSwitch.queued',
      'providers.importCcSwitch.passthrough',
      'providers.importCcSwitch.restarted',
    ]);
    expect(fixture.options.flash).toHaveBeenCalledWith(expected_flash);
    expect(fixture.options.refresh_page).toHaveBeenCalledOnce();
    expect_importing_cleared(fixture.options);
    expect(fixture.events).toEqual([
      'set_importing:true',
      'set_error:null',
      'import_providers',
      'stop_proxy',
      'start_proxy',
      'translate:providers.importCcSwitch.report',
      'translate:providers.importCcSwitch.routes',
      'translate:providers.importCcSwitch.noKey',
      'translate:providers.importCcSwitch.queued',
      'translate:providers.importCcSwitch.passthrough',
      'translate:providers.importCcSwitch.restarted',
      `flash:${expected_flash}`,
      'refresh_page',
      'set_importing:false',
    ]);
  });

  it('omits suffixes when optional report fields are absent', async () => {
    const report: ImportReport = {
      sourceDb: 'fixture.db',
      imported: [{ id: 'provider-1', appType: 'claude_code', name: 'Provider 1' }],
      skippedExisting: [],
      skippedNoKey: [],
      totalInSource: 1,
      importedRoutes: [],
    };
    const fixture = create_fixture(report);

    await run_cc_switch_import(fixture.options);

    expect(fixture.options.translate.mock.calls).toEqual([
      [
        'providers.importCcSwitch.report',
        { imported: '1', skipped: '0', total: '1' },
      ],
    ]);
    expect(fixture.options.flash).toHaveBeenCalledWith(
      'providers.importCcSwitch.report:{"imported":"1","skipped":"0","total":"1"}',
    );
    expect(fixture.options.stop_proxy).not.toHaveBeenCalled();
    expect(fixture.options.start_proxy).not.toHaveBeenCalled();
    expect_importing_cleared(fixture.options);
  });

  it('does not restart when the proxy is not running', async () => {
    const fixture = create_fixture(
      make_report({
        imported: [{ id: 'provider-1', appType: 'claude_code', name: 'Provider 1' }],
        totalInSource: 1,
      }),
      false,
    );

    await run_cc_switch_import(fixture.options);

    expect(fixture.options.stop_proxy).not.toHaveBeenCalled();
    expect(fixture.options.start_proxy).not.toHaveBeenCalled();
    expect(fixture.options.translate).not.toHaveBeenCalledWith(
      'providers.importCcSwitch.restarted',
    );
    expect_importing_cleared(fixture.options);
  });

  it('continues to start the proxy when stopping rejects', async () => {
    const fixture = create_fixture(
      make_report({
        imported: [{ id: 'provider-1', appType: 'claude_code', name: 'Provider 1' }],
        totalInSource: 1,
      }),
      true,
    );
    fixture.options.stop_proxy.mockImplementation(async () => {
      fixture.events.push('stop_proxy');
      throw new Error('stop failed');
    });

    await run_cc_switch_import(fixture.options);

    expect(fixture.options.stop_proxy).toHaveBeenCalledOnce();
    expect(fixture.options.start_proxy).toHaveBeenCalledOnce();
    expect(fixture.options.set_error).toHaveBeenCalledOnce();
    expect(fixture.options.set_error).toHaveBeenCalledWith(null);
    expect(fixture.options.flash).toHaveBeenCalledWith(
      'providers.importCcSwitch.report:{"imported":"1","skipped":"0","total":"1"} · providers.importCcSwitch.restarted',
    );
    expect(fixture.events.indexOf('stop_proxy')).toBeLessThan(
      fixture.events.indexOf('start_proxy'),
    );
    expect_importing_cleared(fixture.options);
  });

  it('reports import failures and clears importing state', async () => {
    const fixture = create_fixture(make_report());
    fixture.options.import_providers.mockRejectedValue(new Error('import failed'));

    await run_cc_switch_import(fixture.options);

    expect(fixture.options.set_error).toHaveBeenNthCalledWith(1, null);
    expect(fixture.options.set_error).toHaveBeenNthCalledWith(
      2,
      'providers.importCcSwitch.failed:{"message":"Error: import failed"}',
    );
    expect(fixture.options.flash).not.toHaveBeenCalled();
    expect(fixture.options.refresh_page).not.toHaveBeenCalled();
    expect_importing_cleared(fixture.options);
  });

  it('reports start failures without flashing success or refreshing', async () => {
    const fixture = create_fixture(
      make_report({
        imported: [{ id: 'provider-1', appType: 'claude_code', name: 'Provider 1' }],
        totalInSource: 1,
      }),
      true,
    );
    fixture.options.start_proxy.mockRejectedValue(new Error('start failed'));

    await run_cc_switch_import(fixture.options);

    expect(fixture.options.stop_proxy).toHaveBeenCalledOnce();
    expect(fixture.options.start_proxy).toHaveBeenCalledOnce();
    expect(fixture.options.set_error).toHaveBeenNthCalledWith(
      2,
      'providers.importCcSwitch.failed:{"message":"Error: start failed"}',
    );
    expect(fixture.options.flash).not.toHaveBeenCalled();
    expect(fixture.options.refresh_page).not.toHaveBeenCalled();
    expect_importing_cleared(fixture.options);
  });

  it('reports refresh failures after the import result flash', async () => {
    const fixture = create_fixture(make_report());
    fixture.options.refresh_page.mockRejectedValue(new Error('refresh failed'));

    await run_cc_switch_import(fixture.options);

    expect(fixture.options.flash).toHaveBeenCalledWith('providers.importCcSwitch.empty');
    expect(fixture.options.set_error).toHaveBeenNthCalledWith(
      2,
      'providers.importCcSwitch.failed:{"message":"Error: refresh failed"}',
    );
    expect(fixture.events).toContain('translate:providers.importCcSwitch.failed');
    expect_importing_cleared(fixture.options);
  });
});
