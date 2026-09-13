import { useEffect, useState } from 'react';
import {
  deleteProxyProvider,
  deleteRoute,
  fetchProxyHealth,
  getProxyStatus,
  listProxyProviders,
  listRoutes,
  saveProxyProvider,
  saveRoute,
  startProxy,
  stopProxy,
  type ProxyHealth,
  type ProxyProvider,
  type ProxyStatus,
  type Route,
} from '../../lib/api/proxy';
import { getProxyConfig, saveProxyConfig } from '../../lib/api/proxy';
import { officialDisplayName } from '../../lib/models';
import { useT } from '../../lib/ctx';

export function ProxyPage() {
  const t = useT();
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [health, setHealth] = useState<ProxyHealth | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [providers, setProviders] = useState<ProxyProvider[]>([]);
  const [routes, setRoutes] = useState<Route[]>([]);
  const [providerForm, setProviderForm] = useState<ProxyProvider | null>(null);
  const [routeForm, setRouteForm] = useState<Route | null>(null);

  const refreshFailover = async () => {
    try {
      const [ps, rs] = await Promise.all([listProxyProviders(), listRoutes()]);
      setProviders(ps);
      setRoutes(rs);
    } catch (e) {
      setError(t('proxy.failover.loadFailed', { message: String(e) }));
    }
  };

  const refreshStatus = async () => {
    try {
      const s = await getProxyStatus();
      setStatus(s);
      return s;
    } catch (e) {
      setError(String(e));
      return null;
    }
  };

  const refreshHealth = async (s: ProxyStatus | null) => {
    if (!s || !s.running) {
      setHealth(null);
      return;
    }
    try {
      const h = await fetchProxyHealth(s.config);
      setHealth(h);
      setError(null);
    } catch (e) {
      setHealth(null);
      setError(t('proxy.unreachable', { message: String(e) }));
    }
  };

  useEffect(() => {
    void (async () => {
      const s = await refreshStatus();
      await refreshHealth(s);
    })();
    void refreshFailover();
  }, []);

  // Poll health every 3s while running
  useEffect(() => {
    if (!status?.running) return;
    const id = setInterval(async () => {
      const s = await refreshStatus();
      await refreshHealth(s);
    }, 3000);
    return () => clearInterval(id);
  }, [status?.running]);

  const handleStart = async () => {
    setBusy(true);
    setError(null);
    try {
      const msg = await startProxy();
      const s = await refreshStatus();
      await refreshHealth(s);
      setError(msg);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleStop = async () => {
    setBusy(true);
    setError(null);
    try {
      await stopProxy();
      const s = await refreshStatus();
      setHealth(null);
      setError(t('proxy.stopped.config', { host: s?.config.host ?? '?', port: s?.config.port ?? '?' }));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // ── Failover config handlers ──
  const handleSaveProvider = async () => {
    if (!providerForm) return;
    setBusy(true);
    setError(null);
    try {
      await saveProxyProvider(providerForm);
      setProviderForm(null);
      await refreshFailover();
      setError(t('proxy.provider.saved'));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleDeleteProvider = async (id: string) => {
    if (!confirm(t('proxy.provider.deleteConfirm', { id }))) return;
    setBusy(true);
    setError(null);
    try {
      await deleteProxyProvider(id);
      await refreshFailover();
      setError(t('proxy.provider.deleted'));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleSaveRoute = async () => {
    if (!routeForm) return;
    setBusy(true);
    setError(null);
    try {
      await saveRoute(routeForm);
      setRouteForm(null);
      await refreshFailover();
      setError(t('proxy.route.saved'));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleDeleteRoute = async (alias: string) => {
    if (!confirm(t('proxy.route.deleteConfirm', { alias }))) return;
    setBusy(true);
    setError(null);
    try {
      await deleteRoute(alias);
      await refreshFailover();
      setError(t('proxy.route.deleted'));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const addRouteHop = () => {
    if (!routeForm) return;
    setRouteForm({
      ...routeForm,
      hops: [...routeForm.hops, { provider: '', model: '' }],
    });
  };
  const updateHop = (i: number, patch: Partial<{ provider: string; model: string }>) => {
    if (!routeForm) return;
    const hops = routeForm.hops.slice();
    hops[i] = { ...hops[i], ...patch };
    setRouteForm({ ...routeForm, hops });
  };
  const removeHop = (i: number) => {
    if (!routeForm) return;
    setRouteForm({ ...routeForm, hops: routeForm.hops.filter((_, idx) => idx !== i) });
  };

  const running = status?.running ?? false;
  const cfg = status?.config;

  // API-key editor state (proxy_config.authToken) — the key cc-switch sends
  // as Bearer/x-api-key.
  const [apiKey, setApiKey] = useState('');
  const [savingKey, setSavingKey] = useState(false);
  useEffect(() => {
    void (async () => {
      try {
        setApiKey((await getProxyConfig()).authToken ?? '');
      } catch {
        /* editor stays empty */
      }
    })();
  }, []);
  const handleSaveKey = async () => {
    if (!cfg) return;
    setSavingKey(true);
    try {
      await saveProxyConfig({ ...cfg, authToken: apiKey.trim() });
      setError(t('proxy.apiKey.saved'));
    } catch (e) {
      setError(String(e));
    } finally {
      setSavingKey(false);
    }
  };

  return (
    <div className="proxy-page">
      <header className="page-header">
        <h2>{t('proxy.title')}</h2>
        <div className="proxy-actions">
          {!running ? (
            <button className="btn primary" onClick={handleStart} disabled={busy}>
              {t('proxy.start')}
            </button>
          ) : (
            <button className="btn danger" onClick={handleStop} disabled={busy}>
              {t('proxy.stop')}
            </button>
          )}
        </div>
      </header>

      {error && <pre className="aag-error">{error}</pre>}

      <section className="proxy-status-card">
        <div className="status-row">
          <span className={running ? 'pill on' : 'pill off'}>
            {running ? t('proxy.running') : t('proxy.stopped')}
          </span>
          {cfg && (
            <code className="endpoint">
              http://{cfg.host}:{cfg.port}
            </code>
          )}
        </div>

        <div className="endpoint-editor">
          <label>
            <span>{t('proxy.apiKey.label')}</span>
            <input
              type="text"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder="sk-aag-..."
              spellCheck={false}
              disabled={savingKey}
            />
          </label>
          <button className="btn" onClick={handleSaveKey} disabled={savingKey || !cfg}>
            {savingKey ? t('common.loading') : t('common.save')}
          </button>
        </div>
        <p className="muted endpoint-editor-hint">{t('proxy.apiKey.hint')}</p>

        {running && health && (
          <>
            <div className="meta-row">
              <span className="muted">{t('proxy.models')}</span>
              {health.models.map((m) => (
                <code key={m} className="model-chip" title={officialDisplayName(m) ?? undefined}>
                  {m}
                </code>
              ))}
            </div>
            <h4>{t('proxy.providers')}</h4>
            <table className="provider-health">
              <thead>
                <tr>
                  <th>{t('proxy.col.id')}</th>
                  <th>{t('proxy.col.enabled')}</th>
                  <th>{t('proxy.col.breaker')}</th>
                  <th>{t('proxy.col.failures')}</th>
                  <th>{t('proxy.col.successes')}</th>
                </tr>
              </thead>
              <tbody>
                {health.providers.map((p) => (
                  <tr key={p.id}>
                    <td><code>{p.id}</code></td>
                    <td>{p.enabled ? t('proxy.col.yes') : t('proxy.col.no')}</td>
                    <td>
                      <span className={`pill ${p.breaker.state}`}>
                        {p.breaker.state}
                      </span>
                    </td>
                    <td>{p.breaker.failures}</td>
                    <td>{p.breaker.successes}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </>
        )}

        {!running && (
          <p className="muted">
            {t('proxy.stoppedHint', {
              sonnet: 'claude-sonnet-4',
              gpt: 'gpt-4o',
              fail: 'mock-fail',
              fable: 'claude-fable-5',
              opus: 'claude-opus-4-8',
            })}
          </p>
        )}
      </section>

      {/* ── Failover configuration (providers + routes) ── */}
      <section className="proxy-failover">
        <h3>{t('proxy.failover.title')}</h3>
        <p className="muted" style={{ margin: 0, fontSize: 12 }}>
          {t('proxy.failover.desc')}
        </p>

        <div className="failover-grid">
          {/* Proxy providers */}
          <div className="failover-col">
            <div className="list-header">
              <h4>{t('proxy.failover.providers')}</h4>
              <button
                className="btn primary"
                onClick={() =>
                  setProviderForm({
                    id: '',
                    name: '',
                    type: 'openai',
                    apiKey: '',
                    baseURL: '',
                    models: [],
                    enabled: true,
                    timeoutMs: 60000,
                    weight: 1,
                    circuitBreaker: {
                      failureThreshold: 5,
                      cooldownMs: 30000,
                      halfOpenRetries: 1,
                      successThreshold: 2,
                    },
                  })
                }
                disabled={busy}
              >
                {t('proxy.failover.add')}
              </button>
            </div>
            {providers.length === 0 ? (
              <p className="muted">{t('proxy.failover.empty.providers')}</p>
            ) : (
              <ul className="failover-provider-list">
                {providers.map((p) => (
                  <li key={p.id} className="failover-provider">
                    <div>
                      <strong>{p.id}</strong>{' '}
                      <span className="muted">({p.type})</span>
                      <div className="muted" style={{ fontSize: 11 }}>
                        {p.baseURL || t('proxy.failover.mock')} · {p.models.length > 0 ? t('proxy.failover.models', { list: p.models.join(', ') }) : t('proxy.failover.modelsNone')}
                        {' · '}
                        {p.enabled ? t('proxy.failover.enabled') : t('proxy.failover.disabled')}
                      </div>
                    </div>
                    <div className="failover-provider-actions">
                      <button
                        className="btn"
                        onClick={() => setProviderForm({ ...p, models: [...p.models] })}
                        disabled={busy}
                      >
                        {t('common.edit')}
                      </button>
                      <button
                        className="btn danger"
                        onClick={() => handleDeleteProvider(p.id)}
                        disabled={busy}
                      >
                        {t('common.delete')}
                      </button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </div>

          {/* Routes */}
          <div className="failover-col">
            <div className="list-header">
              <h4>{t('proxy.failover.routes')}</h4>
              <button
                className="btn primary"
                onClick={() =>
                  setRouteForm({ alias: '', description: '', hops: [{ provider: '', model: '' }] })
                }
                disabled={busy}
              >
                {t('proxy.failover.add')}
              </button>
            </div>
            {routes.length === 0 ? (
              <p className="muted">{t('proxy.failover.empty.routes')}</p>
            ) : (
              <ul className="failover-route-list">
                {routes.map((r) => {
                  const display = officialDisplayName(r.alias);
                  return (
                    <li key={r.alias} className="failover-route">
                      <div>
                        <code>{r.alias}</code>
                        {display && <span className="muted"> — {display}</span>}
                        {display && (
                          <span className="pill off" style={{ marginLeft: 6, fontSize: 10 }}>
                            {t('proxy.route.official')}
                          </span>
                        )}
                        <div className="muted" style={{ fontSize: 11 }}>
                          {r.hops.map((h) => `${h.provider}/${h.model}`).join(' → ')}
                        </div>
                      </div>
                      <div className="failover-provider-actions">
                        <button
                          className="btn"
                          onClick={() =>
                            setRouteForm({
                              alias: r.alias,
                              description: r.description ?? '',
                              hops: r.hops.map((h) => ({ ...h })),
                            })
                          }
                          disabled={busy}
                        >
                          {t('common.edit')}
                        </button>
                        <button
                          className="btn danger"
                          onClick={() => handleDeleteRoute(r.alias)}
                          disabled={busy}
                        >
                          {t('common.delete')}
                        </button>
                      </div>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        </div>
      </section>

      {/* Provider edit modal */}
      {providerForm && (
        <div className="modal-backdrop" onClick={() => setProviderForm(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>{providerForm.id ? t('proxy.modal.provider.edit', { id: providerForm.id }) : t('proxy.modal.provider.add')}</h3>
            <label>
              <span>{t('proxy.modal.provider.id')}</span>
              <input
                value={providerForm.id}
                onChange={(e) => setProviderForm({ ...providerForm, id: e.target.value })}
                disabled={Boolean(providerForm.id)}
              />
            </label>
            <label>
              <span>{t('proxy.modal.provider.name')}</span>
              <input
                value={providerForm.name}
                onChange={(e) => setProviderForm({ ...providerForm, name: e.target.value })}
              />
            </label>
            <label>
              <span>{t('proxy.modal.provider.type')}</span>
              <select
                value={providerForm.type}
                onChange={(e) => setProviderForm({ ...providerForm, type: e.target.value })}
              >
                <option value="mock">mock</option>
                <option value="openai">openai</option>
                <option value="anthropic">anthropic</option>
              </select>
            </label>
            {providerForm.type !== 'mock' && (
              <>
                <label>
                  <span>{t('proxy.modal.provider.baseUrl')}</span>
                  <input
                    value={providerForm.baseURL}
                    onChange={(e) => setProviderForm({ ...providerForm, baseURL: e.target.value })}
                    placeholder="https://api.openai.com/v1"
                  />
                </label>
                <label>
                  <span>{t('proxy.modal.provider.apiKey')}</span>
                  <input
                    type="password"
                    value={providerForm.apiKey}
                    onChange={(e) => setProviderForm({ ...providerForm, apiKey: e.target.value })}
                  />
                </label>
              </>
            )}
            <label>
              <span>{t('proxy.modal.provider.models')}</span>
              <input
                value={providerForm.models.join(', ')}
                onChange={(e) =>
                  setProviderForm({
                    ...providerForm,
                    models: e.target.value.split(',').map((s) => s.trim()).filter(Boolean),
                  })
                }
                placeholder={t('proxy.modal.provider.modelsPlaceholder')}
              />
            </label>
            <label className="checkbox-label">
              <input
                type="checkbox"
                checked={providerForm.enabled}
                onChange={(e) => setProviderForm({ ...providerForm, enabled: e.target.checked })}
              />
              <span>{t('proxy.modal.provider.enabled')}</span>
            </label>
            <div className="modal-actions">
              <button className="btn" onClick={() => setProviderForm(null)}>{t('proxy.modal.provider.cancel')}</button>
              <button
                className="btn primary"
                onClick={handleSaveProvider}
                disabled={busy || !providerForm.id || !providerForm.name}
              >
                {t('proxy.modal.provider.save')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Route edit modal */}
      {routeForm && (
        <div className="modal-backdrop" onClick={() => setRouteForm(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>{routeForm.alias ? t('proxy.modal.route.edit', { alias: routeForm.alias }) : t('proxy.modal.route.add')}</h3>
            <label>
              <span>{t('proxy.modal.route.alias')}</span>
              <input
                value={routeForm.alias}
                onChange={(e) => setRouteForm({ ...routeForm, alias: e.target.value })}
                disabled={Boolean(routeForm.alias)}
              />
            </label>
            <label>
              <span>{t('proxy.modal.route.description')}</span>
              <input
                value={routeForm.description ?? ''}
                onChange={(e) => setRouteForm({ ...routeForm, description: e.target.value })}
              />
            </label>
            <div className="route-hops">
              <span className="muted">{t('proxy.modal.route.hops')}</span>
              {routeForm.hops.map((h, i) => (
                <div key={i} className="route-hop-row">
                  <select
                    value={h.provider}
                    onChange={(e) => updateHop(i, { provider: e.target.value })}
                  >
                    <option value="">{t('proxy.modal.route.providerPlaceholder')}</option>
                    {providers.map((p) => (
                      <option key={p.id} value={p.id}>{p.id}</option>
                    ))}
                  </select>
                  <input
                    value={h.model}
                    onChange={(e) => updateHop(i, { model: e.target.value })}
                    placeholder={t('proxy.modal.route.modelPlaceholder')}
                  />
                  <button className="btn danger" onClick={() => removeHop(i)} disabled={routeForm.hops.length <= 1}>
                    ×
                  </button>
                </div>
              ))}
              <button className="btn" onClick={addRouteHop} disabled={busy}>
                {t('proxy.modal.route.addHop')}
              </button>
            </div>
            <div className="modal-actions">
              <button className="btn" onClick={() => setRouteForm(null)}>{t('proxy.modal.route.cancel')}</button>
              <button
                className="btn primary"
                onClick={handleSaveRoute}
                disabled={busy || !routeForm.alias || routeForm.hops.some((h) => !h.provider || !h.model)}
              >
                {t('proxy.modal.route.save')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}