import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { provider_screening_recheck, screening_error, screening_label } from '../ui/src/lib/api/provider_screening.js';
import { mock_provider_screening_command } from '../ui/src/lib/mock/provider_screening.js';

const invoke = vi.fn();
beforeEach(() => {
  invoke.mockReset();
  vi.stubGlobal('window', { __TAURI_INTERNALS__: { invoke } });
});
afterEach(() => vi.unstubAllGlobals());

describe('selected provider recheck receipts', () => {
  it('returns the selected outcome and actual overview without claiming registration or policy changes', () => {
    const result = mock_provider_screening_command('provider_screening_recheck', { providerId: 'fixture_expired' }) as unknown as {
      providerId: string; checkStarted: boolean; eligible: boolean;
      changes: { policyChanged: boolean; registrationChanged: boolean };
      overview: { enforced: boolean; entries: Array<{ providerId: string; admittedModels: string[] }> };
    };
    expect(result.providerId).toBe('fixture_expired');
    expect(result.checkStarted).toBe(true);
    expect(result.eligible).toBe(false);
    expect(result.changes.policyChanged).toBe(false);
    expect(result.changes.registrationChanged).toBe(false);
    expect(result.overview.entries.find((entry) => entry.providerId === 'fixture_expired')?.admittedModels).toEqual([]);
  });

  it('reads backend truth once after a lost acknowledgement without resending the check', async () => {
    const overview = mock_provider_screening_command('provider_screening_overview', {});
    invoke.mockRejectedValueOnce(new Error('private fixture payload')).mockResolvedValueOnce(overview);
    const result = await provider_screening_recheck('fixture_screened');
    expect(result).toMatchObject({ providerId: 'fixture_screened', checkStarted: null, checkError: 'provider_screening_outcome_unknown', overview });
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(['provider_screening_recheck', 'provider_screening_overview']);
    expect(JSON.stringify(result)).not.toContain('private fixture payload');
  });

  it('keeps committed partial results separate from failed runtime reconciliation', async () => {
    const receipt = mock_provider_screening_command('provider_screening_recheck', { providerId: 'fixture_expired' });
    receipt.runtimeReconciled = false;
    receipt.runtimeError = 'provider_screening_runtime_reconcile_failed';
    invoke.mockResolvedValueOnce(receipt);
    expect(await provider_screening_recheck('fixture_expired')).toEqual(receipt);
    expect(receipt.changes.admissionCommitted).toBe(true);
    expect(receipt.overview?.entries[1].admittedModels).toEqual([]);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it('reports unavailable readback explicitly without inferring cancellation or retrying', async () => {
    invoke.mockRejectedValueOnce(new Error('private acknowledgement')).mockRejectedValueOnce(new Error('private readback'));
    const receipt = await provider_screening_recheck('fixture_screened');
    expect(receipt).toMatchObject({ checkStarted: null, checkError: 'provider_screening_outcome_unknown', overview: null, overviewError: 'provider_screening_overview_unavailable' });
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(JSON.stringify(receipt)).not.toContain('private');
  });

  it('describes unknown, busy and provisional outcomes in Chinese without echoing raw errors', () => {
    expect(screening_error('provider_screening_outcome_unknown', 'zh')).toContain('并未因此取消');
    expect(screening_error('provider_admission_busy', 'zh')).toContain('已有检查');
    expect(screening_label('awaiting_registration', 'zh')).toContain('等待显式导入');
    expect(screening_error('private fixture payload', 'zh')).not.toContain('private');
  });
});
