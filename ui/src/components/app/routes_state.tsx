import { createContext, useContext } from 'react';
import type { Route } from '../../lib/api/proxy.js';

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
