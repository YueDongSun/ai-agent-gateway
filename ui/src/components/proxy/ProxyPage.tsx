import { useEffect, useState } from 'react';
import {
  fetchProxyHealth,
  getProxyStatus,
  startProxy,
  stopProxy,
  type ProxyHealth,
  type ProxyStatus,
} from '../../lib/api/proxy';

export function ProxyPage() {
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [health, setHealth] = useState<ProxyHealth | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

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
    </div>
  );
}