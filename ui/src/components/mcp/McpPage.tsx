import { useEffect, useState } from 'react';
import {
  deleteMcpServer,
  listMcpServers,
  saveMcpServer,
  toggleMcpApp,
  type McpServer,
} from '../../lib/api/mcp';
import { useT } from '../../lib/ctx';

interface AddFormState {
  id: string;
  name: string;
  configJson: string;
}

const EMPTY_FORM: AddFormState = { id: '', name: '', configJson: '{"command":"","args":[],"env":{}}' };

export function McpPage() {
  const t = useT();
  const [servers, setServers] = useState<McpServer[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [showAdd, setShowAdd] = useState(false);
  const [form, setForm] = useState<AddFormState>(EMPTY_FORM);

  const refresh = async () => {
    try {
      const list = await listMcpServers();
      setServers(list);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const handleToggle = async (s: McpServer, app: 'claude' | 'codex') => {
    setError(null);
    try {
      const enabled = app === 'claude' ? !s.enabledClaude : !s.enabledCodex;
      await toggleMcpApp(s.id, app, enabled);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDelete = async (id: string) => {
    if (!confirm(t('mcp.delete.confirm', { id }))) return;
    setBusy(true);
    setError(null);
    try {
      await deleteMcpServer(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleAdd = async () => {
    setError(null);
    try {
      const config = JSON.parse(form.configJson);
      const s: McpServer = {
        id: form.id,
        name: form.name,
        serverConfig: config,
        tags: [],
        enabledClaude: true,
        enabledCodex: true,
      };
      await saveMcpServer(s);
      setShowAdd(false);
      setForm(EMPTY_FORM);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="mcp-page">
      <header className="page-header">
        <h2>{t('mcp.title')}</h2>
        <button className="btn primary" onClick={() => setShowAdd(true)} disabled={busy}>
          {t('mcp.add')}
        </button>
      </header>

      {error && <pre className="aag-error">{error}</pre>}

      <section className="list-section">
        {servers.length === 0 ? (
          <p className="muted">
            {t('mcp.empty')}
          </p>
        ) : (
          <ul className="mcp-list">
            {servers.map((s) => (
              <li key={s.id} className="mcp-card">
                <div className="mcp-main">
                  <div className="mcp-name">
                    <strong>{s.name}</strong>
                    <code className="mcp-id">{s.id}</code>
                  </div>
                  {s.description && <p className="mcp-desc">{s.description}</p>}
                  <details className="mcp-config-details">
                    <summary className="muted">server_config</summary>
                    <pre className="mcp-config">
                      {JSON.stringify(s.serverConfig, null, 2)}
                    </pre>
                  </details>
                  <div className="mcp-toggles">
                    <label>
                      <input
                        type="checkbox"
                        checked={s.enabledClaude}
                        onChange={() => handleToggle(s, 'claude')}
                      />
                      <span>{t('mcp.col.claude')}</span>
                    </label>
                    <label>
                      <input
                        type="checkbox"
                        checked={s.enabledCodex}
                        onChange={() => handleToggle(s, 'codex')}
                      />
                      <span>{t('mcp.col.codex')}</span>
                    </label>
                  </div>
                </div>
                <div className="mcp-actions">
                  <button className="btn danger" onClick={() => handleDelete(s.id)} disabled={busy}>
                    {t('common.delete')}
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      {showAdd && (
        <div className="modal-backdrop" onClick={() => setShowAdd(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>{t('mcp.modal.add')}</h3>
            <label>
              <span>{t('mcp.modal.id')} (unique, no spaces)</span>
              <input
                value={form.id}
                onChange={(e) => setForm({ ...form, id: e.target.value })}
                placeholder="fs-mcp"
              />
            </label>
            <label>
              <span>{t('mcp.modal.name')}</span>
              <input
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
                placeholder="Filesystem MCP"
              />
            </label>
            <label>
              <span>{t('mcp.modal.config')}</span>
              <textarea
                rows={8}
                value={form.configJson}
                onChange={(e) => setForm({ ...form, configJson: e.target.value })}
                spellCheck={false}
              />
            </label>
            <div className="modal-actions">
              <button className="btn" onClick={() => setShowAdd(false)}>
                {t('mcp.modal.cancel')}
              </button>
              <button
                className="btn primary"
                onClick={handleAdd}
                disabled={!form.id || !form.name}
              >
                {t('mcp.modal.save')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
