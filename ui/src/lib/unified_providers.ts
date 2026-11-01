/// Unified provider-card data layer. Merges the two provider stores into
/// the single card list rendered per app page:
///
/// - presets: `providers` table rows (per-app settings snapshots that
///   `provider_switch` writes to live agent config files)
/// - upstreams: `proxy_providers` table rows (what the gateway actually
///   fails over across)
///
/// Linkage convention (written by the cc-switch importer and the schema
/// v17 backfill): preset `p` links to the upstream with id `ccs-<p.id>`.
/// Upstreams without a preset ("standalone", e.g. the seeded mock pool)
/// only render on routing pages, where routes reference them by id.
import type { AppType, Provider } from './api/types';
import type { ProxyProvider } from './api/proxy';

/** `proxy_providers.id` of the upstream linked to preset `presetId`. */
export const upstreamIdOf = (presetId: string): string => `ccs-${presetId}`;

/** Derive a provider id from its display name (same slug rule the
 *  legacy preset + upstream modals used). */
export const slugifyProviderId = (name: string): string =>
  name.trim().toLowerCase().replace(/\s+/g, '-');

/** Default upstream protocol for a new linked upstream: Anthropic wire
 *  shape on Claude-surface apps, OpenAI elsewhere. */
export function defaultUpstreamTypeFor(
  appType: AppType,
): 'anthropic' | 'openai' | 'gemini' {
  if (appType === 'claude_code' || appType === 'claude_vscode') return 'anthropic';
  if (appType === 'gemini') return 'gemini';
  return 'openai';
}

export interface UnifiedCard {
  /** React key + testid suffix: preset id when a preset exists, else the
   *  upstream id. */
  key: string;
  /** Per-app preset row (settings writer), or null for standalone
   *  upstream cards. */
  preset: Provider | null;
  /** Linked `ccs-*` upstream or the standalone pool row, or null when a
   *  preset has no upstream yet (self-loop presets, pre-import rows). */
  upstream: ProxyProvider | null;
  /** Upstream id — the key for health/test/pin/enable state. Null for
   *  preset-only cards. */
  proxyId: string | null;
  /** 1-based failover position among in-queue presets in list order;
   *  null when out of the queue or not a preset. */
  priority: number | null;
  /** True when the upstream row is referenced by a supplier endpoint
   *  projection: supplier saves own its contents, so direct edit/delete is
   *  hidden (the backend also refuses direct deletion). */
  supplierManaged: boolean;
  kind: 'preset' | 'standalone';
}

export interface MergeProviderCardsOptions {
  /** Render standalone upstreams after the presets (routing pages only). */
  includeStandalone: boolean;
  /** Proxy-provider ids referenced by supplier endpoint projections. */
  supplierMappedIds?: ReadonlySet<string>;
}

/** Build the unified card list: presets first in `provider_list` order
 *  (sort_index, created_at, id — i.e. the persisted chain order), each
 *  linked via the `ccs-` convention; standalone upstreams appended in
 *  `proxy_provider_list` order (weight DESC, id ASC). */
export function mergeProviderCards(
  presets: Provider[],
  upstreams: ProxyProvider[],
  opts: MergeProviderCardsOptions,
): UnifiedCard[] {
  const upstreamById = new Map(upstreams.map((u) => [u.id, u]));
  let priority = 0;
  const presetCards: UnifiedCard[] = presets.map((preset) => {
    // `inFailoverQueue` is serde-defaulted to true server-side; treat
    // undefined as in-queue.
    const inQueue = preset.inFailoverQueue !== false;
    if (inQueue) priority += 1;
    const upstream = upstreamById.get(upstreamIdOf(preset.id)) ?? null;
    return {
      key: preset.id,
      preset,
      upstream,
      proxyId: upstream?.id ?? null,
      priority: inQueue ? priority : null,
      supplierManaged: upstream
        ? (opts.supplierMappedIds?.has(upstream.id) ?? false)
        : false,
      kind: 'preset' as const,
    };
  });
  if (!opts.includeStandalone) return presetCards;

  const linkedIds = new Set(presets.map((p) => upstreamIdOf(p.id)));
  const standaloneCards: UnifiedCard[] = upstreams
    .filter((u) => !linkedIds.has(u.id))
    .map((u) => ({
      key: u.id,
      preset: null,
      upstream: u,
      proxyId: u.id,
      priority: null,
      supplierManaged: opts.supplierMappedIds?.has(u.id) ?? false,
      kind: 'standalone' as const,
    }));
  return [...presetCards, ...standaloneCards];
}
