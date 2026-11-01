/// Supplier create/edit dialog.
///
/// Secret handling is write-only and explicit: a blank key field preserves the
/// stored value, clearing requires the dedicated "clear" action (which sends
/// `''`), and read views never carry key material.
///
/// The create path is a compact provider-first flow: API type, base URL, and
/// key up front; `supplier_discover_models` fetches the upstream catalog
/// before the first save; fetched ids become selectable drafts in the initial
/// endpoint's model inventory. Priority, icon, and endpoint overrides hide
/// behind the collapsed Advanced section. Saving sends the endpoint drafts —
/// models included — through `supplier_save`, so the dialog's draft list is
/// the endpoint's model truth; `supplier_models_update` remains the focused
/// models-tab editor.
///
/// A save result carrying `setupError` is still a successful save: the row is
/// persisted, the dialog adopts the returned supplier id, and retrying is an
/// edit that can never create a duplicate provider. Close then reloads and
/// surfaces the notice instead of reporting a failed save.
import { useMemo, useRef, useState } from 'react';
import { useCtx, useT } from '../../lib/ctx.js';
import { backend_error_text } from '../../lib/backend_error.js';
import {
  SUPPLIER_PROTOCOLS,
  build_discover_models_input,
  classify_supplier_error,
  endpoint_auth_fields,
  secret_write_value,
  sort_supplier_models,
  supplier_protocol_label_en,
  supplier_discovery_notice,
  validate_supplier_draft,
  type Secret_intent,
  type Supplier_auth_mode,
  type Supplier_endpoint_input,
  type Supplier_model_input,
  type Supplier_protocol,
  type Supplier_save_input,
  type Supplier_view,
} from '../../lib/api/suppliers.js';
import { supplier_delete, supplier_discover_models, supplier_models_update, supplier_save } from '../../lib/api/suppliers.js';
import { Icon_picker } from './client_management/icon_picker.js';
import { use_page_actions } from '../page_actions.js';
import { use_accessible_dialog } from '../use_accessible_dialog.js';
import { use_navigation_block } from '../../hooks/use_navigation_block.js';
import './suppliers_page.css';

type Key_intent_ui = 'keep' | 'replace' | 'clear';

interface Editor_operation_error {
  kind: string;
  code: string;
  discovery_failure: boolean;
}

/** Fallback shown in the color swatch when no tile color is stored; it
 *  matches the stylesheet's default `.provider-icon` background. */
const DEFAULT_ICON_COLOR = '#8d8578';

interface Flow_strings {
  choose_protocol: string;
  protocol_hint: string;
  fetch: string;
  fetch_hint: string;
  fetch_failed: string;
  discovered_title: string;
  discovered_empty: string;
  advanced: string;
  setup_error_title: string;
  setup_error_hint: string;
}

/** Localized copy for the focused provider setup flow. */
const FLOW_STRINGS: Record<'en' | 'zh', Flow_strings> = {
  en: {
    choose_protocol: 'Choose API type…',
    protocol_hint: 'The API type becomes this supplier’s first protocol endpoint.',
    fetch: 'Fetch models',
    fetch_hint: 'Fetch the advertised model list from the provider before saving. Editing reuses the stored key unless you replace it.',
    fetch_failed: 'Fetching models failed.',
    discovered_title: 'Fetched models — uncheck to exclude from this endpoint',
    discovered_empty: 'The provider returned no models.',
    advanced: 'Advanced',
    setup_error_title: 'Supplier saved, but automatic gateway setup failed.',
    setup_error_hint: 'The provider is stored. Save again to retry the automatic setup, or close to refresh the list.',
  },
  zh: {
    choose_protocol: '选择 API 类型…',
    protocol_hint: 'API 类型将成为该供应商的第一个协议端点。',
    fetch: '获取模型',
    fetch_hint: '保存前获取提供方公布的模型列表。编辑时默认复用已存储的密钥，除非你替换它。',
    fetch_failed: '获取模型失败。',
    discovered_title: '已获取的模型 — 取消勾选可将其排除',
    discovered_empty: '提供方未返回任何模型。',
    advanced: '高级',
    setup_error_title: '供应商已保存，但自动网关配置失败。',
    setup_error_hint: '提供方已保存。再次保存可重试自动配置，或关闭以刷新列表。',
  },
};

function secret_intent_of(ui: Key_intent_ui): Secret_intent {
  return ui === 'keep' ? 'preserve' : ui === 'clear' ? 'clear' : 'set';
}

interface Model_draft {
  key: number;
  modelId: string;
  displayName: string;
  enabled: boolean;
  supportsTools: string;
  supportsImages: string;
  contextWindow: string;
  maxOutputTokens: string;
  tierOverride: string;
}

interface Endpoint_draft {
  protocol: Supplier_protocol;
  endpointUrl: string;
  authMode: Supplier_auth_mode;
  keyIntent: Key_intent_ui;
  keyValue: string;
  headersText: string;
  headersDirty: boolean;
  enabled: boolean;
  priority: string;
  models: Model_draft[];
}

let draft_sequence = 0;

function empty_model(): Model_draft {
  draft_sequence += 1;
  return {
    key: draft_sequence,
    modelId: '',
    displayName: '',
    enabled: true,
    supportsTools: 'unknown',
    supportsImages: 'unknown',
    contextWindow: '',
    maxOutputTokens: '',
    tierOverride: 'auto',
  };
}

function new_endpoint_draft(protocol: Supplier_protocol): Endpoint_draft {
  return {
    protocol: protocol,
    endpointUrl: '',
    authMode: 'shared',
    keyIntent: 'keep',
    keyValue: '',
    headersText: '',
    headersDirty: false,
    enabled: true,
    priority: '100',
    models: [],
  };
}

