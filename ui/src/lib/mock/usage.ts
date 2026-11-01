/// Synthetic usage accounting for the browser demo. Native IPC never
/// dispatches here. In-memory rows only — the demo never opens the real
/// usage database. The aggregates mirror `database/dao/usage.rs` exactly
/// (summary + by-model + by-provider in one snapshot, window bounds
/// inclusive-lower / exclusive-upper) so the dashboard exercises the same
/// semantics it will see on native IPC. `usage_prune` mutates only this
/// demo list and reports the count, like the real command.
import type {
  ModelUsage,
  ProviderUsage,
  UsageReport,
  UsageWindow,
} from '../api/usage.js';

interface Demo_row {
  ts: number;
  appId: string;
  providerId: string;
  model: string;
  promptTokens: number;
  completionTokens: number;
  costUsd: number;
  durationMs: number;
}

/** Deterministic demo history: ~40 rows over the last 30 days so every
 *  window preset (24h / 7d / 30d / all) shows a plausible breakdown.
 *  Seeded from an arithmetic pattern rather than Math.random so repeated
 *  demo sessions render the same numbers. */
function seed_rows(now: number): Demo_row[] {
  const rows: Demo_row[] = [];
  const sources: Array<Pick<Demo_row, 'appId' | 'providerId' | 'model' | 'costUsd'>> = [
    { appId: 'claude', providerId: 'ccs-glm', model: 'glm-5.3', costUsd: 0.012 },
    { appId: 'claude', providerId: 'ccs-kimi', model: 'kimi-k3', costUsd: 0.009 },
    { appId: 'codex', providerId: 'ccs-deepseek', model: 'deepseek-chat', costUsd: 0.004 },
    { appId: 'chat', providerId: 'ccs-minimax', model: 'MiniMax-M3', costUsd: 0.007 },
  ];
  for (let i = 0; i < 40; i += 1) {
    const source = sources[i % sources.length];
    const promptTokens = 400 + ((i * 173) % 2200);
    const completionTokens = 120 + ((i * 89) % 900);
    // Spread across 30 days; the last few rows land inside the last day so
    // every window preset (24h / 7d / 30d / all) shows a plausible slice.
    const ts =
      i >= 37
        ? now - (i - 36) * 5_400
        : now - (30 - Math.floor(i * 0.7)) * 86_400 - ((i * 911) % 3_600);
    rows.push({
      ts,
      appId: source.appId,
      providerId: source.providerId,
      model: source.model,
      promptTokens,
      completionTokens,
      costUsd: source.costUsd,
      durationMs: 700 + ((i * 313) % 4_200),
    });
  }
  return rows.sort((a, b) => a.ts - b.ts);
}

let rows: Demo_row[] | null = null;

function demo_rows(): Demo_row[] {
  if (!rows) rows = seed_rows(Math.floor(Date.now() / 1000));
  return rows;
}

function in_window(row: Demo_row, window: UsageWindow): boolean {
  if (typeof window.since === 'number' && row.ts < window.since) return false;
  if (typeof window.until === 'number' && row.ts >= window.until) return false;
  if (window.appId && row.appId !== window.appId) return false;
  return true;
}

function report_for(window: UsageWindow): UsageReport {
  const selected = demo_rows().filter((row) => in_window(row, window));
  const by_model = new Map<string, ModelUsage>();
  const by_provider = new Map<string, ProviderUsage>();
  let prompt = 0;
  let completion = 0;
  let cost = 0;
  let duration = 0;
  for (const row of selected) {
    prompt += row.promptTokens;
    completion += row.completionTokens;
    cost += row.costUsd;
    duration += row.durationMs;
    const model = by_model.get(row.model) ?? {
      model: row.model,
      requestCount: 0,
      totalPromptTokens: 0,
      totalCompletionTokens: 0,
      totalTokens: 0,
      totalCostUsd: 0,
    };
    model.requestCount += 1;
    model.totalPromptTokens += row.promptTokens;
    model.totalCompletionTokens += row.completionTokens;
    model.totalTokens += row.promptTokens + row.completionTokens;
    model.totalCostUsd += row.costUsd;
    by_model.set(row.model, model);
    const provider = by_provider.get(row.providerId) ?? {
      providerId: row.providerId,
      requestCount: 0,
      totalPromptTokens: 0,
      totalCompletionTokens: 0,
      totalTokens: 0,
      totalCostUsd: 0,
    };
    provider.requestCount += 1;
    provider.totalPromptTokens += row.promptTokens;
    provider.totalCompletionTokens += row.completionTokens;
    provider.totalTokens += row.promptTokens + row.completionTokens;
    provider.totalCostUsd += row.costUsd;
    by_provider.set(row.providerId, provider);
  }
  return {
    summary: {
      totalRequests: selected.length,
      totalPromptTokens: prompt,
      totalCompletionTokens: completion,
      totalTokens: prompt + completion,
      totalCostUsd: cost,
      avgDurationMs: selected.length ? duration / selected.length : 0,
    },
    byModel: [...by_model.values()].sort((a, b) => b.totalCostUsd - a.totalCostUsd),
    byProvider: [...by_provider.values()].sort((a, b) => b.totalCostUsd - a.totalCostUsd),
    isEmpty: selected.length === 0,
  };
}

/** Dispatch one usage command. Unknown commands throw, matching the
 *  native contract that a missing command is a hard error. */
export function mock_usage_command(command: string, args: Record<string, unknown>): unknown {
  switch (command) {
    case 'usage_report': {
      const window: UsageWindow = {
        since: typeof args.since === 'number' ? args.since : null,
        until: typeof args.until === 'number' ? args.until : null,
        appId: typeof args.appId === 'string' && args.appId ? args.appId : null,
      };
      return report_for(window);
    }
    case 'usage_prune': {
      const before = typeof args.beforeEpoch === 'number' ? args.beforeEpoch : null;
      if (before === null) throw new Error('usage_prune_requires_before_epoch');
      const all = demo_rows();
      const kept = all.filter((row) => row.ts >= before);
      const removed = all.length - kept.length;
      rows = kept;
      return removed;
    }
    default:
      throw new Error(`demo_unhandled_command:${command}`);
  }
}
