/// Prompt-enhancement page.
///
/// Owns the small two-step orchestration the backend deliberately does not:
/// expand the short Chinese requirement, show the resulting Chinese brief so it
/// can be edited, then render that exact text as one optimized English prompt.
/// History and the selected backend live in localStorage, so the feature needs
/// no table, no migration, and no server-side content retention.
import { useEffect, useMemo, useRef, useState } from 'react';
import { useCtx } from '../../lib/ctx';
import {
  getPromptDefaults,
  prompt_error,
  prompt_recovery_message,
  runPromptStage,
  type Prompt_defaults,
  type Prompt_error,
  type Prompt_target,
} from '../../lib/api/prompt.js';
import {
  getProxyStatus,
  listProxyProviders,
  listRoutes,
  startProxy,
  type ProxyProvider,
  type Route,
} from '../../lib/api/proxy.js';
import { settingsGet, settingsSet } from '../../lib/api/codexCli.js';
import './prompt_page.css';
import { Prompt_workspace_panel } from './prompt_workspace_panel.js';
import { use_prompt_workspaces } from './use_prompt_workspaces.js';
import { workspace_error_message, type Workspace_context_preview, type Workspace_context_selection } from '../../lib/api/prompt_workspace.js';
import { use_page_actions } from '../page_actions.js';
import { use_navigation_block } from '../../hooks/use_navigation_block.js';

type Stage_status = 'idle' | 'running' | 'done' | 'error';

interface Target_option {
  key: string;
  label: string;
  group: 'gateway' | 'provider';
  target: Prompt_target;
}

interface History_entry {
  id: string;
  created_at: number;
  input: string;
  draft: string;
  prompt: string;
  target_label: string;
}

const TARGET_STORAGE_KEY = 'aag.prompt.target';
const HISTORY_STORAGE_KEY = 'aag.prompt.history';
const HISTORY_LIMIT = 8;
const FALLBACK_INPUT_LIMIT = 32000;
const DRAFT_STORAGE_KEY = 'aag.prompt.current_draft';

function load_current_draft(): { input: string; instruction: string; draft: string; prompt: string; interrupted: boolean; zh_status: Stage_status; en_status: Stage_status; error_code: string | null } | null {
  try {
    const saved = JSON.parse(sessionStorage.getItem(DRAFT_STORAGE_KEY) ?? 'null');
    if (!saved || !['input', 'draft', 'prompt'].every((key) => typeof saved[key] === 'string')) return null;
    const stage_status = (value: unknown): Stage_status => value === 'done' ? 'done' : value === 'running' || value === 'error' ? 'error' : 'idle';
    return { input: saved.input, instruction: typeof saved.instruction === 'string' ? saved.instruction : '', draft: saved.draft, prompt: saved.prompt, interrupted: saved.running === true, zh_status: stage_status(saved.zh_status), en_status: stage_status(saved.en_status), error_code: typeof saved.error_code === 'string' ? saved.error_code : null };
  } catch { return null; }
}

