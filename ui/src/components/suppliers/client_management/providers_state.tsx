import { createContext, useContext } from 'react';
import type { AppPageConfig } from '../../../lib/appPages';
import type { ProxyHealth, TestResult } from '../../../lib/api/proxy.js';
import type { UnifiedCard } from '../../../lib/unified_providers.js';

export interface ProvidersDataState {
  /** Merged preset + upstream card list (AppPage-owned memo). */
  cards: UnifiedCard[];
  currentId: string | null;
  pinnedId: string | null;
  health: ProxyHealth | null;
  testResults: Record<string, TestResult>;
  testingId: string | null;
  /** Whether the gateway proxy is running (gates health/breaker badges). */
  running: boolean;
  /** In-flight command tag (e.g. `switch-<id>`), for button busy states. */
  busy: string | null;
  /** Model-discovery refresh in flight: provider id, `'all'`, or null. */
  refreshingModels: string | null;
  /** Page config: per-app form fields + cardUrl/cardMeta extraction. */
  cfg: AppPageConfig;
  /** Whether this page surfaces standalone pool rows (routing pages only).
   *  Gates both the pool-section header (with its add button) and the
   *  empty-state add button — a pool row created on a page that filters
   *  standalone cards out would never appear there. */
  pool_supported: boolean;
  breakerOf: (id: string) => string | null;
  breakerOfClass: (state: string | null) => string;
}

const ProvidersDataContext = createContext<ProvidersDataState | null>(null);

export function ProvidersDataProvider({
  value,
  children,
}: {
  value: ProvidersDataState;
  children: React.ReactNode;
}) {
  return (
    <ProvidersDataContext.Provider value={value}>
      {children}
    </ProvidersDataContext.Provider>
  );
}

export function useProvidersData(): ProvidersDataState {
  const ctx = useContext(ProvidersDataContext);
  if (!ctx) {
    throw new Error('useProvidersData must be used inside <ProvidersDataProvider>');
  }
  return ctx;
}

export interface ProvidersActionsState {
  /** `provider_switch` — set current + write live agent config. */
  onSwitch: (presetId: string) => void;
  /** `provider_set_failover_queue` — +chain / −chain membership. */
  onToggleQueue: (card: UnifiedCard) => void;
  /** `proxy_provider_save` (optimistic) — upstream enable/disable. */
  onToggleEnabled: (proxyId: string, enabled: boolean) => void;
  /** `proxy_set_pinned_provider` — force-pin / release. */
  onTogglePin: (proxyId: string) => void;
  /** `proxy_test_provider` — minimal connectivity probe. */
  onTestProvider: (proxyId: string) => void;
  onClearTestResult: (proxyId: string) => void;
  /** `proxy_refresh_models` — probe upstream catalogs and auto-apply
   *  newer same-family models. Provider id omitted = every eligible row. */
  onRefreshModels: (proxyId?: string) => void;
  /** `proxy_set_auto_track` — per-provider discovery opt-out. */
  onToggleAutoTrack: (proxyId: string, enabled: boolean) => void;
  /** Open the unified create/edit modal (card omitted = create). */
  onAdd: () => void;
  /** Open the modal in standalone-pool-row create mode (identity +
   *  upstream section only; saved straight to `proxy_providers`). */
  onAddStandalone: () => void;
  /** Open the modal prefilled from a provider template
   *  (`ui/src/lib/provider_presets.ts` catalog). */
  onAddFromTemplate: (templateId: string) => void;
  onEditCard: (card: UnifiedCard) => void;
  /** Deletes both sides of a linked card in one confirm. */
  onDeleteCard: (card: UnifiedCard) => void;
  /** Reorder preset cards (persisted via `provider_reorder`). */
  onReorder: (fromIdx: number, toIdx: number) => void;
  onRefresh: () => void;
}

const ProvidersActionsContext = createContext<ProvidersActionsState | null>(null);

export function ProvidersActionsProvider({
  value,
  children,
}: {
  value: ProvidersActionsState;
  children: React.ReactNode;
}) {
  return (
    <ProvidersActionsContext.Provider value={value}>
      {children}
    </ProvidersActionsContext.Provider>
  );
}

export function useProvidersActions(): ProvidersActionsState {
  const ctx = useContext(ProvidersActionsContext);
  if (!ctx) {
    throw new Error(
      'useProvidersActions must be used inside <ProvidersActionsProvider>',
    );
  }
  return ctx;
}
