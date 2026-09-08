import { lazy, Suspense, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { appCheckUpdate } from './lib/api/app';
import { I18nCtx, useCtx } from './lib/ctx';
import { useI18n } from './i18n/index';
import { useTheme } from './lib/theme';
import { APP_PAGE_ORDER, APP_PAGES } from './lib/appPages';
import { use_gateway_status } from './hooks/use_gateway_status.js';
import { app_version, build_channel } from './lib/build_info.js';
import { isDemo } from './lib/mock/backend.js';
import { Release_badge } from './components/release_badge.js';
import { BrandLogo, ClaudeIcon, CodexIcon, CopilotIcon, SunIcon, MoonIcon } from './components/Icons';
import type { AppType } from './lib/api/types';

interface AppInfo {
  name: string;
  version: string;
  phase: string;
  platform?: string;
  architecture?: string;
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
  const { gateway_status, refresh_gateway_status } = use_gateway_status();
  const release_channel = isDemo ? 'demo' : build_channel === 'demo' ? 'development' : build_channel;

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
      <div className="aag-root aag-error-wrap">
        <pre className="aag-error">{t('app.error', { message: error })}</pre>
      </div>
    );
  }

  const gwLabel =
    gateway_status === 'running'  ? t('header.gateway_running') :
    gateway_status === 'degraded' ? t('header.gateway_degraded') :
    gateway_status === 'stopped'  ? t('header.gateway_stopped') :
                                    t('header.gateway_unknown');
  const gwTitle =
    gateway_status === 'running'  ? t('header.gateway_running_description') :
    gateway_status === 'degraded' ? t('header.gateway_degraded_description') :
    gateway_status === 'stopped'  ? t('header.gateway_stopped_description') :
                                    t('header.gateway_unknown_description');

  return (
    <div className="aag-root">
      <header className="aag-header">
        <div className="aag-brand">
          <span className="aag-logo" aria-hidden>
            <BrandLogo />
          </span>
          <h1>{info?.name ?? t('app.brand')}</h1>
          <Release_badge
            version={app_version}
            channel={release_channel}
            label={t(`release.channel_${release_channel}`)}
            description={t(`release.description_${release_channel}`)}
            platform={info?.platform}
            architecture={info?.architecture}
          />
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
          <span
            className={`gw-status ${gateway_status}`}
            title={gwTitle}
            role="status"
            aria-live="polite"
            data-testid="gateway_status"
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
            <AppPage appType={view} on_gateway_status_refresh={refresh_gateway_status} />
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
