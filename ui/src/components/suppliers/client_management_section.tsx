import { lazy, Suspense, useId, useRef, useState } from 'react';
import { APP_PAGE_ORDER, APP_PAGES } from '../../lib/appPages.js';
import type { AppType } from '../../lib/api/types.js';
import { switchAllProviders } from '../../lib/api/providers.js';
import { useCtx } from '../../lib/ctx.js';
import { request_workspace_navigation, use_navigation_block } from '../../hooks/use_navigation_block.js';
import './client_management_section.css';

const Supplier_client_settings = lazy(() =>
  import('./client_management/supplier_client_settings.js').then((module) => ({
    default: module.Supplier_client_settings,
  })),
);

const client_management_copy = {
  en: {
    title: 'Client connections & routing',
    description: 'Manage providers, gateway settings, routes, and client connections in one place.',
    choose_client: 'Choose a client to manage',
    apply_all: 'Apply current providers to all clients',
    applying: 'Applying…',
    applied: 'applied',
    no_current: 'no selection',
    failed: 'failed',
  },
  zh: {
    title: '客户端连接与路由',
    description: '统一管理供应商、网关设置、路由和客户端连接。',
    choose_client: '选择要管理的客户端',
    apply_all: '一键填充全部客户端',
    applying: '正在填充…',
    applied: '已应用',
    no_current: '未选择',
    failed: '失败',
  },
} as const;

interface Client_management_section_props {
  on_gateway_status_refresh?: () => Promise<void>;
}

/** Supply management is mounted only after an explicit disclosure. A client
 * change remounts its controller, retaining the established async boundaries. */
export function Client_management_section({ on_gateway_status_refresh }: Client_management_section_props) {
  const { t, locale } = useCtx();
  const copy = client_management_copy[locale];
  const [expanded, set_expanded] = useState(false);
  const [app_type, set_app_type] = useState<AppType>('claude_code');
  const [apply_all_state, set_apply_all_state] = useState<{
    running: boolean;
    summary: string | null;
  }>({ running: false, summary: null });
  const switcher_ref = useRef<HTMLDivElement>(null);
  const panel_id = useId();
  const heading_id = useId();
  use_navigation_block(apply_all_state.running);

  const select_client = (next_app_type: AppType, button: HTMLButtonElement) => {
    if (next_app_type !== app_type && !request_workspace_navigation()) return;
    set_app_type(next_app_type);
    const switcher = switcher_ref.current;
    if (!switcher) return;
    const bounds = switcher.getBoundingClientRect();
    const button_bounds = button.getBoundingClientRect();
    if (button_bounds.left < bounds.left) switcher.scrollLeft += button_bounds.left - bounds.left;
    else if (button_bounds.right > bounds.right) switcher.scrollLeft += button_bounds.right - bounds.right;
  };

  const apply_all = async () => {
    set_apply_all_state({ running: true, summary: null });
    try {
      const entries = await switchAllProviders();
      const applied = entries.filter((entry) => entry.outcome === 'applied').length;
      const no_current = entries.filter((entry) => entry.outcome === 'no_current').length;
      const failed = entries.filter((entry) => entry.outcome === 'error');
      const summary = [
        `${applied} ${copy.applied}`,
        `${no_current} ${copy.no_current}`,
        ...(failed.length
          ? [
              `${failed.length} ${copy.failed}: ${failed
                .map((entry) => `${APP_PAGES[entry.app_type as AppType]?.label ?? entry.app_type} (${entry.detail ?? '?'})`)
                .join('; ')}`,
            ]
          : []),
      ].join(' · ');
      set_apply_all_state({ running: false, summary });
      await on_gateway_status_refresh?.();
    } catch (error) {
      set_apply_all_state({ running: false, summary: String(error) });
    }
  };

  return (
    <section className="client_management_section" aria-labelledby={heading_id} data-testid="client-management-section">
      <button
        type="button"
        className="client_management_toggle"
        id={heading_id}
        aria-expanded={expanded}
        aria-controls={panel_id}
        data-testid="client-management-toggle"
        onClick={() => { if (!expanded || request_workspace_navigation()) set_expanded((value) => !value); }}
      >
        <span>
          <strong>{copy.title}</strong>
          <span>{copy.description}</span>
        </span>
        <span className="client_management_indicator" aria-hidden="true">{expanded ? '−' : '+'}</span>
      </button>
      <div id={panel_id} hidden={!expanded} className="client_management_body">
        {expanded && (
          <>
            <div className="client_management_switcher" role="group" aria-label={copy.choose_client} ref={switcher_ref}>
              {APP_PAGE_ORDER.map((client) => (
                <button
                  type="button"
                  key={client}
                  className={app_type === client ? 'active' : ''}
                  data-testid={`client-settings-tab-${client}`}
                  aria-pressed={app_type === client}
                  onClick={(event) => select_client(client, event.currentTarget)}
                >
                  {APP_PAGES[client].label}
                </button>
              ))}
            </div>
            <div className="client_management_apply_all">
              <button
                type="button"
                className="btn"
                disabled={apply_all_state.running}
                data-testid="client-management-apply-all"
                onClick={apply_all}
              >
                {apply_all_state.running ? copy.applying : copy.apply_all}
              </button>
              {apply_all_state.summary && (
                <span role="status" className="client_management_apply_all_summary" data-testid="client-management-apply-all-summary">
                  {apply_all_state.summary}
                </span>
              )}
            </div>
            <Suspense fallback={<p role="status" className="page-loading">{t('common.loading')}</p>}>
              <Supplier_client_settings key={app_type} appType={app_type} on_gateway_status_refresh={on_gateway_status_refresh} />
            </Suspense>
          </>
        )}
      </div>
    </section>
  );
}
