import { createContext, useContext } from 'react';
import type { ProxyProvider, ProxyHealth, TestResult } from '../../lib/api/proxy';

/** Context for the proxy provider pool (UpstreamPool + UpstreamList both
 * consume this). AppPage owns the state and provides the value; the cards
 * never have to plumb 6+ props through their prop tree.
 *
 * Includes the local test-button state (testResults, testingId) and the
 * test/clear handlers - they only matter inside UpstreamPool but live
 * next to the related provider state so the context stays cohesive. */
export interface ProxyPoolState {
  providers: ProxyProvider[];
  pinnedId: string | null;
  health: ProxyHealth | null;
  testResults: Record<string, TestResult>;
  testingId: string | null;
  breakerOf: (id: string) => string | null;
  breakerOfClass: (state: string | null) => string;
  onTestProvider: (providerId: string) => void;
  onClearTestResult: (providerId: string) => void;
  onTogglePin: (providerId: string) => void;
  /** Flip a provider's enabled bit (persisted immediately; the router
   *  skips disabled hops, so this is the pool's on/off switch). */
  onToggleEnabled: (providerId: string, enabled: boolean) => void;
}

const ProxyPoolContext = createContext<ProxyPoolState | null>(null);

export function ProxyPoolProvider({
  value,
  children,
}: {
  value: ProxyPoolState;
  children: React.ReactNode;
}) {
  return <ProxyPoolContext.Provider value={value}>{children}</ProxyPoolContext.Provider>;
}

export function useProxyPool(): ProxyPoolState {
  const ctx = useContext(ProxyPoolContext);
  if (!ctx) {
    throw new Error('useProxyPool must be used inside <ProxyPoolProvider>');
  }
  return ctx;
}

// ---------------------------------------------------------------------------
// RoutesContext — the failover route editor's state slice.
// ---------------------------------------------------------------------------

import type { Route } from '../../lib/api/proxy';

export interface RoutesState {
  routes: Route[];
  isRouteDirty: boolean;
  filteredRoutes: Route[];
  selectedAliases: Set<string>;
  collapsedRoutes: Set<string>;
  knownModels: string[];
  fetchedModels: Record<string, string[]>;
  fetchingId: string | null;
  proxyProviders: { id: string; name: string }[];
  // actions
  toggleRouteSelection: (alias: string) => void;
  toggleRouteCollapse: (alias: string) => void;
  handleDeleteSelectedRoutes: () => void;
  handleAddRoute: () => void;
  handleDeleteRoute: (alias: string) => void;
  handleRenameRoute: (alias: string) => void;
  handleAddHop: (alias: string) => void;
  handleRemoveHop: (alias: string, hopIdx: number) => void;
  handleEditHop: (alias: string, hopIdx: number, field: 'provider' | 'model', value: string) => void;
  handleFetchModels: (providerId: string) => void;
  handleSaveRoutes: () => void;
  handleDiscardRoutes: () => void;
  // drag
  hopRowAt: (x: number, y: number) => { alias: string; idx: number } | null;
  handlePointerDown: (alias: string, fromIdx: number, e: React.PointerEvent) => void;
  handlePointerMove: (e: React.PointerEvent) => void;
  handlePointerUp: (e: React.PointerEvent) => void;
  /** Keyboard reorder on the focused drag handle (↑/↓/Home/End). */
  handleHopKeyDown: (
    alias: string,
    idx: number,
    e: React.KeyboardEvent,
    hopCount: number,
  ) => void;
  routeOverIdx: { alias: string; idx: number } | null;
  pointerDrag: { alias: string; fromIdx: number } | null;
  // gateway restart (routes only reload at proxy start)
  proxyRunning: boolean;
  restartBusy: boolean;
  onRestartProxy: () => void;
  // legacy ref passthrough for cache-on-tab-switch (owned by AppPage)
  routeCacheRef: React.MutableRefObject<Record<string, Route[]>>;
}

const RoutesContext = createContext<RoutesState | null>(null);

export function RoutesProvider({ value, children }: { value: RoutesState; children: React.ReactNode }) {
  return <RoutesContext.Provider value={value}>{children}</RoutesContext.Provider>;
}

export function useRoutes(): RoutesState {
  const ctx = useContext(RoutesContext);
  if (!ctx) {
    throw new Error('useRoutes must be used inside <RoutesProvider>');
  }
  return ctx;
}
