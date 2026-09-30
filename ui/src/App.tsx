import { lazy, Suspense, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { appCheckUpdate } from './lib/api/app';
import { I18nCtx, useCtx } from './lib/ctx';
import { useI18n } from './i18n/index';
import { useTheme } from './lib/theme';
import { APP_PAGE_ORDER, APP_PAGES } from './lib/appPages';
import { get_shell_copy } from './lib/shell_i18n.js';
import { use_gateway_status } from './hooks/use_gateway_status.js';
import { app_version, build_channel } from './lib/build_info.js';
import { isDemo } from './lib/mock/backend.js';
import { Release_badge } from './components/release_badge.js';
import { Diagnostics_modal } from './components/diagnostics_modal.js';
import { Error_boundary } from './components/error_boundary.js';
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
  opencode: CopilotIcon,
  kilo_cli: CopilotIcon,
  claude_vscode: ClaudeIcon,
  kilo_vscode: CopilotIcon,
};

const Dsh_market = lazy(() => import('./components/dsh_market/market_page.js').then((module) => ({ default: () => <module.Dsh_market_boundary><module.Dsh_market_page /></module.Dsh_market_boundary> })));

const Dsh_page = lazy(() => import('./components/dsh/dsh_page.js').then((module) => ({ default: module.Dsh_page })));

const Mcp_page = lazy(() => import('./components/mcp/mcp_page.js').then((module) => ({ default: module.Mcp_page })));

const AppPage = lazy(() =>
  import('./components/app/AppPage').then((m) => ({ default: m.AppPage })),
);

/* Sun/Moon icons used to live here as inline SVG. Round 1 moved them to
 * ./components/Icons.tsx alongside the new ClaudeIcon/CodexIcon/CopilotIcon
 * so all UI icons share one home. */

