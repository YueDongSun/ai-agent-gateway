import { invoke } from '@tauri-apps/api/core';
import type { Model_arena_assessment, Model_assessment_source } from './suppliers_logic.js';
import type { Queue_health_projection } from './provider_failure_health.js';

export interface Arena_point {
  modelName: string; rating: number; ratingLower: number; ratingUpper: number; voteCount: number; rank: number;
}
export interface Arena_flow_model {
  model: string; tier: number | null; downgraded: boolean; basis: string; arena: Model_arena_assessment | null;
  provider?: string; protocol?: string | null; preferred?: boolean; pinned?: boolean;
  reason?: string;
  utility?: { total: number; quality: number; costEfficiency: number | null; freshness: number | null; suitability: number; referenceCost: number | null; metadataStale: boolean } | null;
  health?: { providerState: string | null; samples: number; successRate: number | null; latencyMs: number | null; unavailable: boolean };
  cost?: { inputPerMillion: number | null; outputPerMillion: number | null; source: string; stale: boolean } | null;
}
export interface Arena_snapshot {
  source: Model_assessment_source; sourceUrl: string; publishedDate: string | null; category: string;
  entries: Arena_point[]; selectedSelector: string | null; selected: Model_arena_assessment | null;
  observed: { model: string; appType: string; observedAt: number } | null;
  candidates: Arena_flow_model[]; fableCandidates: Arena_flow_model[];
  /** Read-only failover-queue health projection from the admitted runtime.
   *  `null` until a runtime preview is applied: no runtime means unmeasured
   *  health, never measured zeros. Optional so synthetic demo snapshots can
   *  omit it without fabricating observed data. */
  queueHealth?: Queue_health_projection | null;
  planError: string | null;
  configurationGeneration: number; planSource: 'admitted_runtime' | 'saved_configuration' | 'synthetic_demo'; synthetic?: boolean;
}
export type Arena_surface = 'claude' | 'codex' | 'chat' | 'gemini';
export interface Arena_catalog_view {
  source: string; fetchedAt: number | null; stale: boolean; lastError: string | null;
}

const arena_error_codes = new Set([
  'fetch_timeout', 'fetch_transport', 'fetch_http_error', 'fetch_decode_failed',
  'fetch_too_large', 'fetch_schema_invalid', 'arena_evidence_unavailable',
]);

/** Only safe service codes cross into visible refresh feedback. */
export function arena_refresh_error_code(error: unknown): string {
  const code = error instanceof Error ? error.message : error;
  return typeof code === 'string' && arena_error_codes.has(code) ? code : 'arena_evidence_unavailable';
}

export function get_arena_snapshot(selector: string | null, needs_tools: boolean, needs_images: boolean, surface = 'claude') {
  return invoke<Arena_snapshot>('supplier_arena_snapshot', { selector, needsTools: needs_tools, needsImages: needs_images, surface });
}

// Keep the existing public wrapper available while model-only views use the neutral name.
export { get_arena_snapshot as supplier_arena_snapshot };

export async function refresh_arena_evidence(only_if_stale = false): Promise<Arena_catalog_view> {
  const result = await invoke<Arena_catalog_view>('supplier_catalog_refresh', {
    input: { source: 'lmarena' }, ...(only_if_stale ? { onlyIfStale: true } : {}),
  });
  // Refresh failures are intentionally returned as stale source views by the
  // backend, so a fulfilled IPC promise alone is not a successful download.
  if (!result || result.source !== 'lmarena' || result.stale !== false || result.lastError !== null
      || typeof result.fetchedAt !== 'number' || !Number.isFinite(result.fetchedAt) || result.fetchedAt <= 0) {
    throw new Error(arena_refresh_error_code(result?.lastError));
  }
  return result;
}
