import type { Screening_overview, Screening_policy, Screening_recheck_receipt } from '../api/provider_screening.js';

const DEMO_POLICY: Screening_policy = { max_latency_ms: 5000, probe_deadline_ms: 15000, max_output_tokens: 64, max_response_chars: 1024 };
const demo_entries: Screening_overview['entries'] = [
  { providerId: 'fixture_screened', name: 'Screened connection (demo)', admittedModels: ['fixture_model', 'fixture_model_fast'], subscriptionState: 'active', subscriptionReason: 'active', stage: 'admission', reason: 'accepted', probeModel: 'fixture_model', checkedAt: 1793188800, latencyMs: 800, response: 'Synthetic browser demo response: ok', responseTruncated: false, responseRedacted: false },
  { providerId: 'fixture_expired', name: 'Expired connection (demo)', admittedModels: [], subscriptionState: 'inactive', subscriptionReason: 'subscription_expired', stage: 'subscription', reason: 'subscription_expired', probeModel: null, checkedAt: 1793188800, latencyMs: null, response: null, responseTruncated: false, responseRedacted: false },
];

/** Synthetic, memory-only presentation data. No provider or subscription calls. */
export function mock_provider_screening_command(command: 'provider_screening_overview', args: Record<string, unknown>, stored_policy?: string): Screening_overview;
export function mock_provider_screening_command(command: 'provider_screening_recheck', args: Record<string, unknown>, stored_policy?: string): Screening_recheck_receipt;
export function mock_provider_screening_command(command: string, args: Record<string, unknown>, stored_policy?: string): Screening_overview | Screening_recheck_receipt;
export function mock_provider_screening_command(command: string, args: Record<string, unknown>, stored_policy?: string): Screening_overview | Screening_recheck_receipt {
  let policy = DEMO_POLICY;
  if (stored_policy) policy = JSON.parse(stored_policy) as Screening_policy;
  const entries = structuredClone(demo_entries);
  if (command === 'provider_screening_recheck') {
    const entry = entries.find((item) => item.providerId === args.providerId);
    if (!entry) throw new Error('provider_admission_source_missing');
    entry.checkedAt = Math.floor(Date.now() / 1000);
    return {
      providerId: entry.providerId, checkStarted: true, eligible: entry.admittedModels.length > 0,
      checkError: null,
      changes: { policyChanged: false, registrationChanged: false, admissionCommitted: true, sourceCommitted: true },
      admissionVersion: 1, runtimeReconciled: true, runtimeError: null,
      overview: { enforced: true, policy: { ...policy }, entries }, overviewError: null,
    };
  } else if (command !== 'provider_screening_overview') throw new Error('demo_command_unsupported');
  return { enforced: true, policy: { ...policy }, entries };
}
