import { useEffect, useMemo, useState } from 'react';
import {
  deleteSession,
  listSessions,
  type SessionMeta,
} from '../../lib/api/sessions';

export function SessionsPage() {
  const [sessions, setSessions] = useState<SessionMeta[]>([]);
  const [allSessions, setAllSessions] = useState<SessionMeta[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [search, setSearch] = useState('');
  const [activeApp, setActiveApp] = useState<string | null>(null);

  const refresh = async () => {
    setError(null);
    try {
      // Always fetch the full list so the per-app tabs keep their counts even
      // while the visible rows are filtered to one app. Filtering client-side
      // also keeps the search box working across apps.
      const list = await listSessions(undefined);
      setAllSessions(list);
      setSessions(
        activeApp ? list.filter((s) => s.appId === activeApp) : list,
      );
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeApp]);

  const appCounts = useMemo(() => {
    const m = new Map<string, number>();
    for (const s of allSessions) m.set(s.appId, (m.get(s.appId) ?? 0) + 1);
    return m;
  }, [allSessions]);

  const filtered = useMemo(() => {
    if (!search) return sessions;
    const q = search.toLowerCase();
    return sessions.filter((s) => {
      const fields = [s.id, s.title, s.summary, s.projectDir, s.sourcePath]
        .filter(Boolean)
        .map(String)
        .join('\n')
        .toLowerCase();
      return fields.includes(q);
    });
  }, [sessions, search]);

  const handleDelete = async (id: string) => {
    if (!confirm(`Delete session "${id}"?`)) return;
    setBusy(true);
    setError(null);
    try {
      await deleteSession(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const formatDate = (epoch?: number | null): string => {
    if (!epoch) return '-';
    try {
      return new Date(epoch * 1000).toLocaleString();
    } catch {
      return String(epoch);
    }
  };

  return (
    <div className="sessions-page">
      <header className="page-header">
        <h2>Sessions</h2>
        <input
          className="search-input"
          type="search"
          placeholder="Search title, project, path..."
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
      </header>

      {error && <pre className="aag-error">{error}</pre>}

      <div className="app-tabs">
        <button
          className={activeApp === null ? 'tab active' : 'tab'}
          onClick={() => setActiveApp(null)}
        >
          All ({allSessions.length})
        </button>
        {Array.from(appCounts.entries()).map(([app, count]) => (
          <button
            key={app}
            className={activeApp === app ? 'tab active' : 'tab'}
            onClick={() => setActiveApp(app)}
          >
            {app} ({count})
          </button>
        ))}
      </div>

      {filtered.length === 0 ? (
        <p className="muted">
          No sessions found. Sessions are discovered from Claude Code / Codex
          JSONL files in later phases. Use the API directly to insert rows for
          testing.
        </p>
      ) : (
        <ul className="session-list">
          {filtered.map((s) => (
            <li key={s.id} className="session-card">
              <div className="session-main">
                <div className="session-title">
                  <strong>{s.title ?? s.id}</strong>
                  <code className="session-id">{s.id}</code>
                </div>
                {s.summary && <p className="session-summary">{s.summary}</p>}
                <div className="session-meta muted">
                  <span>app: {s.appId}</span>
                  {s.projectDir && <span> · {s.projectDir}</span>}
                  <span> · {s.messageCount} msgs</span>
                  <span> · last active: {formatDate(s.lastActiveAt)}</span>
                </div>
                {s.sourcePath && (
                  <code className="session-path">{s.sourcePath}</code>
                )}
                {s.resumeCommand && (
                  <pre className="session-resume">{s.resumeCommand}</pre>
                )}
              </div>
              <div className="session-actions">
                <button
                  className="btn danger"
                  onClick={() => handleDelete(s.id)}
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
  );
}