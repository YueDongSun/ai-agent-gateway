import { describe, expect, it, vi } from 'vitest';
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
import { prompt_error, prompt_recovery_message } from '../ui/src/lib/api/prompt.js';
import { mock_prompt_command } from '../ui/src/lib/mock/prompt.js';

describe('prompt recovery presentation contract', () => {
  it('explains known admission failures without forwarding source or credential details', () => {
    const error = prompt_error('provider_subscription_source_changed:private source data');
    expect(error).toEqual({ code: 'provider_subscription_source_changed', detail: null });
    expect(prompt_recovery_message(error.code)).toContain('Recheck this provider in Suppliers');
    expect(prompt_error('provider_subscription_invalid:private source data')).toEqual({ code: 'prompt_enhance_unknown', detail: null });
  });
  it('recognizes bounded recovery failures without forwarding upstream payloads', () => {
    const error = prompt_error('prompt_enhance_recovery_budget_exhausted:private upstream response');
    expect(error).toEqual({ code: 'prompt_enhance_recovery_budget_exhausted', detail: null });
    expect(prompt_recovery_message(error.code)).toContain('unfinished result is preserved');
    expect(prompt_recovery_message('private upstream response')).toBeNull();
  });
  it('accepts a long multi-section source in the demo and labels its evidence', () => {
    const result = mock_prompt_command('prompt_enhance_run', { request: {
      stage: 'zh_expand', text: 'Implement the module, preserve compatibility and test its failure cases.\n'.repeat(350),
      target: { kind: 'gateway', model: 'route' },
    } }) as { complete: boolean; reviewStatus: string; output: string };
    expect(result.complete).toBe(true);
    expect(result.reviewStatus).toBe('demo_only');
    expect(result.output).toContain('preserve compatibility');
  });
});
