/// Usage and cost accounting IPC wrappers (`usage_report`, `usage_prune`).
/// The router records one row per answered request (`proxy::usage_recorder`);
/// these commands are the read surface. Every aggregate takes an explicit
/// time window (Unix epoch seconds, inclusive lower / exclusive upper) —
/// the caller owns the clock, so "last 7 days" and "this billing period"
/// are both expressible without the backend guessing at a calendar.
import { invoke } from '@tauri-apps/api/core';

export interface UsageSummary {
  totalRequests: number;
  totalPromptTokens: number;
  totalCompletionTokens: number;
  totalTokens: number;
  totalCostUsd: number;
  avgDurationMs: number;
}

export interface ModelUsage {
  model: string;
  requestCount: number;
  totalPromptTokens: number;
  totalCompletionTokens: number;
  totalTokens: number;
  totalCostUsd: number;
}

export interface ProviderUsage {
  providerId: string;
  requestCount: number;
  totalPromptTokens: number;
  totalCompletionTokens: number;
  totalTokens: number;
  totalCostUsd: number;
}

/** One snapshot for one window: headline totals plus the per-model and
 *  per-provider breakdowns the dashboard renders together (one command
 *  rather than three so the UI cannot show aggregates that disagree). */
export interface UsageReport {
  summary: UsageSummary;
  byModel: ModelUsage[];
  byProvider: ProviderUsage[];
  /** True when the window holds no records at all, so the UI can tell
   *  "no spend" from "never proxied anything". */
  isEmpty: boolean;
}

export interface UsageWindow {
  /** Inclusive lower bound (Unix epoch seconds). Omit for all history. */
  since?: number | null;
  /** Exclusive upper bound (Unix epoch seconds). Omit for open-ended. */
  until?: number | null;
  /** Restrict to one surface/app (e.g. "claude", "codex"). */
  appId?: string | null;
}

export async function usageReport(window: UsageWindow = {}): Promise<UsageReport> {
  return invoke<UsageReport>('usage_report', {
    since: window.since ?? null,
    until: window.until ?? null,
    appId: window.appId ?? null,
  });
}

/// Delete usage rows older than `beforeEpoch` (Unix epoch seconds).
/// Returns the number of rows removed so the UI can report what was
/// actually pruned instead of assuming the window was already clear.
export async function usagePrune(beforeEpoch: number): Promise<number> {
  return invoke<number>('usage_prune', { beforeEpoch });
}
