/// Global i18n + theme context. Re-exported by pages via useContext.
import { createContext, useContext } from 'react';
import type { MessageKey } from '../i18n/en';
import type { Locale } from '../i18n/index';
import type { Theme } from './theme';

export interface I18nContextValue {
  locale: Locale;
  setLocale: (l: Locale) => void;
  t: (key: MessageKey | string, vars?: Record<string, string | number>) => string;
  theme: 'light' | 'dark';
  themeOverride: Theme;
  setThemeOverride: (t: Theme) => void;
}

export const I18nCtx = createContext<I18nContextValue>(null!);

export function useCtx(): I18nContextValue {
  return useContext(I18nCtx);
}

export function useT(): (key: MessageKey | string, vars?: Record<string, string | number>) => string {
  return useContext(I18nCtx).t;
}