/// Theme manager: reads/writes data-theme on <html>, persists override
/// in localStorage, and auto-detects system preference when no explicit
/// override exists.

import { useCallback, useEffect, useLayoutEffect, useState } from 'react';

export type Theme = 'light' | 'dark' | 'auto';

const STORAGE_KEY = 'aag.theme';

function detectSystem(): 'light' | 'dark' {
  if (typeof window === 'undefined') return 'dark';
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

function readOverride(): Theme {
  if (typeof window === 'undefined') return 'auto';
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY);
    if (stored === 'light' || stored === 'dark') return stored;
  } catch {
    /* ignore */
  }
  return 'auto';
}

function applyTheme(theme: 'light' | 'dark') {
  document.documentElement.setAttribute('data-theme', theme);
}

/**
 * Hook that returns the current resolved theme ('light' | 'dark'), the
 * user override preference ('light' | 'dark' | 'auto'), and a setter.
 * When override is 'auto' it follows the OS preference and reacts to
 * changes in real time. Setting it to 'light' or 'dark' forces that
 * choice and persists the override.
 */
export function useTheme() {
  const [override, setOverrideState] = useState<Theme>(readOverride);
  const [resolved, setResolved] = useState<'light' | 'dark'>(() =>
    override === 'auto' ? detectSystem() : override,
  );

  // Apply on mount and when resolved changes.
  useLayoutEffect(() => {
    applyTheme(resolved);
  }, [resolved]);

  // React to system preference changes when in 'auto' mode.
  useEffect(() => {
    if (override !== 'auto') return;
    const mq = window.matchMedia('(prefers-color-scheme: light)');
    const handler = (e: MediaQueryListEvent) => {
      const next = e.matches ? 'light' : 'dark';
      applyTheme(next);
      setResolved(next);
    };
    mq.addEventListener('change', handler);
    return () => mq.removeEventListener('change', handler);
  }, [override]);

  const setOverride = useCallback((next: Theme) => {
    setOverrideState(next);
    const resolved = next === 'auto' ? detectSystem() : next;
    applyTheme(resolved);
    setResolved(resolved);
    try {
      window.localStorage.setItem(STORAGE_KEY, next);
    } catch {
      /* ignore */
    }
  }, []);

  return { resolved, override, setOverride };
}