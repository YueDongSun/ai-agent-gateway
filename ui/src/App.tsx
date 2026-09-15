import { lazy, Suspense, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { appCheckUpdate } from './lib/api/app';
import { I18nCtx, useCtx } from './lib/ctx';
import { useI18n } from './i18n/index';
import { useTheme } from './lib/theme';
import { APP_PAGE_ORDER, APP_PAGES } from './lib/appPages';
import { getGatewayHealth } from './lib/api/proxy';
import { BrandLogo, ClaudeIcon, CodexIcon, CopilotIcon, SunIcon, MoonIcon } from './components/Icons';
import type { AppType } from './lib/api/types';

interface AppInfo {
  name: string;
  version: string;
  phase: string;
}

/** App-level icon picker for the AppSwitcher. Hidden apps (e.g. `pi`) get
 * no icon — the AppSwitcher already filters them out via APP_PAGE_ORDER. */
const APP_ICONS: Record<string, () => JSX.Element> = {
  claude_code: ClaudeIcon,
  codex: CodexIcon,
  copilot: CopilotIcon,
};

const AppPage = lazy(() =>
  import('./components/app/AppPage').then((m) => ({ default: m.AppPage })),
);

/* Sun/Moon icons used to live here as inline SVG. Round 1 moved them to
 * ./components/Icons.tsx alongside the new ClaudeIcon/CodexIcon/CopilotIcon
 * so all UI icons share one home. */

function AppInner() {
  const ctx = useCtx();
  const { t, locale, setLocale, theme, setThemeOverride } = ctx;

  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<AppType>('claude_code');
  const [updateMsg, setUpdateMsg] = useState<string | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateEndpoint, setUpdateEndpoint] = useState('');
  /** Round 1: global proxy status pill. Independent of the current app
   * tab — polled from the App shell so the pill stays mounted across tab
   * switches. 5 s interval is cheap (single fetch + parse). */
  const [gatewayStatus, setGatewayStatus] = useState<'stopped' | 'running' | 'degraded'>('stopped');

  useEffect(() => {
    invoke<AppInfo>('app_info')
      .then(setInfo)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const health = await getGatewayHealth();
        if (!alive) return;
        const openCount = health.providers.filter((p) => p.breaker?.state === 'open').length;
        setGatewayStatus(openCount > 0 ? 'degraded' : 'running');
      } catch {
        if (alive) setGatewayStatus('stopped');
      }
    };
    void tick();
    const id = setInterval(tick, 5000);
    return () => {
      alive = false;
      clearInterval(id);
    };
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
      <div className="aag-root aag-error-wrap">
        <pre className="aag-error">{t('app.error', { message: error })}</pre>
      </div>
    );
  }

  const gwLabel =
    gatewayStatus === 'running'  ? 'GATEWAY RUNNING'  :
    gatewayStatus === 'degraded' ? 'GATEWAY DEGRADED' :
                                   'GATEWAY STOPPED';
  const gwTitle =
    gatewayStatus === 'running'  ? 'Gateway running' :
    gatewayStatus === 'degraded' ? 'Gateway running, but at least one provider circuit is open' :
                                   'Gateway stopped (start it on the page below)';

  return (
    <div className="aag-root">
      <header className="aag-header">
        <div className="aag-brand">
          <span className="aag-logo" aria-hidden>
            <BrandLogo />
          </span>
          <h1>{info?.name ?? t('app.brand')}</h1>
          {info && (
            <span className="aag-sub">
              {t('app.subtitle', { version: info.version, phase: info.phase })}
            </span>
          )}
        </div>
        <nav className="app-switcher" aria-label="App switcher">
          {APP_PAGE_ORDER.map((at) => {
            const Icon = APP_ICONS[at];
            return (
              <button
                key={at}
                className={view === at ? 'active' : ''}
                onClick={() => setView(at)}
              >
                {Icon ? <Icon /> : null}
                <span>{APP_PAGES[at].label}</span>
              </button>
            );
          })}
        </nav>
        <div className="header-controls">
          {/* Round 1: live proxy status pill — single source of truth for
           * "is the gateway alive?". Polled every 5 s by the App shell. */}
          <span
            className={`gw-status ${gatewayStatus}`}
            title={gwTitle}
            role="status"
            aria-live="polite"
          >
            <span className="gw-status-dot" />
            {gwLabel}
          </span>
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
