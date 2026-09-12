import { useEffect, useState } from 'react';
import {
  createBackup,
  deleteBackup,
  listBackups,
  type BackupEntry,
} from '../../lib/api/backup';
import { useT } from '../../lib/ctx';

export function BackupPage() {
  const t = useT();
  const [backups, setBackups] = useState<BackupEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = async () => {
    try {
      const list = await listBackups();
      setBackups(list);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const handleCreate = async () => {
    setBusy(true);
    setError(null);
    try {
      const entry = await createBackup();
      setError(t('backup.created.detail', { label: entry.label, size: (entry.sizeBytes / 1024).toFixed(1) }));
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleDelete = async (id: string) => {
    if (!confirm(t('backup.delete.confirm', { id }))) return;
    setBusy(true);
    setError(null);
    try {
      await deleteBackup(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const formatSize = (bytes: number): string => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  const formatDate = (iso: string): string => {
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  };

  return (
    <div className="backup-page">
      <header className="page-header">
        <h2>{t('backup.title')}</h2>
        <button className="btn primary" onClick={handleCreate} disabled={busy}>
          {t('backup.create')}
        </button>
      </header>

      {error && <pre className="aag-error">{error}</pre>}

      <section className="list-section">
        {backups.length === 0 ? (
          <p className="muted">
            {t('backup.empty')}
          </p>
        ) : (
          <ul className="backup-list">
            {backups.map((b) => (
              <li key={b.id} className="backup-card">
                <div className="backup-main">
                  <div className="backup-label">{b.label}</div>
                  <code className="backup-id">{b.id}</code>
                  <span className="backup-meta">
                    {formatSize(b.sizeBytes)} · {formatDate(b.createdAt)}
                  </span>
                </div>
                <div className="backup-actions">
                  <button
                    className="btn danger"
                    onClick={() => handleDelete(b.id)}
                    disabled={busy}
                  >
                    {t('common.delete')}
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
