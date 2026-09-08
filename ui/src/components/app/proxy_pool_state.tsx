import { createContext, useContext } from 'react';
import type { ProxyProvider, ProxyHealth, TestResult } from '../../lib/api/proxy.js';

export interface ProxyPoolDataState {
  providers: ProxyProvider[];
  pinnedId: string | null;
  health: ProxyHealth | null;
  testResults: Record<string, TestResult>;
  testingId: string | null;
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
