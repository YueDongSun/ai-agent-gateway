import { useEffect, useMemo, useState } from 'react';
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
import { useT } from '../../lib/ctx';
import { SCHEMAS, stringifyConfig, APP_PROTOCOL } from '../../lib/providerForm';
import { PROTOCOL_LABELS, type Protocol, type Vendor, listVendors, resolveEndpoint } from '../../lib/api/vendors';
import { highlightJson, validateJson } from '../../lib/jsonHighlight';

interface AddFormState {
  id: string;
  name: string;
  /** Smart-form field values, keyed by schema field key. */
  fields: Record<string, string>;
  /** Advanced mode: raw JSON editor takes over. */
  rawJson: string;
  /** Selected vendor id (empty = manual entry). */
  vendorId: string;
  /** Selected vendor protocol (when vendor has multiple). */
  protocol: string;
}

function emptyForm(): AddFormState {
  return { id: '', name: '', fields: {}, rawJson: '{}', vendorId: '', protocol: '' };
}

export function ProvidersPage() {
  const t = useT();
  const tabKey = (at: AppType): string =>
    at === 'claude_code' ? 'providers.tab.claudeCode' : `providers.tab.${at}`;
  const [appType, setAppType] = useState<AppType>('claude_code');
  const [providers, setProviders] = useState<Provider[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [showAdd, setShowAdd] = useState(false);
  const [form, setForm] = useState<AddFormState>(emptyForm);
  const [advanced, setOpenAdvanced] = useState(false);
  const [vendors, setVendors] = useState<Vendor[]>([]);
  /** When editing, the original provider row (id/createdAt/sortIndex preserved on save). */
  const [editing, setEditing] = useState<Provider | null>(null);

  // Load vendors once for the picker.
  useEffect(() => {
    void (async () => {
      try {
        setVendors(await listVendors());
      } catch {
        /* vendors are optional — manual entry still works */
      }
    })();
  }, []);

  const selectedVendor = vendors.find((v) => v.id === form.vendorId) ?? null;

  const schema = SCHEMAS[appType];
  const noFields = schema.fields.length === 0;
  const rawValidation = useMemo(() => validateJson(form.rawJson), [form.rawJson]);

  // Compute the current JSON config: either rawJson (advanced) or built
  // from the smart fields.
  const previewJson = useMemo(() => {
    if (advanced) return form.rawJson;
    if (noFields) return '{}';
    const values: Record<string, string> = {};
    for (const f of schema.fields) {
      values[f.key] = form.fields[f.key] ?? '';
    }
    try {
      return JSON.stringify(schema.build(values), null, 2);
    } catch {
      return '{}';
    }
  }, [advanced, form.rawJson, form.fields, schema, noFields]);

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

  // Reset the form whenever the app tab changes (so fields match the new
  // schema). Pre-populate field defaults from the schema (first option of
  // each select, sensible defaults).
  useEffect(() => {
    const defaults: Record<string, string> = {};
    for (const f of schema.fields) {
      defaults[f.key] = '';
    }
    if (appType === 'codex') defaults.providerType = 'openai';
    if (appType === 'copilot') defaults.providerType = 'openai';
    setForm({ id: '', name: '', fields: defaults, rawJson: stringifyConfig(schema.build(defaults)), vendorId: '', protocol: '' });
    setOpenAdvanced(false);
  }, [appType, schema]);

  const handleSwitch = async (id: string) => {
    setError(null);
    try {
      const result = await switchProvider(id, appType);
      setCurrent(id);
      await refresh(appType);
      const written = result.written.join(', ');
      setError(
        appType === 'codex'
          ? t('providers.switchedCodex', { name: id, count: result.written.length, files: written })
          : t('providers.switched', { name: id, count: result.written.length }),
      );
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDelete = async (id: string) => {
    if (!confirm(t('providers.delete.confirm', { id }))) return;
    setError(null);
    try {
      await deleteProvider(id, appType);
      await refresh(appType);
    } catch (e) {
      setError(String(e));
    }
  };

  const openEdit = (p: Provider) => {
    // Prefill smart fields from the stored settings_config via the
    // schema's extract(); fall back to raw JSON mode when extraction
    // yields nothing recognizable.
    const extracted = schema.extract?.(p.settingsConfig ?? {}) ?? {};
    const recognized = Object.values(extracted).some((v) => Boolean(v));
    const fields = { ...form.fields };
    for (const f of schema.fields) {
      fields[f.key] = extracted[f.key] ?? '';
    }
    setEditing(p);
    setShowAdd(true);
    setOpenAdvanced(!recognized);
    setForm({
      id: p.id,
      name: p.name,
      fields,
      rawJson: stringifyConfig(p.settingsConfig ?? {}),
      vendorId: '',
      protocol: '',
    });
  };

  const handleAdd = async () => {
    setError(null);
    try {
      const validation = advanced ? validateJson(form.rawJson) : { ok: true as const, value: undefined };
      if (advanced && !validation.ok) {
        setError(t('providers.invalidJson', { message: validation.error ?? '' }));
        return;
      }
      const settingsConfig = advanced
        ? (validation.value as Record<string, unknown>)
        : schema.build(form.fields as Record<string, string | number | null>);
      const p: Provider = {
        id: form.id,
        name: form.name,
        settingsConfig,
        // Editing preserves the original row's metadata.
        createdAt: editing?.createdAt ?? Math.floor(Date.now() / 1000),
        sortIndex: editing?.sortIndex ?? providers.length + 1,
        notes: editing?.notes ?? null,
        inFailoverQueue: editing?.inFailoverQueue ?? false,
      };
      await saveProvider(appType, p);
      setShowAdd(false);
      setEditing(null);
      setForm(emptyForm());
      await refresh(appType);
      setError(editing ? t('providers.saved') : null);
    } catch (e) {
      setError(String(e));
    }
  };

  // When the user flips to raw JSON mode, seed rawJson with the
  // current built config so they don't lose what they typed.
  const toggleAdvanced = () => {
    if (!advanced) {
      setForm({ ...form, rawJson: previewJson });
    }
    setOpenAdvanced(!advanced);
  };

  // ── Vendor picker logic ──
  // Picking a vendor fills baseUrl + apiKey from the vendor, choosing the
  // protocol the current app speaks when the vendor has it (falls back to
  // the vendor's first protocol). Switching protocols re-resolves the
  // endpoint (endpoints[protocol] ?? baseUrl).
  const pickVendor = (vendorId: string) => {
    const v = vendors.find((x) => x.id === vendorId) ?? null;
    if (!v) {
      // Manual entry: clear vendor-derived values but keep the rest.
      setForm({ ...form, vendorId: '', protocol: '' });
      return;
    }
    const want = APP_PROTOCOL[appType] ?? '';
    const protocol =
      v.protocols.includes(want) ? want : (v.protocols[0] ?? '');
    const baseUrl = schema.fields.some((f) => f.key === 'baseUrl')
      ? resolveEndpoint(v, protocol)
      : '';
    setForm({
      ...form,
      vendorId,
      protocol,
      // Auto-fill name from the vendor when empty (user can override).
      name: form.name || v.name,
      fields: {
        ...form.fields,
        ...(schema.fields.some((f) => f.key === 'baseUrl') ? { baseUrl } : {}),
        ...(schema.fields.some((f) => f.key === 'apiKey') ? { apiKey: v.apiKey } : {}),
      },
    });
  };

  const pickProtocol = (protocol: string) => {
    const v = selectedVendor;
    if (!v) return;
    const baseUrl = resolveEndpoint(v, protocol);
    setForm({
      ...form,
      protocol,
      fields: {
        ...form.fields,
        ...(schema.fields.some((f) => f.key === 'baseUrl') ? { baseUrl } : {}),
      },
    });
  };

  return (
    <div className="providers-page">
      <header className="page-header">
        <h2>{t('providers.title')}</h2>
        <div className="app-tabs">
          {APP_TYPES.map((a) => (
            <button
              key={a.value}
              className={a.value === appType ? 'tab active' : 'tab'}
              onClick={() => setAppType(a.value)}
            >
              {t(tabKey(a.value))}
            </button>
          ))}
        </div>
      </header>

      {error && <pre className="aag-error">{error}</pre>}

      <section className="list-section">
        <div className="list-header">
          <h3>{t('providers.section', { app: t(tabKey(appType)) })}</h3>
          <button
            className="btn primary"
            onClick={() => {
              setEditing(null);
              setShowAdd(true);
            }}
          >
            {t('providers.add')}
          </button>
        </div>

        {loading ? (
          <p className="muted">{t('common.loading')}</p>
        ) : providers.length === 0 ? (
          <p className="muted">{t('providers.empty')}</p>
        ) : (
          <ul className="provider-list">
            {providers.map((p) => {
              const isCurrent = p.id === current;
              return (
                <li key={p.id} className={isCurrent ? 'provider-card current' : 'provider-card'}>
                  <div className="provider-main">
                    <div className="provider-name">
                      <strong>{p.name}</strong>
                      {isCurrent && <span className="badge current-badge">{t('providers.current')}</span>}
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
                      {t('providers.switch')}
                    </button>
                    <button className="btn" onClick={() => openEdit(p)}>
                      {t('providers.edit')}
                    </button>
                    <button className="btn danger" onClick={() => handleDelete(p.id)}>
                      {t('providers.delete')}
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
            <h3>
              {editing
                ? t('providers.modal.edit', { name: editing.name })
                : t('providers.modal.addFor', { app: t(tabKey(appType)) })}
            </h3>

            <label>
              <span>{t('providers.modal.id')} <small>(unique, no spaces)</small></span>
              <input
                value={form.id}
                onChange={(e) => setForm({ ...form, id: e.target.value })}
                placeholder="my-provider"
                disabled={Boolean(editing)}
              />
            </label>
            <label>
              <span>{t('providers.modal.name')}</span>
              <input
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
                placeholder="My Provider"
              />
            </label>

            {vendors.length > 0 && !advanced && (
              <div className="vendor-picker">
                <label>
                  <span>{t('providers.modal.vendor')}</span>
                  <select
                    value={form.vendorId}
                    onChange={(e) => pickVendor(e.target.value)}
                  >
                    <option value="">{t('providers.modal.vendorManual')}</option>
                    {vendors.map((v) => (
                      <option key={v.id} value={v.id}>{v.name}</option>
                    ))}
                  </select>
                </label>
                {selectedVendor && selectedVendor.protocols.length > 1 && (
                  <label>
                    <span>{t('providers.modal.protocol')}</span>
                    <select
                      value={form.protocol}
                      onChange={(e) => pickProtocol(e.target.value)}
                    >
                      {selectedVendor.protocols.map((p) => (
                        <option key={p} value={p}>
                          {PROTOCOL_LABELS[p as Protocol] ?? p}
                          {APP_PROTOCOL[appType] === p ? ` (${t('providers.modal.protocolRecommended')})` : ''}
                        </option>
                      ))}
                    </select>
                  </label>
                )}
              </div>
            )}

            {!noFields && !advanced && (
              <div className="smart-form">
                {schema.fields.map((f) => (
                  <label key={f.key}>
                    <span>
                      {f.label}
                      {f.required && <small className="req"> *</small>}
                    </span>
                    {f.type === 'select' ? (
                      <select
                        value={form.fields[f.key] ?? ''}
                        onChange={(e) =>
                          setForm({
                            ...form,
                            fields: { ...form.fields, [f.key]: e.target.value },
                          })
                        }
                      >
                        <option value="">— select —</option>
                        {(f.options ?? []).map((o) => (
                          <option key={o.value} value={o.value}>{o.label}</option>
                        ))}
                      </select>
                    ) : (
                      <input
                        type={f.type === 'password' ? 'password' : 'text'}
                        value={form.fields[f.key] ?? ''}
                        onChange={(e) =>
                          setForm({
                            ...form,
                            fields: { ...form.fields, [f.key]: e.target.value },
                          })
                        }
                        placeholder={f.placeholder}
                      />
                    )}
                  </label>
                ))}
              </div>
            )}

            {noFields && (
              <div className="muted" style={{ marginBottom: 12 }}>
                {t('providers.modal.unsupported')}
              </div>
            )}

            <details className="json-preview">
              <summary>{t('providers.modal.preview')}</summary>
              <pre
                className="json-preview-block"
                dangerouslySetInnerHTML={{ __html: highlightJson(previewJson) }}
              />
            </details>

            <button type="button" className="btn link-btn" onClick={toggleAdvanced}>
              {advanced ? t('providers.modal.advancedOff') : t('providers.modal.advancedOn')}
            </button>

            {advanced && (
              <label>
                <span>{t('providers.modal.settingsConfig')}</span>
                <textarea
                  rows={10}
                  value={form.rawJson}
                  onChange={(e) => setForm({ ...form, rawJson: e.target.value })}
                  spellCheck={false}
                  className={rawValidation.ok ? '' : 'json-invalid'}
                />
                {!rawValidation.ok && (
                  <small className="json-error">{t('providers.invalidJson', { message: rawValidation.error ?? '' })}</small>
                )}
              </label>
            )}

            <div className="modal-actions">
              <button
                className="btn"
                onClick={() => {
                  setShowAdd(false);
                  setEditing(null);
                }}
              >
                {t('providers.modal.cancel')}
              </button>
              <button
                className="btn primary"
                onClick={handleAdd}
                disabled={!form.id || !form.name || (advanced && !rawValidation.ok)}
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