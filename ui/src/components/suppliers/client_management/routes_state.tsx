import { createContext, useContext } from 'react';
import type { CircuitBreakerConfig, Route } from '../../../lib/api/proxy.js';

export interface RoutesDataState {
  routes: Route[];
  filteredRoutes: Route[];
  isRouteDirty: boolean;
  /** Gateway status needed by the route toolbar, separated from actions so
   *  state changes do not churn the stable action context. */
  proxy_running: boolean;
  restart_busy: boolean;
  /** A route save/discard/rename is in flight. Every draft mutation is locked
   *  until all submitted writes settle, including pointer and keyboard paths. */
  routes_saving: boolean;
  /** The submitted writes settled but persisted state could not be read yet. */
  outcome_unknown: boolean;
  selectedAliases: Set<string>;
  collapsedRoutes: Set<string>;
  knownModels: string[];
  fetchedModels: Record<string, string[]>;
  fetchingId: string | null;
  /** Proxy-provider details safe for route editing/explanation (never includes
   *  credentials or upstream URLs). */
  proxyProviders: {
    id: string;
    name: string;
    enabled: boolean;
    circuitBreaker: CircuitBreakerConfig;
    modelMapping: Record<string, string>;
  }[];
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

/** Narrow pointer-drag state, deliberately kept out of {@link
 *  RoutesDataState}: it changes on every pointermove tick, so the shared
 *  routes data value keeps a stable identity during a drag and only the rows
 *  that render drag classes subscribe to this context. Owned (state plus
 *  pointer handlers) by AppPage. */
export interface RoutesDragState {
  routeOverIdx: { alias: string; idx: number } | null;
  pointerDrag: { alias: string; fromIdx: number } | null;
}

const RoutesDragContext = createContext<RoutesDragState | null>(null);

export function RoutesDragProvider({
  value,
  children,
}: {
  value: RoutesDragState;
  children: React.ReactNode;
}) {
  return (
    <RoutesDragContext.Provider value={value}>
      {children}
    </RoutesDragContext.Provider>
  );
}

export function useRoutesDrag(): RoutesDragState {
  const ctx = useContext(RoutesDragContext);
  if (!ctx) {
    throw new Error(
      'useRoutesDrag must be used inside <RoutesDragProvider>',
    );
  }
  return ctx;
}

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
  /** Abort the drag without applying a move (touch scroll / palm / a second
   *  pointer cancelled the gesture). */
  handlePointerCancel: (e: React.PointerEvent) => void;
  handleHopKeyDown: (
    alias: string,
    idx: number,
    e: React.KeyboardEvent,
    hopCount: number,
  ) => void;
  // gateway restart
  onRestartProxy: () => void;
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
