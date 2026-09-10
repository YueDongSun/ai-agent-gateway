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

export function ProxyPage() {
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
      setError(`failover config load failed: ${String(e)}`);
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
      setError(`proxy unreachable: ${String(e)}`);
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
      setError(`stopped (config: ${s?.config.host ?? '?'}:${s?.config.port ?? '?'})`);
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
      setError('Provider saved. Restart the proxy for it to take effect.');
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleDeleteProvider = async (id: string) => {
    if (!confirm(`Delete proxy provider "${id}"?`)) return;
    setBusy(true);
    setError(null);
    try {
      await deleteProxyProvider(id);
      await refreshFailover();
      setError('Provider deleted. Restart the proxy for it to take effect.');
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
      setError('Route saved. Restart the proxy for it to take effect.');
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleDeleteRoute = async (alias: string) => {
    if (!confirm(`Delete route "${alias}"?`)) return;
    setBusy(true);
    setError(null);
    try {
      await deleteRoute(alias);
      await refreshFailover();
      setError('Route deleted. Restart the proxy for it to take effect.');
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

  return (
    <div className="proxy-page">
      <header className="page-header">
        <h2>Local Proxy</h2>
        <div className="proxy-actions">
          {!running ? (
            <button className="btn primary" onClick={handleStart} disabled={busy}>
              Start
            </button>
          ) : (
            <button className="btn danger" onClick={handleStop} disabled={busy}>
              Stop
            </button>
          )}
        </div>
      </header>

      {error && <pre className="aag-error">{error}</pre>}

      <section className="proxy-status-card">
        <div className="status-row">
          <span className={running ? 'pill on' : 'pill off'}>
            {running ? '● running' : '○ stopped'}
          </span>
          {cfg && (
            <code className="endpoint">
              http://{cfg.host}:{cfg.port}
            </code>
          )}
        </div>

        {running && health && (
          <>
            <div className="meta-row">
              <span className="muted">Models:</span>
              {health.models.map((m) => (
                <code key={m} className="model-chip">
                  {m}
                </code>
              ))}
            </div>
            <h4>Providers</h4>
            <table className="provider-health">
              <thead>
                <tr>
                  <th>ID</th>
                  <th>Enabled</th>
                  <th>Breaker</th>
                  <th>Failures</th>
                  <th>Successes</th>
                </tr>
              </thead>
              <tbody>
                {health.providers.map((p) => (
                  <tr key={p.id}>
                    <td><code>{p.id}</code></td>
                    <td>{p.enabled ? 'yes' : 'no'}</td>
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
            Click <strong>Start</strong> to launch the proxy on the configured
            host:port. Default seed config gives you two mock providers and three
            routes (<code>claude-sonnet-4</code>, <code>gpt-4o</code>,{' '}
            <code>mock-fail</code>).
          </p>
        )}
      </section>

      {/* ── Failover configuration (providers + routes) ── */}
      <section className="proxy-failover">
        <h3>Failover configuration</h3>
        <p className="muted" style={{ margin: 0, fontSize: 12 }}>
          Add upstream providers (mock / openai / anthropic) and build model
          alias → provider-chain routes. When the first hop fails, the proxy
          falls back to the next. Saving here updates the DB; restart the proxy
          to rebuild the router.
        </p>

        <div className="failover-grid">
          {/* Proxy providers */}
          <div className="failover-col">
            <div className="list-header">
              <h4>Proxy providers</h4>
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
                + Add
              </button>
            </div>
            {providers.length === 0 ? (
              <p className="muted">No proxy providers.</p>
            ) : (
              <ul className="failover-provider-list">
                {providers.map((p) => (
                  <li key={p.id} className="failover-provider">
                    <div>
                      <strong>{p.id}</strong>{' '}
                      <span className="muted">({p.type})</span>
                      <div className="muted" style={{ fontSize: 11 }}>
                        {p.baseURL || '(mock)'} · models: {p.models.join(', ') || '-'}
                        {' · '}
                        {p.enabled ? 'enabled' : 'disabled'}
                      </div>
                    </div>
                    <div className="failover-provider-actions">
                      <button
                        className="btn"
                        onClick={() => setProviderForm({ ...p, models: [...p.models] })}
                        disabled={busy}
                      >
                        Edit
                      </button>
                      <button
                        className="btn danger"
                        onClick={() => handleDeleteProvider(p.id)}
                        disabled={busy}
                      >
                        Delete
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
              <h4>Routes (alias → failover chain)</h4>
              <button
                className="btn primary"
                onClick={() =>
                  setRouteForm({ alias: '', description: '', hops: [{ provider: '', model: '' }] })
                }
                disabled={busy}
              >
                + Add
              </button>
            </div>
            {routes.length === 0 ? (
              <p className="muted">No routes.</p>
            ) : (
              <ul className="failover-route-list">
                {routes.map((r) => (
                  <li key={r.alias} className="failover-route">
                    <div>
                      <code>{r.alias}</code>
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
                        Edit
                      </button>
                      <button
                        className="btn danger"
                        onClick={() => handleDeleteRoute(r.alias)}
                        disabled={busy}
                      >
                        Delete
                      </button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
      </section>

      {/* Provider edit modal */}
      {providerForm && (
        <div className="modal-backdrop" onClick={() => setProviderForm(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>{providerForm.id ? `Edit provider: ${providerForm.id}` : 'Add proxy provider'}</h3>
            <label>
              <span>ID</span>
              <input
                value={providerForm.id}
                onChange={(e) => setProviderForm({ ...providerForm, id: e.target.value })}
                disabled={Boolean(providerForm.id)}
              />
            </label>
            <label>
              <span>Name</span>
              <input
                value={providerForm.name}
                onChange={(e) => setProviderForm({ ...providerForm, name: e.target.value })}
              />
            </label>
            <label>
              <span>Type</span>
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
                  <span>Base URL (endpoint)</span>
                  <input
                    value={providerForm.baseURL}
                    onChange={(e) => setProviderForm({ ...providerForm, baseURL: e.target.value })}
                    placeholder="https://api.openai.com/v1"
                  />
                </label>
                <label>
                  <span>API key</span>
                  <input
                    type="password"
                    value={providerForm.apiKey}
                    onChange={(e) => setProviderForm({ ...providerForm, apiKey: e.target.value })}
                  />
                </label>
              </>
            )}
            <label>
              <span>Models (comma-separated)</span>
              <input
                value={providerForm.models.join(', ')}
                onChange={(e) =>
                  setProviderForm({
                    ...providerForm,
                    models: e.target.value.split(',').map((s) => s.trim()).filter(Boolean),
                  })
                }
                placeholder="claude-sonnet-4, gpt-4o"
              />
            </label>
            <label className="checkbox-label">
              <input
                type="checkbox"
                checked={providerForm.enabled}
                onChange={(e) => setProviderForm({ ...providerForm, enabled: e.target.checked })}
              />
              <span>Enabled</span>
            </label>
            <div className="modal-actions">
              <button className="btn" onClick={() => setProviderForm(null)}>Cancel</button>
              <button
                className="btn primary"
                onClick={handleSaveProvider}
                disabled={busy || !providerForm.id || !providerForm.name}
              >
                Save
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Route edit modal */}
      {routeForm && (
        <div className="modal-backdrop" onClick={() => setRouteForm(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>{routeForm.alias ? `Edit route: ${routeForm.alias}` : 'Add route'}</h3>
            <label>
              <span>Alias (model name clients request)</span>
              <input
                value={routeForm.alias}
                onChange={(e) => setRouteForm({ ...routeForm, alias: e.target.value })}
                disabled={Boolean(routeForm.alias)}
              />
            </label>
            <label>
              <span>Description</span>
              <input
                value={routeForm.description ?? ''}
                onChange={(e) => setRouteForm({ ...routeForm, description: e.target.value })}
              />
            </label>
            <div className="route-hops">
              <span className="muted">Hops (fallback order)</span>
              {routeForm.hops.map((h, i) => (
                <div key={i} className="route-hop-row">
                  <select
                    value={h.provider}
                    onChange={(e) => updateHop(i, { provider: e.target.value })}
                  >
                    <option value="">(provider)</option>
                    {providers.map((p) => (
                      <option key={p.id} value={p.id}>{p.id}</option>
                    ))}
                  </select>
                  <input
                    value={h.model}
                    onChange={(e) => updateHop(i, { model: e.target.value })}
                    placeholder="model"
                  />
                  <button className="btn danger" onClick={() => removeHop(i)} disabled={routeForm.hops.length <= 1}>
                    ×
                  </button>
                </div>
              ))}
              <button className="btn" onClick={addRouteHop} disabled={busy}>
                + Add hop
              </button>
            </div>
            <div className="modal-actions">
              <button className="btn" onClick={() => setRouteForm(null)}>Cancel</button>
              <button
                className="btn primary"
                onClick={handleSaveRoute}
                disabled={busy || !routeForm.alias || routeForm.hops.some((h) => !h.provider || !h.model)}
              >
                Save
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}