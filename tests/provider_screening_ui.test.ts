import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  provider_screening_recheck, save_screening_thresholds, screening_counts, screening_error,
  screening_label, updated_screening_policy, type Screening_overview,
} from '../ui/src/lib/api/provider_screening.js';
import { mock_provider_screening_command } from '../ui/src/lib/mock/provider_screening.js';

const invoke = vi.fn();
beforeEach(() => { invoke.mockReset(); vi.stubGlobal('window', { __TAURI_INTERNALS__: { invoke } }); });
afterEach(() => vi.unstubAllGlobals());

describe('provider screening presentation and actions', () => {
  it('counts unique provider/model pairs independently of reuse and excluded entries', () => {
    const overview = mock_provider_screening_command('provider_screening_overview', {});
    const duplicate = { ...overview.entries[0], admittedModels: ['fixture_model'] };
    const reused = { ...overview.entries[0], providerId: 'another_fixture', admittedModels: ['fixture_model'] };
    expect(screening_counts([...overview.entries, duplicate, reused])).toEqual({ providers: 2, pairs: 3, excluded: 1 });
  });

  it('updates only threshold and deadline while preserving token and capture limits', () => {
    const policy = { max_latency_ms: 5000, probe_deadline_ms: 15000, max_output_tokens: 96, max_response_chars: 2048 };
    expect(updated_screening_policy(policy, 6000, 18000)).toEqual({ ...policy, max_latency_ms: 6000, probe_deadline_ms: 18000 });
    for (const [latency, deadline] of [[0, 15000], [15001, 15000], [1.5, 15000], [5000, 120001], [NaN, 15000]]) {
      expect(() => updated_screening_policy(policy, latency, deadline)).toThrow('provider_screening_policy_invalid');
    }
  });

  it('reads fresh policy limits before save and reads the resulting overview', async () => {
    const current = mock_provider_screening_command('provider_screening_overview', {});
    current.policy.max_output_tokens = 96;
    const saved: Screening_overview = { ...current, policy: { ...current.policy, max_latency_ms: 6000, probe_deadline_ms: 18000 } };
    invoke.mockResolvedValueOnce(current).mockResolvedValueOnce(null).mockResolvedValueOnce(saved);
    await expect(save_screening_thresholds(6000, 18000)).resolves.toEqual(saved);
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(['provider_screening_overview', 'settings_set', 'provider_screening_overview']);
    expect(JSON.parse(invoke.mock.calls[1][1].value)).toEqual(saved.policy);
    expect(invoke.mock.calls[1][1].key).toBe('provider_screening_policy');
  });

  it('rechecks one provider with no prompt or business payload', async () => {
    const receipt = mock_provider_screening_command('provider_screening_recheck', { providerId: 'fixture_expired' });
    invoke.mockResolvedValueOnce(receipt);
    await provider_screening_recheck('fixture_expired');
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0].slice(0, 2)).toEqual(['provider_screening_recheck', { providerId: 'fixture_expired' }]);
    const checked = mock_provider_screening_command('provider_screening_recheck', { providerId: 'fixture_expired' });
    expect(checked.overview?.entries[1].admittedModels).toEqual([]);
    expect(checked.overview?.entries[1].probeModel).toBeNull();
    expect(checked.overview?.entries[1].response).toBeNull();
    expect(checked.overview?.entries[0].response).toContain('Synthetic browser demo');
  });

  it('does not forward raw exceptions as screening notices', () => {
    expect(screening_error('private credential or upstream body')).not.toContain('private credential');
    expect(screening_label('quota_exceeded')).toBe('Provider quota is exhausted');
    expect(screening_label('https://private.invalid?secret=value')).toBe('Unrecognized status');
  });
});
