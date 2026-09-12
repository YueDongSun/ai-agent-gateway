import { useEffect, useState } from 'react';
import { useT } from '../../lib/ctx';
import {
  PROTOCOLS,
  PROTOCOL_LABELS,
  type Protocol,
  type Vendor,
  deleteVendor,
  listVendors,
  resolveEndpoint,
  saveVendor,
} from '../../lib/api/vendors';

interface VendorFormState {
  id: string;
  name: string;
  baseUrl: string;
  apiKey: string;
  /** checked protocols */
  protocols: Record<string, boolean>;
  /** per-protocol endpoint override inputs */
  endpoints: Record<string, string>;
}

function emptyForm(): VendorFormState {
  const protocols: Record<string, boolean> = {};
  for (const p of PROTOCOLS) protocols[p] = false;
  return { id: '', name: '', baseUrl: '', apiKey: '', protocols, endpoints: {} };
}

export function VendorsPage() {
  const t = useT();
  const [vendors, setVendors] = useState<Vendor[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [form, setForm] = useState<VendorFormState | null>(null);

  const refresh = async () => {
    try {
      const list = await listVendors();
      setVendors(list);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const openAdd = () => setForm(emptyForm());

  const openEdit = (v: Vendor) => {
    const protocols: Record<string, boolean> = {};
    for (const p of PROTOCOLS) protocols[p] = v.protocols.includes(p);
    setForm({
      id: v.id,
      name: v.name,
      baseUrl: v.baseUrl,
      apiKey: v.apiKey,
      protocols,
      endpoints: { ...v.endpoints },
    });
  };

  const handleSave = async () => {
    if (!form) return;
    setBusy(true);
    setError(null);
    try {
      const protocols = PROTOCOLS.filter((p) => form.protocols[p]);
      // Only keep endpoint overrides that are non-empty.
      const endpoints: Record<string, string> = {};
      for (const p of protocols) {
        const url = (form.endpoints[p] ?? '').trim();
        if (url) endpoints[p] = url;
      }
      await saveVendor({
        id: form.id,
        name: form.name,
        baseUrl: form.baseUrl.trim(),
        apiKey: form.apiKey,
        protocols,
        endpoints,
        createdAt: null,
      });
      setForm(null);
      await refresh();
      setError(t('vendors.saved'));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleDelete = async (id: string) => {
    if (!confirm(t('vendors.delete.confirm', { id }))) return;
    setBusy(true);
    setError(null);
    try {
      await deleteVendor(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="vendors-page">
      <header className="page-header">
        <h2>{t('vendors.title')}</h2>
        <button className="btn primary" onClick={openAdd} disabled={busy}>
          {t('vendors.add')}
        </button>
      </header>

      <p className="muted" style={{ margin: '0 0 16px', fontSize: 12 }}>
        {t('vendors.intro')}
      </p>

      {error && <pre className="aag-error">{error}</pre>}

      <section className="list-section">
        {vendors.length === 0 ? (
          <p className="muted">{t('vendors.empty')}</p>
        ) : (
          <ul className="vendor-list">
            {vendors.map((v) => (
              <li key={v.id} className="vendor-card">
                <div className="vendor-main">
                  <div className="vendor-name">
                    <strong>{v.name}</strong>
                    <code className="vendor-id">{v.id}</code>
                  </div>
                  <div className="vendor-meta muted">
                    <code>{v.baseUrl || '—'}</code>
                    <span>·</span>
                    {v.apiKey ? <span>{t('vendors.keySet')}</span> : <span>{t('vendors.keyMissing')}</span>}
                  </div>
                  <div className="vendor-protocols">
                    {v.protocols.map((p) => (
                      <span key={p} className="pill off" title={resolveEndpoint(v, p)}>
                        {PROTOCOL_LABELS[p as Protocol] ?? p}
                      </span>
                    ))}
                  </div>
                </div>
                <div className="vendor-actions">
                  <button className="btn" onClick={() => openEdit(v)} disabled={busy}>
                    {t('providers.edit')}
                  </button>
                  <button className="btn danger" onClick={() => handleDelete(v.id)} disabled={busy}>
                    {t('providers.delete')}
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      {form && (
        <div className="modal-backdrop" onClick={() => setForm(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>{form.id ? t('vendors.modal.edit', { id: form.id }) : t('vendors.modal.add')}</h3>
            <label>
              <span>{t('vendors.modal.id')}</span>
              <input
                value={form.id}
                onChange={(e) => setForm({ ...form, id: e.target.value })}
                disabled={Boolean(form.id)}
                placeholder="acme-relay"
              />
            </label>
            <label>
              <span>{t('vendors.modal.name')}</span>
              <input
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
                placeholder="Acme Relay"
              />
            </label>
            <label>
              <span>{t('vendors.modal.baseUrl')}</span>
              <input
                value={form.baseUrl}
                onChange={(e) => setForm({ ...form, baseUrl: e.target.value })}
                placeholder="https://api.acme.dev"
              />
            </label>
            <label>
              <span>{t('vendors.modal.apiKey')}</span>
              <input
                type="password"
                value={form.apiKey}
                onChange={(e) => setForm({ ...form, apiKey: e.target.value })}
                placeholder="sk-..."
              />
            </label>

            <div className="vendor-protocol-editor">
              <span className="muted" style={{ fontSize: 12 }}>{t('vendors.modal.protocols')}</span>
              {PROTOCOLS.map((p) => (
                <div key={p} className="vendor-protocol-row">
                  <label className="checkbox-label">
                    <input
                      type="checkbox"
                      checked={form.protocols[p] ?? false}
                      onChange={(e) =>
                        setForm({
                          ...form,
                          protocols: { ...form.protocols, [p]: e.target.checked },
                        })
                      }
                    />
                    <span>{PROTOCOL_LABELS[p]}</span>
                  </label>
                  {form.protocols[p] && (
                    <input
                      className="vendor-endpoint-input"
                      value={form.endpoints[p] ?? ''}
                      onChange={(e) =>
                        setForm({
                          ...form,
                          endpoints: { ...form.endpoints, [p]: e.target.value },
                        })
                      }
                      placeholder={t('vendors.modal.endpointOverride', { url: form.baseUrl || 'https://…' })}
                    />
                  )}
                </div>
              ))}
            </div>

            <div className="modal-actions">
              <button className="btn" onClick={() => setForm(null)}>
                {t('providers.modal.cancel')}
              </button>
              <button
                className="btn primary"
                onClick={handleSave}
                disabled={busy || !form.id || !form.name}
              >
                {t('providers.modal.save')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}