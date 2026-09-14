import { lazy, Suspense, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { appCheckUpdate } from './lib/api/app';
import { I18nCtx, useCtx } from './lib/ctx';
import { useI18n } from './i18n/index';
import { useTheme } from './lib/theme';
import { APP_PAGE_ORDER, APP_PAGES } from './lib/appPages';
import type { AppType } from './lib/api/types';

interface AppInfo {
  name: string;
  version: string;
  phase: string;
}

const AppPage = lazy(() =>
  import('./components/app/AppPage').then((m) => ({ default: m.AppPage })),
);

/* ---- Sun/Moon icons (inline SVG - no asset dependency) ---- */

function SunIcon() {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <circle cx="12" cy="12" r="5" />
      <line x1="12" y1="1" x2="12" y2="3" />
      <line x1="12" y1="21" x2="12" y2="23" />
      <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
      <line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
      <line x1="1" y1="12" x2="3" y2="12" />
      <line x1="21" y1="12" x2="23" y2="12" />
      <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
      <line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
    </svg>
  );
}

function MoonIcon() {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
    </svg>
  );
}

function AppInner() {
  const ctx = useCtx();
  const { t, locale, setLocale, theme, setThemeOverride } = ctx;

  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<AppType>('claude_code');
  const [updateMsg, setUpdateMsg] = useState<string | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateEndpoint, setUpdateEndpoint] = useState('');

  useEffect(() => {
    invoke<AppInfo>('app_info')
      .then(setInfo)
      .catch((e) => setError(String(e)));
  }, []);

  const handleCheckUpdate = async () => {
    setCheckingUpdate(true);
    setUpdateMsg(null);
    try {
      const latest = await appCheckUpdate({
        endpoint: updateEndpoint.trim() || null,
      });
      if (latest) {
        setUpdateMsg(t('app.update.available', { version: latest }));
      } else {
        setUpdateMsg(t('app.update.none'));
      }
    } catch (e) {
      setUpdateMsg(t('app.update.failed', { message: String(e) }));
    } finally {
      setCheckingUpdate(false);
    }
  };

  const toggleTheme = () => {
    const next = theme === 'dark' ? 'light' : 'dark';
    setThemeOverride(next);
  };

  const nextLang = () => {
    setLocale(locale === 'en' ? 'zh' : 'en');
  };

  if (error) {
    return (
      <div className="aag-root">
        <pre className="aag-error">{t('app.error', { message: error })}</pre>
      </div>
    );
  }

  return (
    <div className="aag-root">
      <header className="aag-header">
        <div className="aag-brand">
          <h1>{info?.name ?? t('app.brand')}</h1>
          {info && (
            <span className="aag-sub">
              {t('app.subtitle', { version: info.version, phase: info.phase })}
            </span>
          )}
        </div>
        <nav className="app-switcher" aria-label="App switcher">
          {APP_PAGE_ORDER.map((at) => (
            <button
              key={at}
              className={view === at ? 'active' : ''}
              onClick={() => setView(at)}
            >
              {APP_PAGES[at].label}
            </button>
          ))}
        </nav>
        <div className="header-controls">
          <button
            className="theme-btn"
            onClick={toggleTheme}
            title={t('header.theme.toggle')}
            aria-label={t('header.theme.toggle')}
          >
            {theme === 'dark' ? <SunIcon /> : <MoonIcon />}
            <span>{theme === 'dark' ? t('header.theme.light') : t('header.theme.dark')}</span>
          </button>
          <button
            className="lang-btn"
            onClick={nextLang}
            title={t('header.lang.toggle')}
            aria-label={t('header.lang.toggle')}
          >
            {locale === 'en' ? '中' : 'EN'}
          </button>
        </div>
      </header>
      <main className="aag-main">
        <Suspense fallback={<section className="page-loading" aria-live="polite">{t('common.loading')}</section>}>
          {APP_PAGE_ORDER.includes(view) ? (
            <AppPage appType={view} />
          ) : (
            <section>
              <h2>{info?.name ?? t('app.brand')}</h2>
              <p>
                AAG is a local gateway for AI coding agents. Claude chains
                through it via cc-switch; Codex runs against it natively
                (Responses API) in an isolated mirror CODEX_HOME.
              </p>
              <div className="about-update">
                <input
                  type="text"
                  className="about-update-endpoint"
                  value={updateEndpoint}
                  onChange={(e) => setUpdateEndpoint(e.target.value)}
                  placeholder={t('app.update.placeholder')}
                  disabled={checkingUpdate}
                />
                <button
                  className="btn"
                  onClick={handleCheckUpdate}
                  disabled={checkingUpdate}
                >
                  {checkingUpdate ? t('app.update.checking') : t('app.update.check')}
                </button>
                {updateMsg && (
                  <span className="about-update-msg">{updateMsg}</span>
                )}
              </div>
            </section>
          )}
        </Suspense>
      </main>
      <footer className="aag-footer">{t('app.footer')}</footer>
    </div>
  );
}

export function App() {
  const i18n = useI18n();
  const { resolved, override, setOverride } = useTheme();

  const ctx = {
    ...i18n,
    theme: resolved,
    themeOverride: override,
    setThemeOverride: setOverride,
  };

  return (
    <I18nCtx.Provider value={ctx}>
      <AppInner />
    </I18nCtx.Provider>
  );
}
