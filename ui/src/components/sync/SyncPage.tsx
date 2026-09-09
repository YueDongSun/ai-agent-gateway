import { useEffect, useState } from 'react';
import {
  deleteSyncConfig,
  listSyncConfigs,
  recordSyncResult,
  saveSyncConfig,
  syncCheck,
  syncDownload,
  syncUpload,
  type SyncConfig,
  type SyncProvider,
} from '../../lib/api/sync';

/// Default form values for a fresh, never-saved provider card.
const emptyS3: SyncConfig = {
  provider: 's3',
  enabled: false,
  endpoint: '',
  bucket: '',
  remotePath: null,
  accessKey: '',
  secretKey: '',
  region: '',
  lastSyncAt: null,
  lastError: null,
};

const emptyWebdav: SyncConfig = {
  provider: 'webdav',
  enabled: false,
  endpoint: '',
  bucket: null,
  remotePath: '',
  accessKey: '',
  secretKey: '',
  region: null,
  lastSyncAt: null,
  lastError: null,
};

export function SyncPage() {
  const [configs, setConfigs] = useState<Record<SyncProvider, SyncConfig | null>>({
    s3: null,
    webdav: null,
  });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = async () => {
    try {
      const list = await listSyncConfigs();
      const next: Record<SyncProvider, SyncConfig | null> = { s3: null, webdav: null };
      for (const c of list) {
        next[c.provider] = c;
      }
      setConfigs(next);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const handleSave = async (cfg: SyncConfig) => {
    setBusy(true);
    setError(null);
    try {
      await saveSyncConfig(cfg);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleDelete = async (provider: SyncProvider) => {
    if (!confirm(`Delete ${provider.toUpperCase()} sync configuration?`)) return;
    setBusy(true);
    setError(null);
    try {
      await deleteSyncConfig(provider);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleRun = async (
    provider: SyncProvider,
    action: 'check' | 'upload' | 'download',
  ) => {
    const cfg = configs[provider];
    if (!cfg) return;
    setBusy(true);
    setError(null);
    try {
      if (action === 'check') {
        await syncCheck(cfg);
        setError(`Connection to ${provider.toUpperCase()} succeeded.`);
      } else if (action === 'upload') {
        const r = await syncUpload(cfg);
        setError(`Uploaded ${r.bytes} bytes to ${provider.toUpperCase()}.`);
      } else {
        if (
          !confirm(
            `Downloading will REPLACE the local AAG database with the remote file. Continue?`,
          )
        ) {
          setBusy(false);
          return;
        }
        const r = await syncDownload(cfg);
        setError(`Downloaded ${r.bytes} bytes. Restart the app for changes to take effect.`);
      }
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="sync-page">
      <header className="page-header">
        <h2>Cloud sync</h2>
        <p className="muted" style={{ margin: 0, fontSize: 12 }}>
          Upload your AAG database to S3 or WebDAV so multiple devices share the
          same provider configuration. S3 uses AWS Signature v4; custom
          S3-compatible endpoints (MinIO, etc.) work via the &quot;endpoint&quot;
          field with an http:// host.
        </p>
      </header>

      {error && <pre className="aag-error">{error}</pre>}

      <div className="sync-grid">
        <SyncCard
          provider="s3"
          emptyConfig={emptyS3}
          current={configs.s3}
          onSave={handleSave}
          onDelete={handleDelete}
          onRun={handleRun}
          busy={busy}
        />
        <SyncCard
          provider="webdav"
          emptyConfig={emptyWebdav}
          current={configs.webdav}
          onSave={handleSave}
          onDelete={handleDelete}
          onRun={handleRun}
          busy={busy}
        />
      </div>
    </div>
  );
}

interface SyncCardProps {
  provider: SyncProvider;
  emptyConfig: SyncConfig;
  current: SyncConfig | null;
  onSave: (cfg: SyncConfig) => Promise<void>;
  onDelete: (provider: SyncProvider) => Promise<void>;
  onRun: (
    provider: SyncProvider,
    action: 'check' | 'upload' | 'download',
  ) => Promise<void>;
  busy: boolean;
}

function SyncCard({
  provider,
  emptyConfig,
  current,
  onSave,
  onDelete,
  onRun,
  busy,
}: SyncCardProps) {
  const [draft, setDraft] = useState<SyncConfig>(current ?? emptyConfig);

  // When the parent refresh yields a new current value, replace the draft.
  useEffect(() => {
    setDraft(current ?? emptyConfig);
  }, [current, emptyConfig]);

  const update = <K extends keyof SyncConfig>(key: K, value: SyncConfig[K]) => {
    setDraft((d) => ({ ...d, [key]: value }));
  };

  const handleSaveClick = () => {
    // Normalize empty optional strings to null so the backend stores them cleanly.
    const normalized: SyncConfig = {
      ...draft,
      bucket: provider === 's3' ? draft.bucket || null : null,
      region: provider === 's3' ? draft.region || null : null,
      remotePath: provider === 'webdav' ? draft.remotePath || null : null,
    };
    void onSave(normalized);
  };

  return (
    <section className={`sync-card sync-card-${provider}`}>
      <div className="sync-card-header">
        <h3>{provider === 's3' ? 'Amazon S3 (or S3-compatible)' : 'WebDAV'}</h3>
        <label className="sync-enabled">
          <input
            type="checkbox"
            checked={draft.enabled}
            onChange={(e) => update('enabled', e.target.checked)}
            disabled={busy}
          />
          Enabled
        </label>
      </div>

      <div className="sync-field">
        <label>Endpoint</label>
        <input
          type="text"
          value={draft.endpoint}
          onChange={(e) => update('endpoint', e.target.value)}
          placeholder={provider === 's3' ? 's3.amazonaws.com (or custom host)' : 'https://dav.example.com/dav/'}
          disabled={busy}
        />
      </div>

      {provider === 's3' ? (
        <>
          <div className="sync-field">
            <label>Bucket</label>
            <input
              type="text"
              value={draft.bucket ?? ''}
              onChange={(e) => update('bucket', e.target.value)}
              placeholder="my-aag-backups"
              disabled={busy}
            />
          </div>
          <div className="sync-field">
            <label>Region</label>
            <input
              type="text"
              value={draft.region ?? ''}
              onChange={(e) => update('region', e.target.value)}
              placeholder="us-east-1"
              disabled={busy}
            />
          </div>
        </>
      ) : (
        <div className="sync-field">
          <label>Remote directory</label>
          <input
            type="text"
            value={draft.remotePath ?? ''}
            onChange={(e) => update('remotePath', e.target.value)}
            placeholder="/aag/"
            disabled={busy}
          />
        </div>
      )}

      <div className="sync-field">
        <label>{provider === 's3' ? 'Access key ID' : 'Username'}</label>
        <input
          type="text"
          value={draft.accessKey}
          onChange={(e) => update('accessKey', e.target.value)}
          autoComplete="off"
          disabled={busy}
        />
      </div>

      <div className="sync-field">
        <label>{provider === 's3' ? 'Secret access key' : 'Password'}</label>
        <input
          type="password"
          value={draft.secretKey}
          onChange={(e) => update('secretKey', e.target.value)}
          autoComplete="off"
          disabled={busy}
        />
      </div>

      {current && (
        <div className="sync-status">
          {current.lastSyncAt != null && (
            <span className="sync-status-line">
              Last sync: <code>{new Date(current.lastSyncAt * 1000).toLocaleString()}</code>
            </span>
          )}
          {current.lastError && (
            <span className="sync-status-line sync-status-error">
              Last error: <code>{current.lastError}</code>
            </span>
          )}
        </div>
      )}

      <div className="sync-actions">
        <button className="btn primary" onClick={handleSaveClick} disabled={busy}>
          Save
        </button>
        {current && (
          <button className="btn danger" onClick={() => onDelete(provider)} disabled={busy}>
            Delete
          </button>
        )}
        {current && (
          <button
            className="btn"
            onClick={() => void recordSyncResult(provider, 'manual test error')}
            disabled={busy}
            title="Stamp a test last_sync_at + last_error on this config row"
          >
            Test stamp
          </button>
        )}
        {current && (
          <>
            <button
              className="btn"
              onClick={() => onRun(provider, 'check')}
              disabled={busy}
            >
              Check connection
            </button>
            <button
              className="btn"
              onClick={() => onRun(provider, 'upload')}
              disabled={busy}
            >
              Upload now
            </button>
            <button
              className="btn"
              onClick={() => onRun(provider, 'download')}
              disabled={busy}
            >
              Download now
            </button>
          </>
        )}
      </div>
    </section>
  );
}
