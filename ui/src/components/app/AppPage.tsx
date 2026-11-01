import { useEffect, useMemo, useRef, useState } from 'react';
import { useCtx } from '../../lib/ctx.js';
import { client_label } from '../../lib/client_apps.js';
import { get_current_client_plan, type Client_router_plan } from '../../lib/api/client_router_plan.js';
import { client_arena_context } from '../../lib/client_arena_context.js';
import type { AppType } from '../../lib/api/types.js';
import { Arena_chart } from '../arena/arena_chart.js';
import './client_plan.css';

const plan_refresh_ms = 15_000;

const plan_copy = {
  en: {
    description: 'Router models from this client’s selected configuration.',
    title: 'Current plan', selected: 'Selected model', fable: 'Fable', opus: 'Opus', sonnet: 'Sonnet', haiku: 'Haiku',
    empty: 'No router model is configured for the current plan.',
    error: 'The current plan could not be read. Refresh to try again.',
    arena_title: 'Arena.ai · model position',
    arena_loading: 'Loading the current plan before comparing models…',
    arena_error: 'Arena comparison is unavailable until the current plan can be read.',
    arena_empty: 'No model is configured for this agent. Configure one in Suppliers to compare Arena.ai results.',
  },
  zh: {
    description: '此客户端当前所选配置中的路由模型。',
    title: '当前方案', selected: '所选模型', fable: 'Fable', opus: 'Opus', sonnet: 'Sonnet', haiku: 'Haiku',
    empty: '当前方案尚未配置路由模型。',
    error: '无法读取当前方案，请刷新重试。',
    arena_title: 'Arena.ai · 模型位置',
    arena_loading: '正在读取当前方案以比较模型…',
    arena_error: '读取当前方案后才能比较 Arena 排名。',
    arena_empty: '此客户端尚未配置模型，请先在供应商页面配置模型以比较 Arena.ai 结果。',
  },
} as const;

/** Client destinations consume only model selections. Management lives in its
 * own destination and cannot become a conditional branch of this component. */
export function AppPage({ appType }: { appType: AppType }) {
  const { t, locale } = useCtx();
  const copy = plan_copy[locale];
  const [plan, set_plan] = useState<Client_router_plan | null>(null);
  const [failed, set_failed] = useState(false);
  const [refresh_version, set_refresh_version] = useState(0);
  const pending_plans = useRef(new Map<AppType, Promise<Client_router_plan>>());

  useEffect(() => {
    let active = true;
    let in_flight = false;
    set_plan(null);
    set_failed(false);
    const load = async () => {
      if (in_flight || document.visibilityState === 'hidden') return;
      in_flight = true;
      let request = pending_plans.current.get(appType);
      if (!request) {
        request = get_current_client_plan(appType);
        pending_plans.current.set(appType, request);
      }
      try {
        const value = await request;
        if (active) {
          set_plan((previous) => JSON.stringify(previous) === JSON.stringify(value) ? previous : value);
          set_failed(false);
        }
      } catch { if (active) { set_plan(null); set_failed(true); } }
      finally {
        if (pending_plans.current.get(appType) === request) pending_plans.current.delete(appType);
        in_flight = false;
      }
    };
    void load();
    const timer = window.setInterval(load, plan_refresh_ms);
    window.addEventListener('focus', load);
    document.addEventListener('visibilitychange', load);
    return () => {
      active = false;
      clearInterval(timer);
      window.removeEventListener('focus', load);
      document.removeEventListener('visibilitychange', load);
    };
  }, [appType, refresh_version]);

  const current_plan = plan?.app_type === appType ? plan : null;
  const loading = !failed && !current_plan;
  const arena_context = useMemo(() => client_arena_context(appType, current_plan?.models ?? []), [appType, current_plan]);
  return (
    <section className="page client_plan_page" data-testid="cli-router-page" data-app={appType}>
      <div className="page-header">
        <div><h2>{client_label(appType)}</h2><p className="muted">{copy.description}</p></div>
      </div>
      <section className="card" aria-busy={loading} aria-labelledby="client_plan_title">
        <div className="card-head">
          <h3 id="client_plan_title">{copy.title}</h3>
          <button className="btn small" data-testid="cli-plan-refresh" onClick={() => set_refresh_version((version) => version + 1)} disabled={loading}>{t('common.refresh')}</button>
        </div>
        {loading ? <p className="muted" role="status" data-testid="cli-plan-loading">{t('common.loading')}</p>
          : failed ? <p className="error-banner" role="alert" data-testid="cli-plan-error">{copy.error}</p>
          : current_plan?.models.length ? (
            <ul className="client_plan_models" data-testid="cli-plan-models">
              {current_plan.models.map(({ model, slots }) => (
                <li key={model} data-testid="cli-router-model">
                  <span className="muted small">{slots.map((slot) => slot === 'default' ? copy.selected : copy[slot]).join(' · ')}</span>
                  <code>{model}</code>
                </li>
              ))}
            </ul>
          ) : <p className="muted" role="status" data-testid="cli-plan-empty">{copy.empty}</p>}
      </section>
      {!loading && !failed && arena_context.initial_selector !== null ? (
        <Arena_chart
          key={JSON.stringify([appType, refresh_version, arena_context.model_options.map(({ selector }) => selector)])}
          model_options={arena_context.model_options}
          initial_selector={arena_context.initial_selector}
          fixed_surface={arena_context.surface}
          context_label={client_label(appType)}
          show_observed={false}
        />
      ) : (
        <section className="card arena_panel" data-testid="coding-agent-arena-state" data-state={loading ? 'loading' : failed ? 'error' : 'empty'} aria-busy={loading}>
          <div className="card-head"><h3>{copy.arena_title}</h3></div>
          <p className="muted" role="status">{loading ? copy.arena_loading : failed ? copy.arena_error : copy.arena_empty}</p>
        </section>
      )}
    </section>
  );
}
