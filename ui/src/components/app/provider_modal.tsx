import { useT } from '../../lib/ctx';
import type { AppPageConfig } from '../../lib/appPages';
import type { UnifiedCard } from '../../lib/unified_providers';

/** Form state for the unified provider modal. `values` holds the per-app
 *  connection fields (cfg.fields); the upstream section holds the linked
 *  `ccs-*` pool row settings. For preset cards the upstream base
 *  URL/key derive from `values` at save time; only standalone cards edit
 *  them directly. */
export interface ProviderFormState {
  id: string;
  name: string;
  values: Record<string, string>;
  icon: string;
  iconColor: string;
  websiteUrl: string;
  notes: string;
  upstreamType: string;
  upstreamBaseURL: string;
  upstreamAPIKey: string;
  upstreamModels: string;
  /** Optional per-upstream model mapping as editable JSON text
   *  (display model -> upstream model). Empty keeps the stored mapping. */
  upstreamModelMapping: string;
  /** Optional per-upstream extra request headers as editable JSON text
   *  (header name -> value). Empty keeps the stored headers. */
  upstreamExtraHeaders: string;
}

interface ProviderModalProps {
  cfg: AppPageConfig;
  /** Card being edited, or null when creating. */
  editing: UnifiedCard | null;
  form: ProviderFormState;
  setForm: (f: ProviderFormState) => void;
  onSave: () => void;
  onClose: () => void;
}

/** Unified create/edit modal replacing the old preset modal and the
 *  upstream modal: Identity | per-app Connection fields | Gateway
 *  upstream section. State is owned by AppPage (README contract); this
 *  component is purely presentational. */
export function ProviderModal({
  cfg,
  editing,
  form,
  setForm,
  onSave,
  onClose,
}: ProviderModalProps) {
  const t = useT();
  const isStandalone = editing?.kind === 'standalone';
  const editingName =
    editing?.preset?.name || editing?.upstream?.name || editing?.key || '';

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} data-testid="provider-modal">
        <h3>
          {editing
            ? t('providers.modal.edit', { name: editingName })
            : t('providers.modal.add')}
        </h3>

        <p className="provider-modal-section">{t('providers.modal.name')}</p>
        <label>
          {t('providers.modal.name')}
          <input
            className="input"
            value={form.name}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
          />
        </label>
        <label>
          {t('providers.modal.id')}
          <input
            className="input"
            value={form.id}
            disabled={Boolean(editing)}
            placeholder={t('providers.modal.idAuto')}
            onChange={(e) => setForm({ ...form, id: e.target.value })}
            data-testid="provider-modal-id"
          />
        </label>
        <div className="provider-modal-row">
          <label>
            {t('providers.icon')}
            <input
              className="input"
              value={form.icon}
              onChange={(e) => setForm({ ...form, icon: e.target.value })}
            />
          </label>
          <label>
            {t('providers.iconColor')}
            <input
              type="color"
              className="provider-modal-color"
              value={form.iconColor || '#b3ac9f'}
              onChange={(e) => setForm({ ...form, iconColor: e.target.value })}
            />
          </label>
        </div>
        <div className="provider-modal-row">
          <label>
            {t('providers.website')}
            <input
              className="input"
              value={form.websiteUrl}
              onChange={(e) => setForm({ ...form, websiteUrl: e.target.value })}
            />
          </label>
          <label>
            {t('providers.notes')}
            <input
              className="input"
              value={form.notes}
              onChange={(e) => setForm({ ...form, notes: e.target.value })}
            />
          </label>
        </div>

        {/* Per-app connection fields (settings_config projection). */}
        {!isStandalone && (
          <>
            <p className="provider-modal-section">
              {t('providers.modal.section.connection')}
            </p>
            {cfg.fields.map((f) => (
              <label key={f.key}>
                {f.label}
                {f.type === 'select' ? (
                  <select
                    className="input"
                    value={form.values[f.key] ?? ''}
                    onChange={(e) =>
                      setForm({ ...form, values: { ...form.values, [f.key]: e.target.value } })
                    }
                  >
                    {(f.options ?? []).map((o) => (
                      <option key={o} value={o}>
                        {o}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    className="input"
                    type={f.type === 'password' ? 'password' : 'text'}
                    placeholder={f.placeholder}
                    value={form.values[f.key] ?? ''}
                    onChange={(e) =>
                      setForm({ ...form, values: { ...form.values, [f.key]: e.target.value } })
                    }
                  />
                )}
              </label>
            ))}
          </>
        )}

        <p className="provider-modal-section">
          {t('providers.modal.section.upstream')}
        </p>
        {!isStandalone && (
          <p className="provider-modal-hint">
            {t('providers.modal.upstream.linked', {
              id: `ccs-${form.id.trim() || '…'}`,
            })}
          </p>
        )}
        {isStandalone && (
          <>
            <label>
              {t('providers.modal.upstream.baseURL')}
              <input
                className="input"
                placeholder="https://api.deepseek.com/v1"
                value={form.upstreamBaseURL}
                onChange={(e) =>
                  setForm({ ...form, upstreamBaseURL: e.target.value })
                }
              />
            </label>
            <label>
              {t('providers.modal.upstream.apiKey')}
              <input
                className="input"
                type="password"
                value={form.upstreamAPIKey}
                onChange={(e) =>
                  setForm({ ...form, upstreamAPIKey: e.target.value })
                }
              />
            </label>
          </>
        )}
        <label>
          {t('providers.modal.upstream.type')}
          <select
            className="input"
            value={form.upstreamType}
            onChange={(e) => setForm({ ...form, upstreamType: e.target.value })}
          >
            <option value="anthropic">anthropic</option>
            <option value="anthropic_passthrough">anthropic passthrough</option>
            <option value="openai">openai</option>
            <option value="gemini">gemini</option>
            <option value="mock">mock</option>
          </select>
        </label>
        <label>
          {t('providers.modal.upstream.models')}
          <textarea
            className="input"
            rows={3}
            placeholder="model-a, model-b&#10;model-c"
            value={form.upstreamModels}
            onChange={(e) => setForm({ ...form, upstreamModels: e.target.value })}
          />
        </label>
        <label>
          {t('providers.modal.upstream.modelMapping')}
          <textarea
            className="input"
            rows={3}
            placeholder='{"gemini-2.5-pro": "gemini-2.5-pro-preview-06-05"}'
            value={form.upstreamModelMapping}
            onChange={(e) =>
              setForm({ ...form, upstreamModelMapping: e.target.value })
            }
          />
        </label>
        <label>
          {t('providers.modal.upstream.extraHeaders')}
          <textarea
            className="input"
            rows={3}
            placeholder='{"x-opencode-session": "…"}'
            value={form.upstreamExtraHeaders}
            onChange={(e) =>
              setForm({ ...form, upstreamExtraHeaders: e.target.value })
            }
          />
        </label>

        <div className="modal-actions">
          <button className="btn" onClick={onClose}>
            {t('common.cancel')}
          </button>
          <button className="btn primary" onClick={onSave} data-testid="provider-modal-save">
            {t('common.save')}
          </button>
        </div>
      </div>
    </div>
  );
}