function load_history(): History_entry[] {
  try {
    const raw = window.localStorage.getItem(HISTORY_STORAGE_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((entry): entry is History_entry =>
        Boolean(entry) && typeof (entry as History_entry).prompt === 'string')
      .slice(0, HISTORY_LIMIT);
  } catch {
    return [];
  }
}

function persist_history(entries: History_entry[]) {
  try {
    window.localStorage.setItem(HISTORY_STORAGE_KEY, JSON.stringify(entries));
  } catch {
    /* quota or disabled storage: history is a convenience, never a source of truth */
  }
}

function load_target_key(): string {
  try {
    return window.localStorage.getItem(TARGET_STORAGE_KEY) ?? '';
  } catch {
    return '';
  }
}

function persist_target_key(key: string) {
  try {
    window.localStorage.setItem(TARGET_STORAGE_KEY, key);
  } catch {
    /* ignore */
  }
}

export function Prompt_page() {
  const { t } = useCtx();
  const workspaces = use_prompt_workspaces();
  const [context_receipt, set_context_receipt] = useState<string | null>(null);
  const task_context_label = useRef<string | null>(null);
  const draft_context = useRef<{ selection?: Workspace_context_selection; label: string } | null>(null);
  const [defaults, set_defaults] = useState<Prompt_defaults | null>(null);
  const [routes, set_routes] = useState<Route[]>([]);
  const [providers, set_providers] = useState<ProxyProvider[]>([]);
  const [gateway_running, set_gateway_running] = useState(false);
  const [target_key, set_target_key] = useState<string>(load_target_key);
  const [recovered] = useState(load_current_draft);
  const [input, set_input] = useState(recovered?.input ?? '');
  const [instruction, set_instruction] = useState(recovered?.instruction ?? '');
  const [draft, set_draft] = useState(recovered?.draft ?? '');
  const [prompt, set_prompt] = useState(recovered?.prompt ?? '');
  const [zh_status, set_zh_status] = useState<Stage_status>(recovered?.zh_status ?? 'idle');
  const [en_status, set_en_status] = useState<Stage_status>(recovered?.en_status ?? 'idle');
  const [error, set_error] = useState<Prompt_error | null>(recovered?.error_code ? prompt_error(recovered.error_code) : null);
  const [notice, set_notice] = useState<string | null>(recovered?.interrupted ? t('prompt.recovered_interrupted') : null);
  const [history, set_history] = useState<History_entry[]>(load_history);
  const [served, set_served] = useState<{ provider: string | null; model: string; elapsed_ms: number; tokens: string | null } | null>(null);
  const [zh_template, set_zh_template] = useState('');
  const [en_template, set_en_template] = useState('');
  const [saving_templates, set_saving_templates] = useState(false);
  const [advanced_open, set_advanced_open] = useState(false);
  const [saved_templates, set_saved_templates] = useState({ zh: '', en: '' });
  const mounted = useRef(false);
  const sequence = useRef(0);
  const idempotency_key = useRef<string | null>(null);
  const operation_active = useRef(false);
  const [task_busy, set_task_busy] = useState(false);
  const retry_stage = useRef<'zh_expand' | 'en_optimize' | null>(null);
  const retry_english = useRef<{ source: string; draft: string; context?: Workspace_context_selection } | null>(null);

  const options = useMemo<Target_option[]>(() => {
    const collected: Target_option[] = [];
    for (const route of routes) {
      if (!route.alias.trim()) continue;
      collected.push({
        key: 'gateway:' + route.alias,
        label: route.alias,
        group: 'gateway',
        target: { kind: 'gateway', model: route.alias },
      });
    }
    for (const provider of providers) {
      if (!provider.enabled) continue;
      for (const model of provider.models) {
        if (!model.trim()) continue;
        collected.push({
          key: 'provider:' + provider.id + ':' + model,
          label: provider.name + ' - ' + model,
          group: 'provider',
          target: { kind: 'provider', providerId: provider.id, model },
        });
      }
    }
    return collected;
  }, [routes, providers]);

  const selected = useMemo(
    () => options.find((option) => option.key === target_key) ?? options[0] ?? null,
    [options, target_key],
  );

  const input_limit = defaults?.maxShortInputChars ?? FALLBACK_INPUT_LIMIT;
  const input_over_limit = input.length > input_limit;
  const busy = task_busy || zh_status === 'running' || en_status === 'running';
  const templates_dirty = zh_template !== saved_templates.zh || en_template !== saved_templates.en;
  use_navigation_block(busy || saving_templates || templates_dirty || workspaces.dirty, () => set_notice(t(busy || saving_templates ? 'prompt.navigation_blocked' : 'prompt.unsaved_settings')));

  useEffect(() => {
    try { sessionStorage.setItem(DRAFT_STORAGE_KEY, JSON.stringify({ input, instruction, draft, prompt, running: busy, zh_status, en_status, error_code: error?.code ?? null })); }
    catch { /* Storage availability does not change generation ownership. */ }
  }, [input, instruction, draft, prompt, busy, zh_status, en_status, error?.code]);

  use_page_actions(workspaces.managing ? {
    target: workspaces.draft_id ? `Workspace ${workspaces.registry?.workspaces.find((item) => item.id === workspaces.draft_id)?.name ?? 'settings'}` : 'New workspace',
    state: workspaces.loading || workspaces.operation ? 'busy' : workspaces.error ? 'error' : workspaces.dirty ? 'dirty' : workspaces.selected ? 'clean' : 'idle',
    on_save: async () => { if (!await workspaces.save()) throw new Error('Workspace save failed'); },
    on_delete: async () => { if (!await workspaces.remove()) throw new Error('Workspace removal failed'); },
    save_disabled: busy || !workspaces.dirty || !workspaces.name.trim() || !workspaces.root.trim(),
    delete_disabled: busy || !workspaces.selected || workspaces.draft_id !== workspaces.selected.id,
  } : null, 10);
  use_page_actions(advanced_open ? {
    target: 'Prompt templates', state: saving_templates ? 'busy' : templates_dirty ? 'dirty' : 'clean',
    on_save: async () => { if (!await save_templates()) throw new Error('Template save failed'); },
    save_disabled: busy || saving_templates || !defaults || !templates_dirty,
  } : null, 5);

  useEffect(() => {
    mounted.current = true;
    void (async () => {
      try {
        const [next_defaults, next_routes, next_providers, status] = await Promise.all([
          getPromptDefaults(),
          listRoutes(),
          listProxyProviders(),
          getProxyStatus(),
        ]);
        const stored_zh = await settingsGet(next_defaults.zhSettingKey);
        const stored_en = await settingsGet(next_defaults.enSettingKey);
        if (!mounted.current) return;
        set_defaults(next_defaults);
        set_routes(next_routes);
        set_providers(next_providers);
        set_gateway_running(status.running);
        set_zh_template(stored_zh ?? '');
        set_en_template(stored_en ?? '');
        set_saved_templates({ zh: stored_zh ?? '', en: stored_en ?? '' });
      } catch (cause) {
        if (mounted.current) set_error(prompt_error(cause));
      }
    })();
    return () => {
      mounted.current = false;
      sequence.current += 1;
    };
  }, []);

  // Keep the selected backend valid as routes and providers load or change.
  useEffect(() => {
    if (options.length === 0) return;
    if (options.some((option) => option.key === target_key)) return;
    set_target_key(options[0].key);
    persist_target_key(options[0].key);
  }, [options, target_key]);

  function ensure_idempotency_key(): string {
    if (!idempotency_key.current) {
      idempotency_key.current = globalThis.crypto?.randomUUID?.() ?? String(Date.now()) + '-' + Math.random().toString(36).slice(2);
    }
    return idempotency_key.current;
  }

  function invalidate_retry() {
    idempotency_key.current = null;
    retry_stage.current = null;
    retry_english.current = null;
  }

  function remember(entry: History_entry) {
    set_history((current) => {
      const next = [
        entry,
        ...current.filter((item) => !(item.input === entry.input && item.prompt === entry.prompt)),
      ].slice(0, HISTORY_LIMIT);
      persist_history(next);
      return next;
    });
  }

  async function run_zh(task_context?: Workspace_context_selection): Promise<string | null> {
    retry_stage.current = 'zh_expand';
    retry_english.current = null;
    if (!selected) {
      set_error({ code: 'prompt_enhance_target_model_required', detail: null });
      return null;
    }
    const text = input.trim();
    if (!text) {
      set_error({ code: 'prompt_enhance_empty_input', detail: null });
      return null;
    }
    const ticket = ++sequence.current;
    set_zh_status('running');
    set_error(null);
    set_notice(null);
    try {
      const result = await runPromptStage({
        stage: 'zh_expand',
        text,
        target: selected.target,
        instruction: instruction.trim() || undefined,
        workspaceContext: task_context,
        idempotencyKey: ensure_idempotency_key(),
      });
      if (!mounted.current || ticket !== sequence.current) return null;
      confirm_context(result.workspaceContext, task_context);
      set_draft(result.output);
      draft_context.current = { selection: task_context, label: task_context_label.current ?? 'Workspace context off' };
      set_served({
        provider: result.provider ?? null,
        model: result.model,
        elapsed_ms: result.elapsedMs,
        tokens: result.usage ? result.usage.promptTokens + '+' + result.usage.completionTokens : null,
      });
      if (result.complete === false) {
        set_error(prompt_error(result.failureCode ?? 'prompt_enhance_incomplete_response'));
        set_zh_status('error');
        return null;
      }
      set_zh_status('done');
      return result.output;
    } catch (cause) {
      if (mounted.current && ticket === sequence.current) {
        set_error(prompt_error(cause));
        set_zh_status('error');
      }
      return null;
    }
  }

  async function run_en(source: string, draft_for_history: string, task_context?: Workspace_context_selection): Promise<boolean> {
    retry_stage.current = 'en_optimize';
    retry_english.current = { source, draft: draft_for_history, context: task_context };
    if (!selected) {
      set_error({ code: 'prompt_enhance_target_model_required', detail: null });
      return false;
    }
    const text = source.trim();
    if (!text) {
      set_error({ code: 'prompt_enhance_empty_input', detail: null });
      return false;
    }
    const ticket = ++sequence.current;
    set_en_status('running');
    set_error(null);
    set_notice(null);
    try {
      const result = await runPromptStage({ stage: 'en_optimize', text, target: selected.target, workspaceContext: task_context, idempotencyKey: ensure_idempotency_key() });
      if (!mounted.current || ticket !== sequence.current) return false;
      confirm_context(result.workspaceContext, task_context);
      set_prompt(result.output);
      set_served({
        provider: result.provider ?? null,
        model: result.model,
        elapsed_ms: result.elapsedMs,
        tokens: result.usage ? result.usage.promptTokens + '+' + result.usage.completionTokens : null,
      });
      if (result.complete === false) {
        set_error(prompt_error(result.failureCode ?? 'prompt_enhance_incomplete_response'));
        set_en_status('error');
        return false;
      }
      if (result.reviewStatus === 'unavailable' || result.reviewStatus === 'correction_failed_draft_preserved') {
        set_notice('The generated prompt is preserved. Automated review could not finish; check the result before use.');
      } else if ((result.attempts ?? 1) > 1) {
        set_notice('The prompt was completed with automatic continuation or review.');
      }
      set_en_status('done');
      workspaces.reset_consent();
      remember({
        id: String(Date.now()),
        created_at: Date.now(),
        input: input.trim(),
        draft: draft_for_history,
        prompt: result.output,
        target_label: result.targetLabel,
      });
      idempotency_key.current = null;
      retry_stage.current = null;
      retry_english.current = null;
      return true;
    } catch (cause) {
      if (mounted.current && ticket === sequence.current) {
        set_error(prompt_error(cause));
        set_en_status('error');
      }
      return false;
    }
  }

  function confirm_context(receipt: Workspace_context_preview | undefined, selection?: Workspace_context_selection) {
    if (selection && (!receipt || receipt.workspaceId !== selection.workspaceId || receipt.workspaceRevision !== selection.workspaceRevision || receipt.contextVersion !== selection.expectedContextVersion)) {
      throw new Error('prompt_workspace_context_unavailable');
    }
    const included = receipt?.files.filter((file) => file.state === 'ready').map((file) => file.name) ?? [];
    const label = receipt ? `${receipt.workspaceName} · ${included.length ? included.join(', ') : 'no instruction files found'}` : 'Off · no instruction files included';
    task_context_label.current = label;
    set_context_receipt(label);
  }

  function capture_task_context(): Workspace_context_selection | undefined {
    const context = workspaces.capture_context();
    const label = context ? `${workspaces.selected?.name ?? 'Selected workspace'} · ${[context.readAgents ? 'AGENTS.md' : '', context.readClaude ? 'CLAUDE.md' : ''].filter(Boolean).join(', ')}` : 'Workspace context off';
    task_context_label.current = label;
    set_context_receipt(label);
    return context;
  }

  async function with_task(operation: () => Promise<void>) {
    if (operation_active.current || saving_templates) return;
    retry_stage.current = null;
    retry_english.current = null;
    operation_active.current = true;
    set_task_busy(true);
    try { await operation(); }
    finally {
      operation_active.current = false;
      if (mounted.current) set_task_busy(false);
    }
  }

  async function run_all() {
    return with_task(async () => {
    try {
      const task_context = capture_task_context();
      const expanded = await run_zh(task_context);
      if (expanded === null) return;
      await run_en(expanded, expanded, task_context);
    } catch (cause) { set_error(prompt_error(cause)); }
    });
  }

  async function expand_with_context() {
    return with_task(async () => {
    try { await run_zh(capture_task_context()); }
    catch (cause) { set_error(prompt_error(cause)); }
    });
  }

  async function optimize_with_context() {
    return with_task(async () => {
    try {
      const bound = draft_context.current;
      const context = bound ? bound.selection : capture_task_context();
      if (bound) set_context_receipt(bound.label);
      await run_en(draft, draft, context);
    }
    catch (cause) { set_error(prompt_error(cause)); }
    });
  }

  async function retry_failed_stage() {
    const english = retry_english.current;
    if (retry_stage.current === 'en_optimize' && english) {
      return with_task(async () => { await run_en(english.source, english.draft, english.context); });
    }
    return expand_with_context();
  }

  async function copy_value(value: string, label: string) {
    if (!value) return;
    try {
      await navigator.clipboard.writeText(value);
      if (mounted.current) set_notice(t('prompt.copied', { label }));
    } catch {
      if (mounted.current) set_notice(t('prompt.copy_failed'));
    }
  }

  async function save_templates() {
    if (!defaults || operation_active.current || saving_templates) return false;
    invalidate_retry();
    const limit = defaults.maxSystemPromptChars;
    if (zh_template.length > limit || en_template.length > limit) {
      set_notice(t('prompt.advanced.template_too_long', { limit: String(limit) }));
      return false;
    }
    set_saving_templates(true);
    try {
      await settingsSet(defaults.zhSettingKey, zh_template);
      await settingsSet(defaults.enSettingKey, en_template);
      if (mounted.current) { set_notice(t('prompt.advanced.template_saved')); set_saved_templates({ zh: zh_template, en: en_template }); }
      return true;
    } catch (cause) {
      if (mounted.current) set_error(prompt_error(cause));
      return false;
    } finally {
      if (mounted.current) set_saving_templates(false);
    }
  }

  async function reset_templates() {
    if (!defaults || operation_active.current || saving_templates) return;
    invalidate_retry();
    set_saving_templates(true);
    try {
      await settingsSet(defaults.zhSettingKey, '');
      await settingsSet(defaults.enSettingKey, '');
      if (mounted.current) {
        set_zh_template('');
        set_en_template('');
        set_saved_templates({ zh: '', en: '' });
        set_notice(t('prompt.advanced.template_saved'));
      }
    } catch (cause) {
      if (mounted.current) set_error(prompt_error(cause));
    } finally {
      if (mounted.current) set_saving_templates(false);
    }
  }

  async function start_gateway() {
    invalidate_retry();
    try {
      const message = await startProxy();
      if (!mounted.current) return;
      set_gateway_running(true);
      set_notice(message);
    } catch (cause) {
      if (mounted.current) set_error(prompt_error(cause));
    }
  }

  function load_entry(entry: History_entry) {
    if (operation_active.current) return;
    sequence.current++;
    invalidate_retry();
    set_input(entry.input);
    set_draft(entry.draft);
    set_prompt(entry.prompt);
    set_zh_status(entry.draft ? 'done' : 'idle');
    set_en_status(entry.prompt ? 'done' : 'idle');
    set_notice(null);
    set_error(null);
    set_served(null);
    workspaces.reset_consent();
    set_context_receipt(null);
    draft_context.current = null;
  }

  function clear_history() {
    if (operation_active.current) return;
    set_history([]);
    persist_history([]);
  }

  const gateway_group = options.filter((option) => option.group === 'gateway');
  const provider_group = options.filter((option) => option.group === 'provider');
  const stage_label = (status: Stage_status) => (status === 'running' ? 'Preparing and checking the prompt; continuing automatically if needed…' : '');

  return (
    <div className="prompt_page" data-testid="prompt_page">
      <header className="prompt_heading">
        <div>
          <p className="prompt_eyebrow">{t('prompt.eyebrow')}</p>
          <h2>{t('prompt.title')}</h2>
          <p className="prompt_muted">{t('prompt.description')}</p>
        </div>
      </header>

      <Prompt_workspace_panel controller={workspaces} busy={busy} />

      <section className="prompt_controls" aria-label={t('prompt.target.label')}>
        <label className="prompt_field prompt_field_grow">
          <span>{t('prompt.target.label')}</span>
          <select
            data-testid="prompt_target"
            value={selected?.key ?? ''}
            disabled={busy || options.length === 0}
            onChange={(event) => {
              if (operation_active.current) return;
              invalidate_retry();
              set_target_key(event.target.value);
              persist_target_key(event.target.value);
            }}
          >
            {options.length === 0 && <option value="">{t('prompt.target.empty')}</option>}
            {gateway_group.length > 0 && (
              <optgroup label={t('prompt.target.gateway_group')}>
                {gateway_group.map((option) => (
                  <option key={option.key} value={option.key}>{option.label}</option>
                ))}
              </optgroup>
            )}
            {provider_group.length > 0 && (
              <optgroup label={t('prompt.target.provider_group')}>
                {provider_group.map((option) => (
                  <option key={option.key} value={option.key}>{option.label}</option>
                ))}
              </optgroup>
            )}
          </select>
          <small className="prompt_hint">
            {selected?.group === 'provider' ? t('prompt.target.provider_hint') : t('prompt.target.gateway_hint')}
          </small>
        </label>
        {selected?.group === 'gateway' && !gateway_running && (
          <div className="prompt_warning" role="status" data-testid="prompt_gateway_stopped">
            <span>{t('prompt.target.gateway_stopped')}</span>
            <button className="btn small" onClick={() => { void start_gateway(); }}>
              {t('prompt.target.start_gateway')}
            </button>
          </div>
        )}
      </section>

      {error && (
        <div role="alert" className="prompt_error" data-testid="prompt_error">
          <span>{error.message ?? (error.code.startsWith('prompt_workspace_')
            ? workspace_error_message(error.code)
            : (prompt_recovery_message(error.code) ?? t('prompt.error.' + error.code, { detail: error.detail ?? t('prompt.error.detail_unknown') })))}</span>
          {error.field && <span className="prompt_error_field"> {error.field}</span>}
          {error.request_id && (
            <span className="prompt_error_request">
              {' '}({t('prompt.error.request_id')}: <code>{error.request_id}</code>
              <button className="btn small" type="button" onClick={() => { void copy_value(error.request_id ?? '', t('prompt.error.request_id')); }}>{t('prompt.error.copy_request_id')}</button>)
            </span>
          )}
          {error.retryable && retry_stage.current && (
            <button className="btn small" type="button" data-testid="prompt_retry" onClick={() => { void retry_failed_stage(); }} disabled={busy || input_over_limit}>
              {t('prompt.error.retry')}
            </button>
          )}
        </div>
      )}
      {notice && (
        <div role="status" className="prompt_notice" data-testid="prompt_notice">{notice}</div>
      )}

      <section className="prompt_composer">
        <label className="prompt_field">
          <span>{t('prompt.input.label')}</span>
          <textarea
            data-testid="prompt_input"
            rows={2}
            value={input}
            placeholder={t('prompt.input.placeholder')}
            disabled={busy}
            onChange={(event) => { if (operation_active.current) return; invalidate_retry(); set_input(event.target.value); if (draft_context.current || en_status === 'done') { workspaces.reset_consent(); set_context_receipt(null); draft_context.current = null; } }}
          />
        </label>
        <div className="prompt_composer_footer">
          <span className={input_over_limit ? 'prompt_counter prompt_counter_over' : 'prompt_counter'}>{input.length} / {input_limit}</span>
          {input_over_limit && <span role="alert" className="prompt_hint prompt_hint_error">{t('prompt.input.over_limit', { limit: String(input_limit) })}</span>}
          <div className="prompt_actions">
            <button
              className="btn"
              data-testid="prompt_expand"
              disabled={busy || input_over_limit || !input.trim() || !workspaces.context_ready}
              onClick={() => { void expand_with_context(); }}
            >
              {t('prompt.rerun_zh')}
            </button>
            <button
              className="btn primary"
              data-testid="prompt_run"
              disabled={busy || input_over_limit || !input.trim() || !workspaces.context_ready}
              onClick={() => { void run_all(); }}
            >
              {zh_status === 'running' ? t('prompt.stage.running') : t('prompt.run_all')}
            </button>
          </div>
        </div>
      </section>

      <div className="prompt_stages">
        <section className="prompt_stage" data-testid="prompt_stage_zh">
          <header className="prompt_stage_heading">
            <div>
              <h3>{t('prompt.stage.zh.title')}</h3>
              <p className="prompt_muted">{t('prompt.stage.zh.help')}</p>
            </div>
            <div className="prompt_actions">
              <button
                className="btn small"
                data-testid="prompt_copy_zh"
                disabled={!draft}
                onClick={() => { void copy_value(draft, t('prompt.label.zh')); }}
              >
                {t('prompt.copy')}
              </button>
              <button
                className="btn small"
                data-testid="prompt_optimize"
                disabled={busy || !draft.trim() || (!draft_context.current && !workspaces.context_ready)}
                onClick={() => { void optimize_with_context(); }}
              >
                {en_status === 'running' ? t('prompt.stage.running') : t('prompt.rerun_en')}
              </button>
            </div>
          </header>
          <textarea
            className="prompt_output"
            data-testid="prompt_draft"
            rows={6}
            disabled={busy}
            value={draft}
            placeholder={t('prompt.stage.empty')}
            onChange={(event) => { if (!operation_active.current) { invalidate_retry(); set_draft(event.target.value); } }}
          />
          <p className="prompt_hint">{stage_label(zh_status)}</p>
        </section>

        <section className="prompt_stage" data-testid="prompt_stage_en">
          <header className="prompt_stage_heading">
            <div>
              <h3>{t('prompt.stage.en.title')}</h3>
              <p className="prompt_muted">{t('prompt.stage.en.help')}</p>
            </div>
            <button
              className="btn small"
              data-testid="prompt_copy_en"
              disabled={!prompt}
              onClick={() => { void copy_value(prompt, t('prompt.label.en')); }}
            >
              {t('prompt.copy')}
            </button>
          </header>
          <textarea
            className="prompt_output prompt_output_en"
            data-testid="prompt_output"
            rows={6}
            disabled={busy}
            value={prompt}
            placeholder={t('prompt.stage.empty')}
            onChange={(event) => { if (!operation_active.current) { invalidate_retry(); set_prompt(event.target.value); } }}
          />
          <p className="prompt_hint">{stage_label(en_status)}</p>
        </section>
      </div>

      {context_receipt && <p className="prompt_hint" role="status" data-testid="prompt_workspace_receipt">Draft context: {context_receipt}. Workspace choices apply to the next expansion.</p>}

      {served && (
        <p className="prompt_meta" data-testid="prompt_served">
          {t('prompt.meta.served_by', {
            provider: served.provider ?? t('prompt.meta.gateway_route'),
            model: served.model,
            ms: String(served.elapsed_ms),
          })}
          {served.tokens ? ' - ' + t('prompt.meta.tokens', { tokens: served.tokens }) : ''}
        </p>
      )}

      <section className="prompt_history" aria-label={t('prompt.history.title')}>
        <header className="prompt_stage_heading">
          <h3>{t('prompt.history.title')}</h3>
          <button className="btn small" data-testid="prompt_history_clear" disabled={busy || history.length === 0} onClick={clear_history}>
            {t('prompt.history.clear')}
          </button>
        </header>
        {history.length === 0
          ? <p className="prompt_muted" data-testid="prompt_history_empty">{t('prompt.history.empty')}</p>
          : (
            <ul className="prompt_history_list">
              {history.map((entry) => (
                <li key={entry.id}>
                  <button className="prompt_history_item" data-testid="prompt_history_item" disabled={busy} onClick={() => load_entry(entry)}>
                    <strong>{entry.input || t('prompt.history.untitled')}</strong>
                    <span>{entry.prompt.slice(0, 120)}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
      </section>

      <details className="prompt_advanced" onToggle={(event) => set_advanced_open(event.currentTarget.open)}>
        <summary>{t('prompt.advanced.summary')}</summary>
        <label className="prompt_field">
          <span>{t('prompt.instruction.label')}</span>
          <input
            type="text"
            data-testid="prompt_instruction"
            disabled={busy}
            value={instruction}
            maxLength={defaults?.maxInstructionChars ?? 1000}
            placeholder={t('prompt.instruction.placeholder')}
            onChange={(event) => { if (!operation_active.current) { invalidate_retry(); set_instruction(event.target.value); } }}
          />
        </label>
        <p className="prompt_muted">{t('prompt.advanced.template_help')}</p>
        <label className="prompt_field">
          <span>{t('prompt.advanced.zh_template')}</span>
          <textarea
            rows={3}
            data-testid="prompt_template_zh"
            disabled={busy || saving_templates}
            value={zh_template}
            placeholder={defaults?.zhSystem ?? ''}
            onChange={(event) => set_zh_template(event.target.value)}
          />
        </label>
        <label className="prompt_field">
          <span>{t('prompt.advanced.en_template')}</span>
          <textarea
            rows={3}
            data-testid="prompt_template_en"
            disabled={busy || saving_templates}
            value={en_template}
            placeholder={defaults?.enSystem ?? ''}
            onChange={(event) => set_en_template(event.target.value)}
          />
        </label>
        <div className="prompt_actions">
          <button className="btn small" disabled={busy || saving_templates} onClick={() => { void reset_templates(); }}>
            {t('prompt.advanced.template_reset')}
          </button>
          <button className="btn small primary" data-testid="prompt_template_save" disabled={busy || saving_templates} onClick={() => { void save_templates(); }}>
            {t('common.save')}
          </button>
        </div>
      </details>

      <p className="prompt_privacy">{t('prompt.privacy')}</p>
    </div>
  );
}
