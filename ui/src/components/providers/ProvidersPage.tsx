import { useEffect, useState } from 'react';
import {
  APP_TYPES,
  type AppType,
  type Provider,
} from '../../lib/api/types';
import {
  deleteProvider,
  getCurrentProvider,
  listProviders,
  saveProvider,
  switchProvider,
} from '../../lib/api/providers';

interface AddFormState {
  id: string;
  name: string;
  settingsJson: string;
}

const EMPTY_FORM: AddFormState = {
  id: '',
  name: '',
  settingsJson: '{}',
};

export function ProvidersPage() {
  const [appType, setAppType] = useState<AppType>('claude_code');
  const [providers, setProviders] = useState<Provider[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [showAdd, setShowAdd] = useState(false);
  const [form, setForm] = useState<AddFormState>(EMPTY_FORM);

  const refresh = async (app: AppType) => {
    setLoading(true);
    setError(null);
    try {
      const [list, cur] = await Promise.all([
        listProviders(app),
        getCurrentProvider(app),
      ]);
      setProviders(list);
      setCurrent(cur);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void refresh(appType);
  }, [appType]);

  const handleSwitch = async (id: string) => {
    setError(null);
    try {
      const result = await switchProvider(id, appType);
      setCurrent(id);
      // Re-read list in case ordering changed.
      await refresh(appType);
      const written = result.written.join(', ');
      setError(
        appType === 'codex'
          ? `Switched ${id}. Wrote ${result.written.length} file(s) to: ${written}`
          : `Switched ${id}. Wrote ${result.written.length} file(s).`,
      );
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDelete = async (id: string) => {
    if (!confirm(`Delete provider "${id}"?`)) return;
    setError(null);
    try {
      await deleteProvider(id, appType);
      await refresh(appType);
    } catch (e) {
      setError(String(e));
    }
  };

  const handleAdd = async () => {
    setError(null);
    try {
      const settingsConfig = JSON.parse(form.settingsJson);
      const p: Provider = {
        id: form.id,
        name: form.name,
        settingsConfig,
        createdAt: Math.floor(Date.now() / 1000),
        sortIndex: providers.length + 1,
        inFailoverQueue: false,
      };
      await saveProvider(appType, p);
      setShowAdd(false);
      setForm(EMPTY_FORM);
      await refresh(appType);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="providers-page">
      <header className="page-header">
        <h2>Providers</h2>
        <div className="app-tabs">
          {APP_TYPES.map((a) => (
            <button
              key={a.value}
              className={a.value === appType ? 'tab active' : 'tab'}
              onClick={() => setAppType(a.value)}
            >
              {a.label}
            </button>
          ))}
        </div>
      </header>

      {error && <pre className="aag-error">{error}</pre>}

      <section className="list-section">
        <div className="list-header">
          <h3>{APP_TYPES.find((a) => a.value === appType)?.label} providers</h3>
          <button className="btn primary" onClick={() => setShowAdd(true)}>
            + Add provider
          </button>
        </div>

        {loading ? (
          <p className="muted">Loading…</p>
        ) : providers.length === 0 ? (
          <p className="muted">No providers yet. Click "Add provider" to create one.</p>
        ) : (
          <ul className="provider-list">
            {providers.map((p) => {
              const isCurrent = p.id === current;
              return (
                <li key={p.id} className={isCurrent ? 'provider-card current' : 'provider-card'}>
                  <div className="provider-main">
                    <div className="provider-name">
                      <strong>{p.name}</strong>
                      {isCurrent && <span className="badge current-badge">current</span>}
                    </div>
                    <code className="provider-id">{p.id}</code>
                    {p.notes && <p className="provider-notes">{p.notes}</p>}
                  </div>
                  <div className="provider-actions">
                    <button
                      className="btn"
                      onClick={() => handleSwitch(p.id)}
                      disabled={isCurrent}
                    >
                      Switch
                    </button>
                    <button className="btn danger" onClick={() => handleDelete(p.id)}>
                      Delete
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </section>

      {showAdd && (
        <div className="modal-backdrop" onClick={() => setShowAdd(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>Add {APP_TYPES.find((a) => a.value === appType)?.label} provider</h3>
            <label>
              <span>ID (unique, no spaces)</span>
              <input
                value={form.id}
                onChange={(e) => setForm({ ...form, id: e.target.value })}
                placeholder="my-provider"
              />
            </label>
            <label>
              <span>Display name</span>
              <input
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
                placeholder="My Provider"
              />
            </label>
            <label>
              <span>settings_config (JSON)</span>
              <textarea
                rows={8}
                value={form.settingsJson}
                onChange={(e) => setForm({ ...form, settingsJson: e.target.value })}
                spellCheck={false}
              />
              <small className="muted">
                Claude Code: <code>{`{ "env": { "ANTHROPIC_BASE_URL": "...", "ANTHROPIC_AUTH_TOKEN": "..." } }`}</code>
                <br />
                Codex: <code>{`{ "auth": {...}, "config": "model = \"...\"\n..." }`}</code>
                <br />
                Copilot CLI: <code>{`{ "model": "claude-sonnet-4", "env": { "COPILOT_PROVIDER_BASE_URL": "http://127.0.0.1:8787/v1", "COPILOT_PROVIDER_API_KEY": "...", "COPILOT_PROVIDER_TYPE": "openai" } }`}</code>
                <br />
                Pi: <code>{`{ "providerId": "my-pi", "baseUrl": "http://127.0.0.1:8787/v1", "apiKey": "...", "model": "claude-sonnet-4", "models": ["claude-sonnet-4", "gpt-4o"] }`}</code>
                {appType === 'antigravity' && (
                  <>
                    <br />
                    <span className="muted" style={{ color: '#e5a04a' }}>
                      ⚠ Antigravity desktop switching is not supported yet (no public config
                      format). Switching to an Antigravity provider returns an error; configure
                      the API key directly in Antigravity's model dropdown.
                    </span>
                  </>
                )}
              </small>
            </label>
            <div className="modal-actions">
              <button className="btn" onClick={() => setShowAdd(false)}>
                Cancel
              </button>
              <button
                className="btn primary"
                onClick={handleAdd}
                disabled={!form.id || !form.name}
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