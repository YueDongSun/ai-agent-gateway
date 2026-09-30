/// i18n helper + locale store. Lightweight: no external dep, plain
/// objects, React hook for live locale switching. Locale is persisted in
/// localStorage; theme follows system preference with a manual override
/// (see useTheme).

import { useCallback, useEffect, useState } from 'react';
import { en, type MessageKey } from './en';
import { zh } from './zh';

export type Locale = 'en' | 'zh';

const STORAGE_KEY = 'aag.locale';

const catalogs: Record<Locale, Record<string, string>> = { en, zh };

/** Look up a message in the given locale, falling back to en, then to the key. */
export function t(locale: Locale, key: MessageKey | string, vars?: Record<string, string | number>): string {
  const template = catalogs[locale]?.[key] ?? catalogs.en[key as MessageKey] ?? String(key);
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (_, name) => {
    const v = vars[name];
    return v === undefined || v === null ? `{${name}}` : String(v);
  });
}

/** Read an explicit locale choice; new workspaces start in English. */
function detectInitialLocale(): Locale {
  if (typeof window === 'undefined') return 'en';
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY);
    if (stored === 'en' || stored === 'zh') return stored;
  } catch {
    /* ignore quota / disabled storage */
  }
  return 'en';
}

/**
 * Hook returning the current locale + a setter. Setting also persists to
 * localStorage and writes `lang` on <html> so CSS can react (e.g. longer
 * line-height for Chinese).
 */
export function useI18n(): { locale: Locale; setLocale: (l: Locale) => void; t: (key: MessageKey | string, vars?: Record<string, string | number>) => string } {
  const [locale, setLocaleState] = useState<Locale>(detectInitialLocale);

  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);

  const setLocale = useCallback((l: Locale) => {
    setLocaleState(l);
    try {
      window.localStorage.setItem(STORAGE_KEY, l);
    } catch {
      /* ignore */
    }
  }, []);

  const tt = useCallback(
    (key: MessageKey | string, vars?: Record<string, string | number>) => t(locale, key, vars),
    [locale],
  );

  return { locale, setLocale, t: tt };
}

export type { MessageKey };
