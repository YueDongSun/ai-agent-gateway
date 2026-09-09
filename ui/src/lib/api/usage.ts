/// Usage tracking Tauri command wrappers.
/// Commands defined in `src-tauri/src/lib.rs` (usage_record, usage_summary, usage_by_model, usage_by_provider).
import { invoke } from '@tauri-apps/api/core';

export interface UsageRecordInput {
  ts: number;
  appId: string;
  providerId: string;
  model: string;
  promptTokens: number;
  completionTokens: number;
  costUsd: number;
  durationMs: number;
}

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

/// Time-window filter for the usage queries. All three are optional; `null`
/// means "no filter on that dimension".
export interface UsageFilter {
  since?: number | null;
  until?: number | null;
  appId?: string | null;
}

/// Record a single usage event. Returns the new row id.
/// (Called by the proxy after each request, or manually for testing.)
export async function recordUsage(record: UsageRecordInput): Promise<number> {
  return invoke<number>('usage_record', { record });
}

/// Aggregate usage summary over a time window + optional app filter.
export async function getUsageSummary(filter: UsageFilter = {}): Promise<UsageSummary> {
  return invoke<UsageSummary>('usage_summary', {
    since: filter.since ?? null,
    until: filter.until ?? null,
    appId: filter.appId ?? null,
  });
}

/// Per-model aggregate, sorted by total_tokens DESC.
export async function getUsageByModel(filter: UsageFilter = {}): Promise<ModelUsage[]> {
  return invoke<ModelUsage[]>('usage_by_model', {
    since: filter.since ?? null,
    until: filter.until ?? null,
    appId: filter.appId ?? null,
  });
}

/// Per-provider aggregate, sorted by total_tokens DESC.
export async function getUsageByProvider(filter: UsageFilter = {}): Promise<ProviderUsage[]> {
  return invoke<ProviderUsage[]>('usage_by_provider', {
    since: filter.since ?? null,
    until: filter.until ?? null,
    appId: filter.appId ?? null,
  });
}
