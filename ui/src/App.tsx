import { lazy, Suspense, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { appCheckUpdate } from './lib/api/app';
import { I18nCtx, useCtx } from './lib/ctx';
import { useI18n } from './i18n/index';
import { useTheme } from './lib/theme';
import { use_workspace_navigation } from './hooks/use_workspace_navigation.js';
import { use_status_surface } from './hooks/use_status_surface.js';
import { Workspace_header } from './components/workspace_header.js';
import { Page_actions_provider, Floating_page_actions } from './components/page_actions.js';
import { client_page_order, client_label } from './lib/client_apps.js';
import { get_shell_copy } from './lib/shell_i18n.js';
import { use_gateway_status } from './hooks/use_gateway_status.js';
import { app_version, build_channel } from './lib/build_info.js';
import { isDemo } from './lib/mock/backend.js';
import { Release_badge } from './components/release_badge.js';
import { Diagnostics_modal } from './components/diagnostics_modal.js';
import { Error_boundary } from './components/error_boundary.js';
import {
  BrandLogo,
  ClaudeIcon,
  CodexIcon,
  CopilotIcon,
  gemini_icon,
  kilo_icon,
  cline_icon,
  PiIcon,
} from './components/Icons';

interface AppInfo {
  name: string;
  version: string;
  phase: string;
  platform?: string;
  architecture?: string;
}

/** App-level icon picker for the AppSwitcher. Hidden apps (e.g. `pi`) get
 * no icon — the AppSwitcher already filters them out via client_page_order. */
const APP_ICONS: Record<string, () => JSX.Element> = {
  claude_code: ClaudeIcon,
  codex: CodexIcon,
  copilot: CopilotIcon,
  gemini: gemini_icon,
  pi: PiIcon,
  opencode: CopilotIcon,
  kilo_cli: kilo_icon,
  cline_cli: cline_icon,
  claude_vscode: ClaudeIcon,
  kilo_vscode: CopilotIcon,
};

const Dsh_market = lazy(() => import('./components/dsh_market/market_page.js').then((module) => ({ default: () => <module.Dsh_market_boundary><module.Dsh_market_page /></module.Dsh_market_boundary> })));

const Dsh_page = lazy(() => import('./components/dsh/dsh_page.js').then((module) => ({ default: module.Dsh_page })));

const Mcp_page = lazy(() => import('./components/mcp/mcp_page.js').then((module) => ({ default: module.Mcp_page })));

const AppPage = lazy(() =>
  import('./components/app/AppPage').then((m) => ({ default: m.AppPage })),
);

const Prompt_page = lazy(() =>
  import('./components/prompt/prompt_page').then((m) => ({ default: m.Prompt_page })),
);

const Suppliers_page = lazy(() =>
  import('./components/suppliers/suppliers_page.js').then((m) => ({ default: m.Suppliers_page })),
);

const Usage_page = lazy(() =>
  import('./components/usage/usage_page.js').then((m) => ({ default: m.Usage_page })),
);

/* Sun/Moon icons used to live here as inline SVG. Round 1 moved them to
 * ./components/Icons.tsx alongside the new ClaudeIcon/CodexIcon/CopilotIcon
 * so all UI icons share one home. */

function AppInner() {
  const ctx = useCtx();
  const { t, locale, theme } = ctx;
  const shell_copy = get_shell_copy(locale);

  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const main_ref = useRef<HTMLElement>(null);
  const header_ref = useRef<HTMLElement>(null);
  const { view, navigate } = use_workspace_navigation(main_ref);
  use_status_surface(header_ref, main_ref, view, theme);
  const navigation_ref = useRef<HTMLElement>(null);
  const [updateMsg, setUpdateMsg] = useState<string | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateEndpoint, setUpdateEndpoint] = useState('');
  const [show_diagnostics, set_show_diagnostics] = useState(false);
  const { gateway_status, readiness, refresh_gateway_status } = use_gateway_status();
  const release_channel = isDemo ? 'demo' : build_channel === 'demo' ? 'development' : build_channel;
  const workspace_label = view === 'dsh' ? shell_copy.runtime_workspace
    : view === 'mcp' || view === 'prompt' ? shell_copy.tools_workspace
    : view === 'suppliers' || view === 'usage' ? shell_copy.provider_workspace : shell_copy.client_workspace;
  const active_label = view === 'dsh' ? 'DSH' : view === 'mcp' ? 'MCP'
    : view === 'prompt' ? t('prompt.nav') : view === 'suppliers' ? shell_copy.suppliers
    : view === 'usage' ? t('usage.nav') : client_label(view);

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

  if (error) {
    return (
      <div className="aag-root aag-error-wrap">
        <pre className="aag-error">{t('app.error', { message: error })}</pre>
      </div>
    );
  }

  return (
    <Page_actions_provider scope={view}>
    <div className="aag-root workspace_shell">
      <a className="workspace_skip_link" href="#workspace_content" onClick={(event) => { event.preventDefault(); main_ref.current?.focus(); main_ref.current?.scrollIntoView({ block: 'start' }); }}>{shell_copy.skip_content}</a>
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
            {client_page_order.map((at) => {
              const Icon = APP_ICONS[at];
              return (
                <button
                  key={at}
                  data-testid={`app-tab-${at}`}
                  data-app={at}
                  className={view === at ? 'active' : ''}
                  onClick={() => navigate(at)}
                  aria-current={view === at ? 'page' : undefined}
                >
                  {Icon ? Icon() : null}
                  <span>{client_label(at)}</span>
                </button>
              );
            })}
          </div>
          <div className="workspace_nav_group">
            <h2 className="workspace_nav_label">{shell_copy.supply}</h2>
            <button className={view === 'suppliers' ? 'active' : ''} onClick={() => navigate('suppliers')} data-testid="suppliers_nav" aria-current={view === 'suppliers' ? 'page' : undefined} title={shell_copy.suppliers_description}>
              <svg viewBox="0 0 16 16" fill="none" aria-hidden><path d="M8 1.5 14 5v6L8 14.5 2 11V5l6-3.5Z M2 5l6 3.5L14 5 M8 8.5v6" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" /></svg>
              <span>{shell_copy.suppliers}</span>
            </button>
            <button className={view === 'usage' ? 'active' : ''} onClick={() => navigate('usage')} data-testid="usage_nav" aria-current={view === 'usage' ? 'page' : undefined} title={t('usage.nav.help')}>
              <svg viewBox="0 0 16 16" fill="none" aria-hidden><path d="M2 13.5h12M3.5 13.5V9m3 4.5V6.5m3 7V8m3 5.5V4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" /></svg>
              <span>{t('usage.nav')}</span>
            </button>
          </div>
          <div className="workspace_nav_group">
            <h2 className="workspace_nav_label">{shell_copy.extensions}</h2>
            <button className={view === 'prompt' ? 'active' : ''} onClick={() => navigate('prompt')} data-testid="prompt_nav" aria-current={view === 'prompt' ? 'page' : undefined} title={shell_copy.prompt_description}>
              <svg viewBox="0 0 16 16" fill="none" aria-hidden><path d="M2 3h12M2 8h8.5M2 13h5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" /></svg>
              <span>{t('prompt.nav')}</span>
            </button>
            <button className={view === 'mcp' ? 'active' : ''} onClick={() => navigate('mcp')} data-testid="mcp_nav" aria-current={view === 'mcp' ? 'page' : undefined} title={shell_copy.mcp_description}>
              <svg viewBox="0 0 16 16" fill="none" aria-hidden><path d="M5 5V2m6 3V2M4 5h8v3a4 4 0 0 1-8 0V5Zm4 7v2" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
              <span>MCP</span>
            </button>
            <button className={view === 'dsh' ? 'active' : ''} onClick={() => navigate('dsh')} data-testid="dsh_nav" aria-current={view === 'dsh' ? 'page' : undefined} title={shell_copy.dsh_description}>
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
      <Workspace_header
        view={view} active_label={active_label} workspace_label={workspace_label}
        navigate={navigate} gateway_status={gateway_status} readiness={readiness}
        on_diagnostics={() => set_show_diagnostics(true)} header_ref={header_ref}
      />
      <main className="aag-main" id="workspace_content" tabIndex={-1} ref={main_ref} aria-label={active_label}>
        <Suspense fallback={<section className="page-loading" aria-live="polite">{t('common.loading')}</section>}>
          <div key={view} className="workspace_page">
          {view === 'dsh' ? <Dsh_page /> : view === 'mcp' ? <Mcp_page /> : view === 'prompt' ? <Prompt_page /> : view === 'suppliers' ? <Suppliers_page on_gateway_status_refresh={refresh_gateway_status} /> : view === 'usage' ? <Usage_page /> : client_page_order.includes(view) ? (
            <AppPage key={view} appType={view} />
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
          </div>
        </Suspense>
      </main>
      <Floating_page_actions />
      <footer className="aag-footer">{t('app.footer')}</footer>
      <Diagnostics_modal
        open={show_diagnostics}
        on_close={() => set_show_diagnostics(false)}
      />
    </div>
    </Page_actions_provider>
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