function tri_read(value: boolean | null): string {
  return value === true ? 'true' : value === false ? 'false' : 'unknown';
}

function tri_write(value: string): boolean | null {
  return value === 'true' ? true : value === 'false' ? false : null;
}

function num_or_null(value: string): number | null {
  const trimmed = value.trim();
  if (trimmed === '') return null;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? parsed : null;
}

function models_from_view(supplier: Supplier_view, protocol: string): Model_draft[] {
  const endpoint = supplier.endpoints.find((item) => item.protocol === protocol);
  if (!endpoint) return [];
  return sort_supplier_models(endpoint.models).map((model) => {
    draft_sequence += 1;
    return {
      key: draft_sequence,
      modelId: model.modelId,
      displayName: model.displayName ?? '',
      enabled: model.enabled,
      supportsTools: tri_read(model.supportsTools),
      supportsImages: tri_read(model.supportsImages),
      contextWindow: model.contextWindow === null ? '' : String(model.contextWindow),
      maxOutputTokens: model.maxOutputTokens === null ? '' : String(model.maxOutputTokens),
      tierOverride: model.tierOverride === null ? 'auto' : String(model.tierOverride),
    };
  });
}

function endpoint_drafts_from_view(supplier: Supplier_view): Endpoint_draft[] {
  return SUPPLIER_PROTOCOLS.map((protocol): Endpoint_draft | null => {
    const endpoint = supplier.endpoints.find((item) => item.protocol === protocol);
    if (!endpoint) return null;
    return {
      protocol: protocol,
      endpointUrl: endpoint.endpointUrl ?? '',
      authMode: endpoint.authMode,
      keyIntent: 'keep',
      keyValue: '',
      headersText: endpoint.headerNames.join('\n'),
      headersDirty: false,
      enabled: endpoint.enabled,
      priority: String(endpoint.priority),
      models: models_from_view(supplier, protocol),
    };
  }).filter((draft): draft is Endpoint_draft => draft !== null);
}

function parse_headers(text: string): Record<string, string> {
  const headers: Record<string, string> = {};
  for (const line of text.split('\n')) {
    const trimmed = line.trim();
    if (trimmed === '') continue;
    const equal = trimmed.indexOf('=');
    if (equal <= 0) continue;
    headers[trimmed.slice(0, equal).trim()] = trimmed.slice(equal + 1);
  }
  return headers;
}

export interface Supplier_editor_props {
  /** `null` opens the create form. */
  supplier: Supplier_view | null;
  generation: number;
  delete_requested?: boolean;
  on_close: () => void;
  on_changed: (generation: number, message: string, close_editor?: boolean) => void;
  /** Reload the page list after a generation conflict or a create. */
  on_reload: () => void;
}

