import type { RefObject } from 'react';
import { useCtx } from '../lib/ctx.js';
import { get_shell_copy } from '../lib/shell_i18n.js';
import { client_page_order, client_label } from '../lib/client_apps.js';
import type { Workspace_view } from '../lib/workspace_routes.js';
import { SunIcon, MoonIcon } from './Icons.js';
import './workspace_header.css';
import type { Gateway_readiness } from '../lib/gateway_readiness.js';

interface Workspace_header_props {
  view: Workspace_view;
  active_label: string;
  workspace_label: string;
  navigate: (view: Workspace_view) => void;
  gateway_status: 'running' | 'stopped' | 'degraded' | 'unknown';
  readiness?: Gateway_readiness | null;
  on_diagnostics: () => void;
  header_ref: RefObject<HTMLElement>;
}

export function Workspace_header({ view, active_label, workspace_label, navigate, gateway_status, readiness, on_diagnostics, header_ref }: Workspace_header_props) {
  const { t, locale, setLocale, theme, setThemeOverride } = useCtx();
  const shell_copy = get_shell_copy(locale);
  const gateway_label = t(`header.gateway_${gateway_status}`);
  const gateway_title = readiness
    ? `${t(`header.runtime_${readiness.runtime_kind}`)} · ${t(`header.readiness_${readiness.reason}`)}${readiness.available_model_count === null ? '' : ` · ${t('header.available_models', { count: String(readiness.available_model_count) })}`}`
    : t(`header.gateway_${gateway_status}_description`);

  return (
    <header className="aag-header workspace_header" ref={header_ref} data-testid="workspace_header" data-view={view}>
      <div className="workspace_context">
        <span>{workspace_label}</span>
        <span className="workspace_context_divider" aria-hidden>/</span>
        <label className="workspace_destination">
          <span className="sr_only">{shell_copy.navigation}</span>
          <select value={view} onChange={(event) => navigate(event.target.value as Workspace_view)} aria-label={shell_copy.navigation} data-testid="workspace_destination">
            <optgroup label={shell_copy.agents}>
              {client_page_order.map((app) => <option key={app} value={app}>{client_label(app)}</option>)}
            </optgroup>
            <optgroup label={shell_copy.supply}>
              <option value="suppliers">{shell_copy.suppliers}</option>
              <option value="usage">{t('usage.nav')}</option>
            </optgroup>
            <optgroup label={shell_copy.extensions}>
              <option value="prompt">{t('prompt.nav')}</option>
              <option value="mcp">MCP</option>
              <option value="dsh">DSH</option>
            </optgroup>
          </select>
        </label>
      </div>
      <span className="sr_only" role="status" aria-live="polite">{active_label}</span>
      <div className="header-controls">
        <span className={`gw-status ${gateway_status}`} title={gateway_title} role="status" aria-live="polite" data-testid="gateway_status">
          <span className="gw-status-dot" aria-hidden />
          {gateway_label}{readiness && <span className="gateway_runtime_detail" data-testid="gateway_readiness"> · {t(`header.runtime_${readiness.runtime_kind}`)} · {t(`header.readiness_${readiness.reason}`)}</span>}
        </span>
        <button className="diagnostics-btn" onClick={on_diagnostics} title={t('diagnostics.open')} aria-label={t('diagnostics.open')} data-testid="diagnostics_open">
          {t('diagnostics.open')}
        </button>
        <button className="theme-btn" onClick={() => setThemeOverride(theme === 'dark' ? 'light' : 'dark')} title={t('header.theme.toggle')} aria-label={t('header.theme.toggle')}>
          {theme === 'dark' ? <SunIcon /> : <MoonIcon />}
          <span>{theme === 'dark' ? t('header.theme.light') : t('header.theme.dark')}</span>
        </button>
        <button className="lang-btn" onClick={() => setLocale(locale === 'en' ? 'zh' : 'en')} title={t('header.lang.toggle')} aria-label={t('header.lang.toggle')}>
          {shell_copy.language}
        </button>
      </div>
    </header>
  );
}