function AppInner() {
  const ctx = useCtx();
  const { t, locale, setLocale, theme, setThemeOverride } = ctx;
  const shell_copy = get_shell_copy(locale);

  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<AppType | 'dsh' | 'mcp'>('claude_code');
  const navigation_ref = useRef<HTMLElement>(null);
  const [updateMsg, setUpdateMsg] = useState<string | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateEndpoint, setUpdateEndpoint] = useState('');
  const [show_diagnostics, set_show_diagnostics] = useState(false);
  const { gateway_status, refresh_gateway_status } = use_gateway_status();
  const release_channel = isDemo ? 'demo' : build_channel === 'demo' ? 'development' : build_channel;
  const workspace_label = view === 'dsh' ? shell_copy.runtime_workspace
    : view === 'mcp' ? shell_copy.tools_workspace : shell_copy.provider_workspace;
  const active_label = view === 'dsh' ? 'DSH' : view === 'mcp' ? 'MCP' : APP_PAGES[view].label;

  useEffect(() => {
    invoke<AppInfo>('app_info')
      .then(setInfo)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    const navigation = navigation_ref.current;
    if (!navigation) return;
    const reveal_active_navigation = () => {
      if (navigation.scrollWidth <= navigation.clientWidth) return;
      const active_button = navigation.querySelector<HTMLButtonElement>('button[aria-current="page"]');
      if (!active_button) return;
      const navigation_bounds = navigation.getBoundingClientRect();
      const button_bounds = active_button.getBoundingClientRect();
      if (button_bounds.left < navigation_bounds.left) {
        navigation.scrollLeft += button_bounds.left - navigation_bounds.left - 3;
      } else if (button_bounds.right > navigation_bounds.right) {
        navigation.scrollLeft += button_bounds.right - navigation_bounds.right + 3;
      }
    };
    reveal_active_navigation();
    const observer = new ResizeObserver(reveal_active_navigation);
    observer.observe(navigation);
    return () => observer.disconnect();
  }, [view]);

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
    <div className="aag-root workspace_shell">
      <a className="workspace_skip_link" href="#workspace_content">{shell_copy.skip_content}</a>
      <aside className="workspace_sidebar" data-testid="workspace_sidebar">
        <div className="aag-brand">
          <span className="aag-logo" aria-hidden>
            <BrandLogo />
          </span>
          <div className="workspace_brand_text">
            <h1>AAG</h1>
            <p>{info?.name ?? t('app.brand')}</p>
          </div>
        </div>
        <div className="workspace_release">
          <Release_badge
            version={app_version}
            channel={release_channel}
            label={t(`release.channel_${release_channel}`)}
            description={t(`release.description_${release_channel}`)}
            platform={info?.platform}
            architecture={info?.architecture}
          />
        </div>
        <nav className="app-switcher" aria-label={shell_copy.navigation} ref={navigation_ref}>
          <div className="workspace_nav_group">
            <h2 className="workspace_nav_label">{shell_copy.agents}</h2>
            {APP_PAGE_ORDER.map((at) => {
              const Icon = APP_ICONS[at];
              return (
                <button
                  key={at}
                  className={view === at ? 'active' : ''}
                  onClick={() => setView(at)}
                  aria-current={view === at ? 'page' : undefined}
                >
                  {Icon ? <Icon /> : null}
                  <span>{APP_PAGES[at].label}</span>
                </button>
              );
            })}
          </div>
          <div className="workspace_nav_group">
            <h2 className="workspace_nav_label">{shell_copy.extensions}</h2>
            <button className={view === 'mcp' ? 'active' : ''} onClick={() => setView('mcp')} data-testid="mcp_nav" aria-current={view === 'mcp' ? 'page' : undefined} title={shell_copy.mcp_description}>
              <svg viewBox="0 0 16 16" fill="none" aria-hidden><path d="M5 5V2m6 3V2M4 5h8v3a4 4 0 0 1-8 0V5Zm4 7v2" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
              <span>MCP</span>
            </button>
            <button className={view === 'dsh' ? 'active' : ''} onClick={() => setView('dsh')} data-testid="dsh_nav" aria-current={view === 'dsh' ? 'page' : undefined} title={shell_copy.dsh_description}>
              <svg viewBox="0 0 16 16" fill="none" aria-hidden><path d="M5 2H2v3m9-3h3v3M2 11v3h3m9-3v3h-3M5 5h6v6H5z" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
              <span>DSH</span>
            </button>
          </div>
        </nav>
        <div className="workspace_note" data-testid="workspace_scope">
          <span className="workspace_note_mark" aria-hidden>✳</span>
          <strong>{isDemo ? shell_copy.demo_workspace : shell_copy.local_workspace}</strong>
          <p>{isDemo ? shell_copy.demo_description : shell_copy.local_description}</p>
        </div>
      </aside>
      <header className="aag-header workspace_header">
        <div className="workspace_context">
          <span>{workspace_label}</span>
          <span className="workspace_context_divider" aria-hidden>/</span>
          <strong>{active_label}</strong>
        </div>
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
            className="diagnostics-btn"
            onClick={() => set_show_diagnostics(true)}
            title={t('diagnostics.open')}
            aria-label={t('diagnostics.open')}
            data-testid="diagnostics_open"
          >
            {t('diagnostics.open')}
          </button>
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
            {shell_copy.language}
          </button>
        </div>
      </header>
      <main className="aag-main" id="workspace_content" tabIndex={-1}>
        <Suspense fallback={<section className="page-loading" aria-live="polite">{t('common.loading')}</section>}>
          {view === 'dsh' ? <Dsh_page /> : view === 'mcp' ? <Mcp_page /> : APP_PAGE_ORDER.includes(view) ? (
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
      <Diagnostics_modal
        open={show_diagnostics}
        on_close={() => set_show_diagnostics(false)}
      />
    </div>
  );
}

function Main_app() {
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
      <Error_boundary>
        <AppInner />
      </Error_boundary>
    </I18nCtx.Provider>
  );
}

/** The dedicated marketplace has narrow IPC authority and no application shell. */
export function App() {
  const params = new URLSearchParams(window.location.search);
  if (params.get('view') === 'dsh_market') {
    return <Suspense fallback={<p role="status" style={{ padding: 30 }}>{params.get('locale') === 'zh' ? '正在载入插件市场…' : 'Loading plugin marketplace…'}</p>}><Dsh_market /></Suspense>;
  }
  return <Main_app />;
}
