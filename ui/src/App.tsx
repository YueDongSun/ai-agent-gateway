import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { appCheckUpdate } from './lib/api/app';
import { switchProvider } from './lib/api/providers';
import type { DeeplinkAction } from './lib/api/deeplink';
import { ProvidersPage } from './components/providers/ProvidersPage';
import { VendorsPage } from './components/vendors/VendorsPage';
import { ProxyPage } from './components/proxy/ProxyPage';
import { BackupPage } from './components/backup/BackupPage';
import { McpPage } from './components/mcp/McpPage';
import { SkillsPage } from './components/skills/SkillsPage';
import { SessionsPage } from './components/sessions/SessionsPage';
import { UsagePage } from './components/usage/UsagePage';
import { SyncPage } from './components/sync/SyncPage';
import { DeepLinkPage } from './components/deeplink/DeepLinkPage';
import { CodexCliPage } from './components/codexcli/CodexCliPage';
import { I18nCtx, useCtx } from './lib/ctx';
import { useI18n } from './i18n/index';
import { useTheme } from './lib/theme';

interface AppInfo {
  name: string;
  version: string;
  phase: string;
}

type View = 'app' | 'providers' | 'vendors' | 'proxy' | 'backup' | 'mcp' | 'skills' | 'sessions' | 'usage' | 'sync' | 'deeplink' | 'codexcli';

/* ---- Sun/Moon icons (inline SVG — no asset dependency) ---- */

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
  const { t, locale, setLocale, theme, themeOverride, setThemeOverride } = ctx;

  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<View>('providers');
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

  const handleExecuteDeeplink = async (
    action: DeeplinkAction,
  ): Promise<string> => {
    switch (action.kind) {
      case 'switchProvider': {
        const r = await switchProvider(action.providerId, action.appType);
        return t('deeplink.switchProvider', { name: action.providerId, count: r.written.length });
      }
      case 'focusProvider':
        setView('providers');
        return t('deeplink.focusProvider', { name: action.providerId });
      case 'focusMcp':
        setView('mcp');
        return t('deeplink.focusMcp', { name: action.serverId });
      case 'focusSkill':
        setView('skills');
        return t('deeplink.focusSkill', { name: action.skillId });
      case 'showDeeplinks':
        setView('deeplink');
        return t('deeplink.showDeeplinks');
      case 'unknown':
        return t('deeplink.unknown', { reason: action.reason });
    }
  };

  const nav = (key: string, viewId: View) => (
    <button
      key={viewId}
      className={view === viewId ? 'nav-item active' : 'nav-item'}
      onClick={() => setView(viewId)}
    >
      {t(key)}
    </button>
  );

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
        <div className="aag-header-row">
          <h1>{info?.name ?? t('app.brand')}</h1>
          <nav className="aag-nav">
            {nav('app.nav.providers', 'providers')}
            {nav('app.nav.vendors',   'vendors')}
            {nav('app.nav.proxy',       'proxy')}
            {nav('app.nav.backup',      'backup')}
            {nav('app.nav.mcp',         'mcp')}
            {nav('app.nav.skills',      'skills')}
            {nav('app.nav.sessions',    'sessions')}
            {nav('app.nav.usage',       'usage')}
            {nav('app.nav.sync',        'sync')}
            {nav('app.nav.deeplink',    'deeplink')}
            {nav('app.nav.codexcli',    'codexcli')}
            {nav('app.nav.about',       'app')}
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
        </div>
        {info && (
          <p className="aag-sub">
            {t('app.subtitle', { version: info.version, phase: info.phase })}
          </p>
        )}
      </header>
      <main className="aag-main">
        {view === 'providers' ? (
          <ProvidersPage />
        ) : view === 'vendors' ? (
          <VendorsPage />
        ) : view === 'proxy' ? (
          <ProxyPage />
        ) : view === 'backup' ? (
          <BackupPage />
        ) : view === 'mcp' ? (
          <McpPage />
        ) : view === 'skills' ? (
          <SkillsPage />
        ) : view === 'sessions' ? (
          <SessionsPage />
        ) : view === 'usage' ? (
          <UsagePage />
        ) : view === 'sync' ? (
          <SyncPage />
        ) : view === 'deeplink' ? (
          <DeepLinkPage onExecute={handleExecuteDeeplink} />
        ) : view === 'codexcli' ? (
          <CodexCliPage />
        ) : (
          <section>
            <h2>{t('app.nav.about')}</h2>
            <p>
              AAG ({t('app.brand')}) is a Tauri desktop app for managing provider
              configuration across multiple AI coding CLIs (Claude Code, Codex,
              Gemini CLI, OpenCode, OpenClaw, Hermes, Grok Build, Claude Desktop).
            </p>
            <p>
              See <code>DESIGN.md</code> for the migration plan and{' '}
              <code>PROGRESS.md</code> for current status.
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
      </main>
      <footer className="aag-footer">
        {t('app.footer')}
      </footer>
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