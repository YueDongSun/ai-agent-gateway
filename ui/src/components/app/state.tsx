import { createContext, useContext } from 'react';
import type { ProxyProvider, ProxyHealth, TestResult } from '../../lib/api/proxy';

/** ProxyPoolContext split (Phase 23d): parallel to the RoutesState
 *  data/actions split. The UpstreamPool and UpstreamList components
 *  are tree siblings under one provider; with the data + actions in
 *  separate contexts, a test-button state churn doesn't re-render
 *  the pool card and vice versa. */

// ---------------------------------------------------------------------------
// ProxyPoolDataContext — providers / pins / health / test results
// ---------------------------------------------------------------------------

export interface ProxyPoolDataState {
  providers: ProxyProvider[];
  pinnedId: string | null;
  health: ProxyHealth | null;
  testResults: Record<string, TestResult>;
  testingId: string | null;
  /** Helpers that derive from `health` — closure-only, no deps churn. */
  breakerOf: (id: string) => string | null;
  breakerOfClass: (state: string | null) => string;
}

const ProxyPoolDataContext = createContext<ProxyPoolDataState | null>(null);

export function ProxyPoolDataProvider({
  value,
  children,
}: {
  value: ProxyPoolDataState;
  children: React.ReactNode;
}) {
  return <ProxyPoolDataContext.Provider value={value}>{children}</ProxyPoolDataContext.Provider>;
}

export function useProxyPoolData(): ProxyPoolDataState {
  const ctx = useContext(ProxyPoolDataContext);
  if (!ctx) {
    throw new Error('useProxyPoolData must be used inside <ProxyPoolDataProvider>');
  }
  return ctx;
}

// ---------------------------------------------------------------------------
// ProxyPoolActionsContext — handlers; identity stable across renders.
// ---------------------------------------------------------------------------

export interface ProxyPoolActionsState {
  onTestProvider: (providerId: string) => void;
  onClearTestResult: (providerId: string) => void;
  onTogglePin: (providerId: string) => void;
  onToggleEnabled: (providerId: string, enabled: boolean) => void;
}

const ProxyPoolActionsContext = createContext<ProxyPoolActionsState | null>(null);

export function ProxyPoolActionsProvider({
  value,
  children,
}: {
  value: ProxyPoolActionsState;
  children: React.ReactNode;
}) {
  return <ProxyPoolActionsContext.Provider value={value}>{children}</ProxyPoolActionsContext.Provider>;
}

export function useProxyPoolActions(): ProxyPoolActionsState {
  const ctx = useContext(ProxyPoolActionsContext);
  if (!ctx) {
    throw new Error('useProxyPoolActions must be used inside <ProxyPoolActionsProvider>');
  }
  return ctx;
}

/** Combined shape - used by `ProxyPoolProvider` (the convenience
 *  wrapper that takes one merged value and feeds both contexts). */
export interface ProxyPoolState extends ProxyPoolDataState, ProxyPoolActionsState {}

export function ProxyPoolProvider({
  value,
  children,
}: {
  value: ProxyPoolState;
  children: React.ReactNode;
}) {
  return <ProxyPoolDataProvider value={value}><ProxyPoolActionsProvider value={value}>{children}</ProxyPoolActionsProvider></ProxyPoolDataProvider>;
}

// ---------------------------------------------------------------------------
// RoutesContext — the failover route editor's state slice.
// ---------------------------------------------------------------------------

import type { Route } from '../../lib/api/proxy';

/** RoutesContext split (Phase 23c): the original 22-field context
 *  re-rendered every consumer on any change, including action identity
 *  churn from inline `() => setRoutes(prev => ...)` closures. Splitting
 *  data and actions lets consumers subscribe to just one — a drag-tick
 *  doesn't re-render action-only consumers and vice versa. */

// ---------------------------------------------------------------------------
// RoutesDataContext — the part that changes with user actions.
// ---------------------------------------------------------------------------

export interface RoutesDataState {
  routes: Route[];
  filteredRoutes: Route[];
  isRouteDirty: boolean;
  selectedAliases: Set<string>;
  collapsedRoutes: Set<string>;
  knownModels: string[];
  fetchedModels: Record<string, string[]>;
  fetchingId: string | null;
  /** Drag state — changes on every pointermove tick. */
  routeOverIdx: { alias: string; idx: number } | null;
  pointerDrag: { alias: string; fromIdx: number } | null;
  /** Mini copy of proxy providers for the hop-provider <select>. */
  proxyProviders: { id: string; name: string }[];
  /** Per-tab route cache ref (owned by AppPage; pass-through for
   *  discard / tab-switch restore logic). */
  routeCacheRef: React.MutableRefObject<Record<string, Route[]>>;
}

const RoutesDataContext = createContext<RoutesDataState | null>(null);

export function RoutesDataProvider({
  value,
  children,
}: {
  value: RoutesDataState;
  children: React.ReactNode;
}) {
  return (
    <RoutesDataContext.Provider value={value}>
      {children}
    </RoutesDataContext.Provider>
  );
}

export function useRoutesData(): RoutesDataState {
  const ctx = useContext(RoutesDataContext);
  if (!ctx) {
    throw new Error('useRoutesData must be used inside <RoutesDataProvider>');
  }
  return ctx;
}

// ---------------------------------------------------------------------------
// RoutesActionsContext — handlers + meta; identity-stable across most renders
// (parent memoizes action objects).
// ---------------------------------------------------------------------------

export interface RoutesActionsState {
  toggleRouteSelection: (alias: string) => void;
  toggleRouteCollapse: (alias: string) => void;
  handleDeleteSelectedRoutes: () => void;
  handleAddRoute: () => void;
  handleDeleteRoute: (alias: string) => void;
  handleRenameRoute: (alias: string) => void;
  handleAddHop: (alias: string) => void;
  handleRemoveHop: (alias: string, hopIdx: number) => void;
  handleEditHop: (
    alias: string,
    hopIdx: number,
    field: 'provider' | 'model',
    value: string,
  ) => void;
  handleFetchModels: (providerId: string) => void;
  handleSaveRoutes: () => void;
  handleDiscardRoutes: () => void;
  // drag
  hopRowAt: (x: number, y: number) => { alias: string; idx: number } | null;
  handlePointerDown: (alias: string, fromIdx: number, e: React.PointerEvent) => void;
  handlePointerMove: (e: React.PointerEvent) => void;
  handlePointerUp: (e: React.PointerEvent) => void;
  handleHopKeyDown: (
    alias: string,
    idx: number,
    e: React.KeyboardEvent,
    hopCount: number,
  ) => void;
  // gateway restart
  onRestartProxy: () => void;
  // gateway status (the restart button is visible only when running)
  proxyRunning: boolean;
  restartBusy: boolean;
}

const RoutesActionsContext = createContext<RoutesActionsState | null>(null);

export function RoutesActionsProvider({
  value,
  children,
}: {
  value: RoutesActionsState;
  children: React.ReactNode;
}) {
  return (
    <RoutesActionsContext.Provider value={value}>
      {children}
    </RoutesActionsContext.Provider>
  );
}

export function useRoutesActions(): RoutesActionsState {
  const ctx = useContext(RoutesActionsContext);
  if (!ctx) {
    throw new Error(
      'useRoutesActions must be used inside <RoutesActionsProvider>',
    );
  }
  return ctx;
}

/** Combined shape - kept for type-compat with the AppPage memo
 *  signatures that predate the split. */
export type RoutesState = RoutesDataState & RoutesActionsState;