export function Supplier_editor(props: Supplier_editor_props) {
  const t = useT();
  const { locale } = useCtx();
  const L = FLOW_STRINGS[locale];
  const [tab, set_tab] = useState<'general' | 'endpoints' | 'models'>('general');
  const [name, set_name] = useState(props.supplier?.name ?? '');
  const [base_url, set_base_url] = useState(props.supplier?.baseUrl ?? '');
  const [enabled, set_enabled] = useState(props.supplier?.enabled ?? true);
  const [priority, set_priority] = useState(String(props.supplier?.priority ?? 100));
  const [key_intent, set_key_intent] = useState<Key_intent_ui>(
    props.supplier?.apiKeyConfigured ? 'keep' : 'replace',
  );
  const [key_value, set_key_value] = useState('');
  const [icon, set_icon] = useState(props.supplier?.icon ?? '');
  const [icon_color, set_icon_color] = useState(props.supplier?.iconColor ?? '');
  const [endpoints, set_endpoints] = useState<Endpoint_draft[]>(() =>
    props.supplier === null ? [] : endpoint_drafts_from_view(props.supplier!),
  );
  const [flow_protocol, set_flow_protocol] = useState<Supplier_protocol | ''>(() => {
    const first = props.supplier?.endpoints[0]?.protocol;
    return SUPPLIER_PROTOCOLS.find((protocol) => protocol === first) ?? '';
  });
  const [discovered, set_discovered] = useState<{ protocol: Supplier_protocol; ids: string[] } | null>(null);
  const [models_protocol, set_models_protocol] = useState<Supplier_protocol | ''>('');
  const [busy, set_busy] = useState(false);
  const operation_running = useRef(false);
  const [error, set_error] = useState<string | Editor_operation_error | null>(null);
  const [delete_open, set_delete_open] = useState(props.delete_requested ?? false);
  // Post-save setup failure state: the save itself succeeded, so the dialog
  // stays open editing the persisted row instead of reopening as a create.
  const [setup_error, set_setup_error] = useState<string | null>(null);
  const [adopted_id, set_adopted_id] = useState<string | null>(null);
  const [local_generation, set_local_generation] = useState<number | null>(null);
  const [local_key_configured, set_local_key_configured] = useState<boolean | null>(null);

  const draft = { name, base_url, enabled, priority, key_intent, key_value, icon, icon_color, endpoints };
  const initial_draft = useRef(draft);
  const draft_dirty = JSON.stringify(draft) !== JSON.stringify(initial_draft.current);
  const persisted_id = props.supplier?.id ?? adopted_id;
  const editing_existing = persisted_id !== null;
  const effective_generation = local_generation ?? props.generation;
  const key_configured = local_key_configured ?? props.supplier?.apiKeyConfigured ?? false;
  use_navigation_block(busy || draft_dirty, () => set_error(t('suppliers.editor.navigation_blocked')));
  const dialog_ref = use_accessible_dialog(() => {
    if (delete_open) set_delete_open(false);
    else request_close();
  }, busy);

  function update_endpoint(protocol: string, patch: Partial<Endpoint_draft>) {
    set_endpoints((current) =>
      current.map((endpoint) => (endpoint.protocol === protocol ? { ...endpoint, ...patch } : endpoint)),
    );
  }

  function update_model(protocol: string, key: number, patch: Partial<Model_draft>) {
    set_endpoints((current) =>
      current.map((endpoint) =>
        endpoint.protocol === protocol
          ? {
              ...endpoint,
              models: endpoint.models.map((model) => (model.key === key ? { ...model, ...patch } : model)),
            }
          : endpoint,
      ),
    );
  }

  function move_model(protocol: string, key: number, delta: number) {
    set_endpoints((current) =>
      current.map((endpoint) => {
        if (endpoint.protocol !== protocol) return endpoint;
        const index = endpoint.models.findIndex((model) => model.key === key);
        const target = index + delta;
        if (index < 0 || target < 0 || target >= endpoint.models.length) return endpoint;
        const models = [...endpoint.models];
        const [moved] = models.splice(index, 1);
        models.splice(target, 0, moved);
        return { ...endpoint, models };
      }),
    );
  }

  function select_flow_protocol(value: string) {
    const protocol = SUPPLIER_PROTOCOLS.find((item) => item === value) ?? '';
    set_flow_protocol(protocol);
    set_discovered(null);
    if (protocol !== '' && !endpoints.some((endpoint) => endpoint.protocol === protocol)) {
      set_endpoints((current) => [...current, new_endpoint_draft(protocol)]);
    }
  }

  function model_inputs(models: Model_draft[]): Supplier_model_input[] {
    // `SupplierModelInput` has no sortIndex: the list order is the order.
    return models
      .filter((model) => model.modelId.trim() !== '')
      .map((model) => ({
        modelId: model.modelId.trim(),
        displayName: model.displayName.trim() === '' ? null : model.displayName.trim(),
        enabled: model.enabled,
        supportsTools: tri_write(model.supportsTools),
        supportsImages: tri_write(model.supportsImages),
        contextWindow: num_or_null(model.contextWindow),
        maxOutputTokens: num_or_null(model.maxOutputTokens),
        tierOverride: model.tierOverride === 'auto' ? null : Number(model.tierOverride),
        source: 'manual',
      }));
  }

  function build_endpoint_inputs(): Supplier_endpoint_input[] {
    return endpoints.map((endpoint) => {
      const auth = endpoint_auth_fields({
        authMode: endpoint.authMode,
        keyIntent: secret_intent_of(endpoint.keyIntent),
        keyValue: endpoint.keyValue,
      });
      return {
        protocol: endpoint.protocol,
        endpointUrl: endpoint.endpointUrl,
        useSharedApiKey: auth.useSharedApiKey,
        ...(auth.apiKeyOverride === undefined ? {} : { apiKeyOverride: auth.apiKeyOverride }),
        enabled: endpoint.enabled,
        priority: Number(endpoint.priority) || 0,
        ...(endpoint.headersDirty ? { extraHeaders: parse_headers(endpoint.headersText) } : {}),
        // The draft list is the endpoint's model truth: fetched models join
        // manual edits in the initial inventory, and an edit persists
        // exactly what the dialog shows.
        models: model_inputs(endpoint.models),
      };
    });
  }

  async function perform(action: () => Promise<void>, discovery_failure = false) {
    if (operation_running.current) return;
    operation_running.current = true;
    set_busy(true);
    set_error(null);
    try {
      await action();
    } catch (cause) {
      const info = classify_supplier_error(cause);
      // Keep codes until rendering so composed messages follow the active locale.
      set_error({ kind: info.kind, code: info.text, discovery_failure });
      if (info.kind === 'generation_conflict') props.on_reload();
    } finally {
      operation_running.current = false;
      set_busy(false);
    }
  }

  function error_text(kind: string, text: string, discovery_failure = false): string {
    const message = backend_error_text(text, t);
    if (discovery_failure && (kind === 'code' || kind === 'unknown')) return `${L.fetch_failed} ${message}`;
    switch (kind) {
      case 'generation_conflict':
        return t('suppliers.status.apply_failed');
      case 'in_use':
        return t('suppliers.error.in_use');
      case 'malformed_vendor':
        return t('suppliers.error.malformed_vendor');
      case 'feature_unavailable':
        return t('suppliers.error.feature_unavailable');
      default:
        return t('suppliers.save_failed', { message });
    }
  }

  async function fetch_models() {
    const protocol = flow_protocol;
    if (operation_running.current || protocol === '') return;
    if (!endpoints.some((endpoint) => endpoint.protocol === protocol)) {
      set_endpoints((current) => [...current, new_endpoint_draft(protocol)]);
    }
    const endpoint = endpoints.find((item) => item.protocol === protocol) ?? null;
    await perform(async () => {
      const ids = await supplier_discover_models(
        build_discover_models_input({
          supplierId: persisted_id,
          protocol: protocol,
          baseUrl: base_url,
          endpointUrl: endpoint?.endpointUrl ?? '',
          apiKeyIntent: secret_intent_of(key_intent),
          apiKeyValue: key_value,
          authMode: endpoint?.authMode ?? 'shared',
          endpointKeyIntent: secret_intent_of(endpoint?.keyIntent ?? 'keep'),
          endpointKeyValue: endpoint?.keyValue ?? '',
          extraHeaders: endpoint !== null && endpoint.headersDirty ? parse_headers(endpoint.headersText) : null,
        }),
      );
      set_discovered({ protocol: protocol, ids: ids });
      // Fetched ids are selected by default: each becomes a model draft on
      // the endpoint unless already listed; unchecking removes the draft.
      set_endpoints((current) =>
        current.map((item) => {
          if (item.protocol !== protocol) return item;
          const present = new Set(item.models.map((model) => model.modelId));
          const additions = ids
            .filter((id) => !present.has(id))
            .map((id) => ({ ...empty_model(), modelId: id }));
          return { ...item, models: [...item.models, ...additions] };
        }),
      );
    }, true);
  }

  function toggle_discovered_model(id: string) {
    if (discovered === null) return;
    const protocol = discovered.protocol;
    set_endpoints((current) =>
      current.map((item) => {
        if (item.protocol !== protocol) return item;
        const listed = item.models.some((model) => model.modelId === id);
        return listed
          ? { ...item, models: item.models.filter((model) => model.modelId !== id) }
          : { ...item, models: [...item.models, { ...empty_model(), modelId: id }] };
      }),
    );
  }

  function reset_secret_intents() {
    // The typed keys were persisted by the successful save; a retry must
    // preserve them instead of re-sending (or clearing) the stored values.
    set_key_intent('keep');
    set_key_value('');
    set_endpoints((current) =>
      current.map((endpoint) =>
        endpoint.keyIntent === 'keep' && endpoint.keyValue === ''
          ? endpoint
          : { ...endpoint, keyIntent: 'keep', keyValue: '' },
      ),
    );
  }

  async function save() {
    const was_create = persisted_id === null;
    const draft_codes = validate_supplier_draft({
      name: name,
      baseUrl: base_url,
      priority: Number(priority) || 0,
      endpoints: endpoints.map((endpoint) => ({ protocol: endpoint.protocol, endpointUrl: endpoint.endpointUrl })),
    });
    if (draft_codes.length > 0) {
      set_error({ kind: 'unknown', code: draft_codes[0], discovery_failure: false });
      return;
    }
    await perform(async () => {
      // Write-only key: keep -> omit, clear -> '', replace -> the typed value.
      const secret_value = secret_write_value(secret_intent_of(key_intent), key_value);
      const payload: Supplier_save_input = {
        expectedGeneration: effective_generation,
        name: name.trim(),
        enabled: enabled,
        priority: Number(priority) || 0,
        baseUrl: base_url,
        ...(persisted_id === null ? {} : { id: persisted_id }),
        ...(secret_value === undefined ? {} : { apiKey: secret_value }),
        // Display fields are always sent: the state carries the stored
        // values on an untouched save, and `''` is the explicit clear.
        icon: icon,
        iconColor: icon_color,
        endpoints: build_endpoint_inputs(),
      };
      const result = await supplier_save(payload);
      const discovery_notice = supplier_discovery_notice(result.discoveryError, t);
      initial_draft.current = {
        ...draft,
        key_intent: 'keep',
        key_value: '',
        endpoints: endpoints.map((endpoint) => ({ ...endpoint, keyIntent: 'keep', keyValue: '' })),
      };
      set_local_generation(result.generation);
      set_local_key_configured(secret_value !== undefined ? secret_value !== '' : key_configured);
      reset_secret_intents();
      if (result.setupError != null && result.setupError !== '') {
        // The row is persisted; only the automatic gateway/mirror setup
        // failed. Stay open as an edit of the returned id (retry cannot
        // create a duplicate) and never present this as a failed save.
        set_setup_error(result.setupError);
        if (was_create) set_adopted_id(result.supplierId);
        props.on_reload();
        return;
      }
      set_setup_error(null);
      if (was_create) {
        props.on_changed(result.generation, t('suppliers.editor.created') + discovery_notice);
        props.on_reload();
      } else {
        props.on_changed(result.generation, t('suppliers.editor.saved', { generation: result.generation }) + discovery_notice);
      }
    });
  }

  async function save_models() {
    const protocol = models_protocol;
    if (protocol === '' || persisted_id === null) return;
    const endpoint = endpoints.find((item) => item.protocol === protocol);
    if (!endpoint) return;
    await perform(async () => {
      const result = await supplier_models_update({
        supplierId: persisted_id,
        protocol: protocol,
        expectedGeneration: effective_generation,
        models: model_inputs(endpoint.models),
      });
      // Only this protocol's models committed. General fields, credentials,
      // endpoint settings and sibling protocol drafts retain their baselines.
      initial_draft.current = {
        ...initial_draft.current,
        endpoints: initial_draft.current.endpoints.map((item) =>
          item.protocol === protocol ? { ...item, models: endpoint.models } : item,
        ),
      };
      set_local_generation(result.generation);
      if (result.setupError) {
        set_setup_error(result.setupError);
        props.on_reload();
        return;
      }
      set_setup_error(null);
      props.on_changed(result.generation, t('suppliers.editor.models_saved', { generation: result.generation }), false);
    });
  }

  async function remove_supplier() {
    if (persisted_id === null) return;
    const target_id = persisted_id;
    await perform(async () => {
      const result = await supplier_delete(target_id, effective_generation);
      const notice = t('suppliers.editor.deleted') + (result.setupError ? ` ${t('suppliers.setup_partial', { code: backend_error_text(result.setupError, t) })}` : '');
      props.on_changed(result.generation, notice);
      props.on_reload();
    });
  }

  function request_close() {
    if (operation_running.current) return;
    if (setup_error !== null) {
      // Closing after a setup failure still reloads the list and surfaces
      // the notice: the supplier itself was saved.
      props.on_changed(effective_generation, `${L.setup_error_title} ${backend_error_text(setup_error, t)}`);
      return;
    }
    props.on_close();
  }

  const models_endpoint = useMemo(
    () => endpoints.find((endpoint) => endpoint.protocol === models_protocol) ?? null,
    [endpoints, models_protocol],
  );

  const flow_endpoint = endpoints.find((endpoint) => endpoint.protocol === flow_protocol) ?? null;

  /** Per-protocol endpoint controls, shared by the endpoints tab (all
   *  endpoints) and the Advanced section (the flow's endpoint). */
  function endpoint_fields(endpoint: Endpoint_draft) {
    return (
      <>
        <label className="supplier_check">
          <input type="checkbox" checked={endpoint.enabled} disabled={busy} data-testid={`suppliers_endpoint_enabled_${endpoint.protocol}`} onChange={(event) => update_endpoint(endpoint.protocol, { enabled: event.target.checked })} />
          {t('suppliers.endpoints.enabled')}
        </label>
        <label className="supplier_field">
          <span>{t('suppliers.endpoints.url_override')}</span>
          <input value={endpoint.endpointUrl} disabled={busy} data-testid={`suppliers_endpoint_url_${endpoint.protocol}`} placeholder={base_url || 'https://'} onChange={(event) => update_endpoint(endpoint.protocol, { endpointUrl: event.target.value })} />
          <small className="supplier_hint">{t('suppliers.endpoints.url_override_hint')}</small>
        </label>
        <label className="supplier_field">
          <span>{t('suppliers.endpoints.priority')}</span>
          <input type="number" value={endpoint.priority} disabled={busy} onChange={(event) => update_endpoint(endpoint.protocol, { priority: event.target.value })} />
        </label>
        <fieldset className="supplier_fieldset">
          <legend>{t('suppliers.endpoints.auth_label')}</legend>
          {(['shared', 'override', 'none'] as const).map((mode) => (
            <label className="supplier_check" key={mode}>
              <input
                type="radio"
                name={`supplier_auth_${endpoint.protocol}`}
                checked={endpoint.authMode === mode}
                disabled={busy}
                data-testid={`suppliers_endpoint_auth_${mode}_${endpoint.protocol}`}
                onChange={() => update_endpoint(endpoint.protocol, { authMode: mode })}
              />
              {t(`suppliers.endpoints.auth.${mode}`)}
            </label>
          ))}
          {endpoint.authMode === 'override' && (
            <div className="supplier_field">
              <label className="supplier_check">
                <input type="radio" name={`supplier_key_${endpoint.protocol}`} checked={endpoint.keyIntent === 'keep'} disabled={busy} onChange={() => update_endpoint(endpoint.protocol, { keyIntent: 'keep' })} />
                {t('suppliers.editor.api_key.keep')}
              </label>
              <label className="supplier_check">
                <input type="radio" name={`supplier_key_${endpoint.protocol}`} checked={endpoint.keyIntent === 'replace'} disabled={busy} onChange={() => update_endpoint(endpoint.protocol, { keyIntent: 'replace' })} />
                {t('suppliers.editor.api_key.replace')}
              </label>
              {endpoint.keyIntent === 'replace' && (
                <input
                  type="password"
                  autoComplete="off"
                  value={endpoint.keyValue}
                  disabled={busy}
                  data-testid={`suppliers_endpoint_key_${endpoint.protocol}`}
                  placeholder={t('suppliers.editor.api_key.placeholder')}
                  onChange={(event) => update_endpoint(endpoint.protocol, { keyValue: event.target.value })}
                />
              )}
              <label className="supplier_check">
                <input type="radio" name={`supplier_key_${endpoint.protocol}`} checked={endpoint.keyIntent === 'clear'} disabled={busy} onChange={() => update_endpoint(endpoint.protocol, { keyIntent: 'clear' })} />
                {t('suppliers.editor.api_key.clear')}
              </label>
            </div>
          )}
        </fieldset>
        <label className="supplier_field">
          <span>{t('suppliers.endpoints.headers')}</span>
          <textarea
            rows={3}
            value={endpoint.headersText}
            disabled={busy}
            data-testid={`suppliers_endpoint_headers_${endpoint.protocol}`}
            onChange={(event) => update_endpoint(endpoint.protocol, { headersText: event.target.value, headersDirty: true })}
          />
          <small className="supplier_hint">{t('suppliers.endpoints.headers_hint')}</small>
        </label>
      </>
    );
  }

  use_page_actions({ target: (delete_open ? 'Confirm delete: ' : '') + (name || 'New provider'),
    state: busy ? 'busy' : error || setup_error ? 'error' : delete_open ? 'idle' : draft_dirty ? 'dirty' : 'clean',
    on_save: delete_open ? undefined : save,
    on_delete: editing_existing ? () => delete_open ? remove_supplier() : set_delete_open(true) : undefined,
    save_disabled: busy, delete_disabled: busy }, delete_open ? 40 : 20);
  return (
    <div className="supplier_backdrop" role="presentation">
      <div ref={dialog_ref} tabIndex={-1} className="supplier_dialog supplier_editor_dialog" role="dialog" aria-modal="true" aria-label={editing_existing ? t('suppliers.editor.edit_title') : t('suppliers.editor.create_title')} data-testid="suppliers_editor" data-dirty={draft_dirty}>
        <div className="supplier_dialog_heading">
          <h2>{editing_existing ? t('suppliers.editor.edit_title') : t('suppliers.editor.create_title')}</h2>
          <button type="button" className="btn small" disabled={busy} onClick={request_close} data-testid="suppliers_editor_close">
            {t('suppliers.editor.close')}
          </button>
        </div>
        {setup_error !== null && (
          <div role="status" className="supplier_notice warn" data-testid="suppliers_editor_setup_error">
            <strong>{L.setup_error_title}</strong>
            <p>{backend_error_text(setup_error, t)}</p>
            <p className="supplier_hint">{L.setup_error_hint}</p>
          </div>
        )}
        {error && <div role="alert" className="supplier_error" data-testid="suppliers_editor_error">{typeof error === 'string' ? backend_error_text(error, t) : error_text(error.kind, error.code, error.discovery_failure)}</div>}
        {draft_dirty && <p role="status" className="supplier_hint" data-testid="suppliers_editor_dirty">{t('suppliers.editor.unsaved_changes')}</p>}
        <div className="supplier_tabs" role="tablist">
          {(['general', 'endpoints', 'models'] as const).map((item) => (
            <button
              key={item}
              type="button"
              role="tab"
              aria-selected={tab === item}
              className={tab === item ? 'supplier_tab active' : 'supplier_tab'}
              data-testid={`suppliers_tab_${item}`}
              onClick={() => {
                set_tab(item);
                if (item === 'models' && models_protocol === '') {
                  set_models_protocol(endpoints[0]?.protocol ?? '');
                }
              }}
            >
              {t(`suppliers.editor.tab.${item}`)}
            </button>
          ))}
        </div>

        {tab === 'general' && (
          <div className="supplier_form" data-testid="suppliers_tab_general_panel">
            <label className="supplier_field">
              <span>{t('suppliers.editor.name')}</span>
              <input data-testid="suppliers_editor_name" value={name} disabled={busy} onChange={(event) => set_name(event.target.value)} />
            </label>
            <label className="supplier_field">
              <span>{t('suppliers.endpoints.protocol')}</span>
              <select
                value={flow_protocol}
                disabled={busy}
                data-testid="suppliers_editor_protocol"
                onChange={(event) => select_flow_protocol(event.target.value)}
              >
                <option value="">{L.choose_protocol}</option>
                {SUPPLIER_PROTOCOLS.map((protocol) => (
                  <option key={protocol} value={protocol}>
                    {supplier_protocol_label_en(protocol)}
                  </option>
                ))}
              </select>
              <small className="supplier_hint">{L.protocol_hint}</small>
            </label>
            <label className="supplier_field">
              <span>{t('suppliers.editor.base_url')}</span>
              <input data-testid="suppliers_editor_base_url" value={base_url} disabled={busy} placeholder="https://" onChange={(event) => set_base_url(event.target.value)} />
              <small className="supplier_hint">{t('suppliers.editor.base_url_hint')}</small>
            </label>
            <fieldset className="supplier_fieldset">
              <legend>{t('suppliers.editor.api_key')}</legend>
              {editing_existing && <><p className="supplier_hint" data-testid="suppliers_editor_key_state">
                {key_configured ? t('suppliers.editor.api_key.configured_hint') : t('suppliers.editor.api_key.none_hint')}
              </p>
              <label className="supplier_check">
                <input type="radio" name="supplier_key_intent" data-testid="suppliers_editor_key_keep" checked={key_intent === 'keep'} disabled={busy} onChange={() => set_key_intent('keep')} />
                {t('suppliers.editor.api_key.keep')}
              </label>
              <label className="supplier_check">
                <input type="radio" name="supplier_key_intent" data-testid="suppliers_editor_key_replace" checked={key_intent === 'replace'} disabled={busy} onChange={() => set_key_intent('replace')} />
                {t('suppliers.editor.api_key.replace')}
              </label>
              </>}
              {(!editing_existing || key_intent === 'replace') && (
                <input
                  type="password"
                  aria-label={t('suppliers.editor.api_key')}
                  autoComplete="off"
                  data-testid="suppliers_editor_key_value"
                  value={key_value}
                  disabled={busy}
                  placeholder={t('suppliers.editor.api_key.placeholder')}
                  onChange={(event) => set_key_value(event.target.value)}
                />
              )}
              {editing_existing && <label className="supplier_check">
                <input type="radio" name="supplier_key_intent" data-testid="suppliers_editor_key_clear" checked={key_intent === 'clear'} disabled={busy} onChange={() => set_key_intent('clear')} />
                {t('suppliers.editor.api_key.clear')}
              </label>}
            </fieldset>
            <div className="supplier_fetch_row">
              <button type="button" className="btn" disabled={busy || flow_protocol === ''} data-testid="suppliers_editor_fetch_models" onClick={() => void fetch_models()}>
                {L.fetch}
              </button>
              <small className="supplier_hint">{L.fetch_hint}</small>
            </div>
            {discovered !== null && (
              <div className="supplier_discovered" data-testid="suppliers_discovered">
                <span>{L.discovered_title}</span>
                {discovered.ids.length === 0 ? (
                  <p className="supplier_hint">{L.discovered_empty}</p>
                ) : (
                  <ul>
                    {discovered.ids.map((id, index) => {
                      const listed =
                        endpoints.find((item) => item.protocol === discovered.protocol)?.models.some(
                          (model) => model.modelId === id,
                        ) ?? false;
                      return (
                        <li key={id}>
                          <label className="supplier_check">
                            <input
                              type="checkbox"
                              checked={listed}
                              disabled={busy}
                              data-testid={`suppliers_discovered_${index}`}
                              onChange={() => toggle_discovered_model(id)}
                            />
                            {id}
                          </label>
                        </li>
                      );
                    })}
                  </ul>
                )}
              </div>
            )}
            <details className="supplier_advanced" data-testid="suppliers_editor_advanced">
              <summary>{L.advanced}</summary>
              <div className="supplier_advanced_body">
                <label className="supplier_check">
                  <input type="checkbox" data-testid="suppliers_editor_enabled" checked={enabled} disabled={busy} onChange={(event) => set_enabled(event.target.checked)} />
                  {t('suppliers.editor.enabled')}
                </label>
                <label className="supplier_field">
                  <span>{t('suppliers.editor.priority')}</span>
                  <input type="number" data-testid="suppliers_editor_priority" value={priority} disabled={busy} onChange={(event) => set_priority(event.target.value)} />
                  <small className="supplier_hint">{t('suppliers.editor.priority_hint')}</small>
                </label>
                <div className="supplier_field">
                  <span>{t('suppliers.editor.icon')}</span>
                  <div className="supplier_icon_row">
                    <input
                      data-testid="suppliers_editor_icon"
                      value={icon}
                      disabled={busy}
                      placeholder={t('suppliers.editor.icon_placeholder')}
                      onChange={(event) => set_icon(event.target.value)}
                    />
                    <input
                      type="color"
                      className="supplier_icon_color"
                      data-testid="suppliers_editor_icon_color"
                      aria-label={t('suppliers.editor.icon_color')}
                      value={icon_color || DEFAULT_ICON_COLOR}
                      disabled={busy}
                      onChange={(event) => set_icon_color(event.target.value)}
                    />
                    <Icon_picker
                      icon={icon}
                      color={icon_color}
                      name={name || t('suppliers.editor.icon_preview_name')}
                      onPick={(next) => set_icon(next)}
                    />
                  </div>
                  <small className="supplier_hint">{t('suppliers.editor.icon_hint')}</small>
                  {icon_color !== '' && (
                    <button
                      type="button"
                      className="btn small supplier_icon_color_clear"
                      disabled={busy}
                      data-testid="suppliers_editor_icon_color_clear"
                      onClick={() => set_icon_color('')}
                    >
                      {t('suppliers.editor.icon_color_clear')}
                    </button>
                  )}
                </div>
                {editing_existing && (
                  <label className="supplier_field">
                    <span>{t('suppliers.editor.id')}</span>
                    <input data-testid="suppliers_editor_id" value={persisted_id} readOnly aria-readonly="true" />
                    <small className="supplier_hint">{t('suppliers.editor.id_hint')}</small>
                  </label>
                )}
                {flow_endpoint !== null && (
                  <fieldset className="supplier_fieldset" data-testid={`suppliers_flow_endpoint_${flow_endpoint.protocol}`}>
                    <legend>{supplier_protocol_label_en(flow_endpoint.protocol)}</legend>
                    {endpoint_fields(flow_endpoint)}
                  </fieldset>
                )}
              </div>
            </details>
          </div>
        )}

        {tab === 'endpoints' && (
          <div className="supplier_form" data-testid="suppliers_tab_endpoints_panel">
            <p className="supplier_hint">{t('suppliers.endpoints.legacy_note')}</p>
            {endpoints.length === 0 && <p className="supplier_empty_note">{t('suppliers.endpoints.empty')}</p>}
            {endpoints.map((endpoint) => (
              <fieldset className="supplier_fieldset" key={endpoint.protocol} data-testid={`suppliers_endpoint_${endpoint.protocol}`}>
                <legend>{supplier_protocol_label_en(endpoint.protocol)}</legend>
                {endpoint_fields(endpoint)}
              </fieldset>
            ))}
            <div className="supplier_row">
              {SUPPLIER_PROTOCOLS.filter((protocol) => !endpoints.some((endpoint) => endpoint.protocol === protocol)).map((protocol) => (
                <button
                  key={protocol}
                  type="button"
                  className="btn"
                  disabled={busy}
                  data-testid={`suppliers_endpoint_add_${protocol}`}
                  onClick={() =>
                    set_endpoints((current) => [
                      ...current,
                      new_endpoint_draft(protocol),
                    ])
                  }
                >
                  + {supplier_protocol_label_en(protocol)}
                </button>
              ))}
            </div>
            {endpoints.map((endpoint) => (
              <button
                key={`remove-${endpoint.protocol}`}
                type="button"
                className="btn small"
                disabled={busy}
                data-testid={`suppliers_endpoint_remove_${endpoint.protocol}`}
                onClick={() => set_endpoints((current) => current.filter((item) => item.protocol !== endpoint.protocol))}
              >
                {t('suppliers.models.remove')} · {supplier_protocol_label_en(endpoint.protocol)}
              </button>
            ))}
          </div>
        )}

        {tab === 'models' && (
          <div className="supplier_form" data-testid="suppliers_tab_models_panel">
            {editing_existing ? (
              <>
                <label className="supplier_field">
                  <span>{t('suppliers.models.endpoint_label')}</span>
                  <select
                    value={models_protocol}
                    disabled={busy}
                    data-testid="suppliers_models_endpoint"
                    onChange={(event) => set_models_protocol(event.target.value as Supplier_protocol | '')}
                  >
                    {endpoints.map((endpoint) => (
                      <option key={endpoint.protocol} value={endpoint.protocol}>
                        {supplier_protocol_label_en(endpoint.protocol)}
                      </option>
                    ))}
                  </select>
                </label>
                {models_endpoint && (
                  <>
                    <table className="supplier_models_table">
                      <thead>
                        <tr>
                          <th>{t('suppliers.models.model_id')}</th>
                          <th>{t('suppliers.models.display_name')}</th>
                          <th>{t('suppliers.models.enabled')}</th>
                          <th>{t('suppliers.models.tools')}</th>
                          <th>{t('suppliers.models.images')}</th>
                          <th>{t('suppliers.models.context')}</th>
                          <th>{t('suppliers.models.max_output')}</th>
                          <th>{t('suppliers.models.tier')}</th>
                          <th />
                        </tr>
                      </thead>
                      <tbody>
                        {models_endpoint.models.map((model, index) => (
                          <tr key={model.key}>
                            <td>
                              <input data-testid={`suppliers_model_id_${model.key}`} value={model.modelId} disabled={busy} onChange={(event) => update_model(models_endpoint.protocol, model.key, { modelId: event.target.value })} />
                            </td>
                            <td>
                              <input value={model.displayName} disabled={busy} onChange={(event) => update_model(models_endpoint.protocol, model.key, { displayName: event.target.value })} />
                            </td>
                            <td>
                              <input type="checkbox" checked={model.enabled} disabled={busy} onChange={(event) => update_model(models_endpoint.protocol, model.key, { enabled: event.target.checked })} />
                            </td>
                            <td>
                              <select value={model.supportsTools} disabled={busy} onChange={(event) => update_model(models_endpoint.protocol, model.key, { supportsTools: event.target.value })}>
                                <option value="unknown">?</option>
                                <option value="true">✓</option>
                                <option value="false">✗</option>
                              </select>
                            </td>
                            <td>
                              <select value={model.supportsImages} disabled={busy} onChange={(event) => update_model(models_endpoint.protocol, model.key, { supportsImages: event.target.value })}>
                                <option value="unknown">?</option>
                                <option value="true">✓</option>
                                <option value="false">✗</option>
                              </select>
                            </td>
                            <td>
                              <input type="number" value={model.contextWindow} disabled={busy} onChange={(event) => update_model(models_endpoint.protocol, model.key, { contextWindow: event.target.value })} />
                            </td>
                            <td>
                              <input type="number" value={model.maxOutputTokens} disabled={busy} onChange={(event) => update_model(models_endpoint.protocol, model.key, { maxOutputTokens: event.target.value })} />
                            </td>
                            <td>
                              <select value={model.tierOverride} disabled={busy} data-testid={`suppliers_model_tier_${model.key}`} onChange={(event) => update_model(models_endpoint.protocol, model.key, { tierOverride: event.target.value })}>
                                <option value="auto">{t('suppliers.models.tier_auto')}</option>
                                <option value="0">{t('suppliers.models.tier_0')}</option>
                                <option value="1">{t('suppliers.models.tier_1')}</option>
                                <option value="2">{t('suppliers.models.tier_2')}</option>
                                <option value="3">{t('suppliers.models.tier_3')}</option>
                              </select>
                            </td>
                            <td className="supplier_model_actions">
                              <button type="button" className="btn small" disabled={busy || index === 0} aria-label={t('suppliers.models.move_up')} onClick={() => move_model(models_endpoint.protocol, model.key, -1)}>↑</button>
                              <button type="button" className="btn small" disabled={busy || index === models_endpoint.models.length - 1} aria-label={t('suppliers.models.move_down')} onClick={() => move_model(models_endpoint.protocol, model.key, 1)}>↓</button>
                              <button type="button" className="btn small" disabled={busy} data-testid={`suppliers_model_remove_${model.key}`} onClick={() => update_endpoint(models_endpoint.protocol, { models: models_endpoint.models.filter((item) => item.key !== model.key) })}>
                                {t('suppliers.models.remove')}
                              </button>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                    <p className="supplier_hint">{t('suppliers.models.model_id_hint')}</p>
                    <div className="supplier_row">
                      <button type="button" className="btn" disabled={busy} data-testid="suppliers_model_add" onClick={() => update_endpoint(models_endpoint.protocol, { models: [...models_endpoint.models, empty_model()] })}>
                        + {t('suppliers.models.add')}
                      </button>
                      <button type="button" className="btn primary" disabled={busy} data-testid="suppliers_models_save" onClick={() => void save_models()}>
                        {t('suppliers.editor.save_models')}
                      </button>
                    </div>
                  </>
                )}
              </>
            ) : (
              <p className="supplier_empty_note">{t('suppliers.models.empty')}</p>
            )}
          </div>
        )}

        <div className="supplier_dialog_footer">
          {editing_existing && (
            <button type="button" className="btn danger" disabled={busy} data-testid="suppliers_editor_delete" onClick={() => set_delete_open(true)}>
              {t('suppliers.card.delete')}
            </button>
          )}
          <div className="supplier_row">
            <button type="button" className="btn" disabled={busy} data-testid="suppliers_editor_cancel" onClick={request_close}>
              {t('suppliers.editor.cancel')}
            </button>
            <button type="button" className="btn primary" disabled={busy} data-testid="suppliers_editor_save" onClick={() => void save()}>
              {t('suppliers.editor.save')}
            </button>
          </div>
        </div>

        {delete_open && editing_existing && (
          <Supplier_delete_dialog busy={busy} name={name || persisted_id || ''}
            on_cancel={() => { if (!operation_running.current) set_delete_open(false); }}
            on_confirm={() => void remove_supplier()} />
        )}
      </div>
    </div>
  );
}

function Supplier_delete_dialog(props: { busy: boolean; name: string; on_cancel: () => void; on_confirm: () => void }) {
  const t = useT();
  const dialog_ref = use_accessible_dialog(props.on_cancel, props.busy);
  return (
    <div ref={dialog_ref} tabIndex={-1} className="supplier_dialog nested" role="alertdialog" aria-modal="true" aria-label={t('suppliers.editor.delete_title')} data-testid="suppliers_delete_dialog">
      <h2>{t('suppliers.editor.delete_title')}</h2>
      <p>{t('suppliers.editor.delete_body', { name: props.name })}</p>
      <div className="supplier_row">
        <button type="button" className="btn" disabled={props.busy} data-testid="suppliers_delete_cancel" onClick={props.on_cancel}>
          {t('suppliers.editor.cancel')}
        </button>
        <button type="button" className="btn danger" disabled={props.busy} data-testid="suppliers_delete_confirm" onClick={props.on_confirm}>
          {t('suppliers.editor.delete_confirm')}
        </button>
      </div>
    </div>
  );
}
