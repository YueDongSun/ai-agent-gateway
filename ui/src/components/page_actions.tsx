import { createContext, useCallback, useContext, useId, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import './page_actions.css';

export interface Page_actions {
  target: string;
  state: 'idle' | 'clean' | 'dirty' | 'busy' | 'error';
  on_save?: () => unknown | Promise<unknown>;
  on_delete?: () => unknown | Promise<unknown>;
  save_disabled?: boolean;
  delete_disabled?: boolean;
}
interface Entry { scope: string; priority: number; actions: Page_actions }
const Actions_context = createContext<{ scope: string; register: (key: string, entry: Entry) => () => void } | null>(null);
const Display_context = createContext<Page_actions | null>(null);

export function Page_actions_provider({ scope, children }: { scope: string; children: ReactNode }) {
  const [entries, set_entries] = useState<Map<string, Entry>>(() => new Map());
  const register = useCallback((key: string, entry: Entry) => {
    set_entries((current) => new Map(current).set(key, entry));
    return () => set_entries((current) => {
      if (current.get(key) !== entry) return current;
      const next = new Map(current); next.delete(key); return next;
    });
  }, []);
  const value = useMemo(() => ({ scope, register }), [scope, register]);
  const selected = [...entries.values()].filter((entry) => entry.scope === scope)
    .sort((left, right) => right.priority - left.priority)[0]?.actions ?? null;
  return <Actions_context.Provider value={value}><Display_context.Provider value={selected}>{children}</Display_context.Provider></Actions_context.Provider>;
}

/** Stable dispatch reads the latest callbacks; registration only tracks visible
 * action state, so form keystrokes cannot create a context render loop. */
export function use_page_actions(actions: Page_actions | null, priority = 10) {
  const context = useContext(Actions_context);
  const key = useId();
  const latest = useRef(actions);
  latest.current = actions;
  const target = actions?.target;
  const state = actions?.state;
  const can_save = Boolean(actions?.on_save);
  const can_delete = Boolean(actions?.on_delete);
  const save_disabled = actions?.save_disabled;
  const delete_disabled = actions?.delete_disabled;
  useLayoutEffect(() => {
    if (!context || target === undefined || state === undefined) return;
    return context.register(key, { scope: context.scope, priority, actions: {
      target, state, save_disabled, delete_disabled,
      on_save: can_save ? () => latest.current?.on_save?.() : undefined,
      on_delete: can_delete ? () => latest.current?.on_delete?.() : undefined,
    } });
  }, [context, key, priority, target, state, can_save, can_delete, save_disabled, delete_disabled]);
}

export function Floating_page_actions() {
  const actions = useContext(Display_context);
  const context = useContext(Actions_context);
  const operation = useRef(0);
  const operation_busy = useRef(false);
  const [pending, set_pending] = useState<'save' | 'delete' | null>(null);
  const [failed, set_failed] = useState(false);
  const [modal_host, set_modal_host] = useState<Element | null>(null);
  useLayoutEffect(() => {
    const update_host = () => {
      const dialogs = [...document.querySelectorAll('[role="dialog"], [role="alertdialog"]')].filter((element) => element.getClientRects().length > 0);
      const next = dialogs.at(-1) ?? null;
      set_modal_host((current) => current === next ? current : next);
    };
    update_host();
    const observer = new MutationObserver(update_host);
    observer.observe(document.body, { childList: true, subtree: true });
    return () => observer.disconnect();
  }, []);
  const action_target = actions?.target ?? '';
  useLayoutEffect(() => { operation.current++; operation_busy.current = false; set_pending(null); set_failed(false); }, [action_target, context?.scope]);
  useLayoutEffect(() => {
    document.documentElement.dataset.floatingActions = 'visible';
    return () => { delete document.documentElement.dataset.floatingActions; };
  }, []);
  const invoke = async (kind: 'save' | 'delete') => {
    const callback = kind === 'save' ? actions?.on_save : actions?.on_delete;
    if (!callback || operation_busy.current) return;
    const token = ++operation.current; operation_busy.current = true;
    set_pending(kind); set_failed(false);
    try { await callback(); } catch { if (operation.current === token) set_failed(true); }
    finally { if (operation.current === token) { operation_busy.current = false; set_pending(null); } }
  };
  const busy = Boolean(pending) || actions?.state === 'busy';
  const status = failed || actions?.state === 'error' ? 'Action needs attention' : pending === 'save' ? 'Saving…' : pending === 'delete' ? 'Deleting…' : actions?.state === 'dirty' ? 'Unsaved changes' : actions?.state === 'clean' ? 'Saved' : actions?.state === 'busy' ? 'Working…' : 'No editable selection';
  const dock = <aside className="page_action_dock" aria-label="Current page actions" data-testid="page-action-dock">
    <div className="page_action_state"><strong>{actions?.target ?? 'Page actions'}</strong><span role="status" aria-live="polite" data-testid="page-action-state">{status}</span></div>
    <button type="button" className="page_action_square" data-testid="page-action-save" disabled={busy || !actions?.on_save || actions.save_disabled}
      title={actions?.on_save ? `Save ${actions.target}` : 'Select editable settings to save'} aria-label={actions?.on_save ? `Save ${actions.target}` : 'Save unavailable'} onClick={() => void invoke('save')}>
      <svg viewBox="0 0 24 24" fill="none" aria-hidden><path d="M5 3h12l4 4v14H3V3h2Zm2 0v6h10V3M7 21v-8h10v8" stroke="currentColor" strokeWidth="1.7" strokeLinejoin="round" /></svg>
    </button>
    <button type="button" className="page_action_square danger" data-testid="page-action-delete" disabled={busy || !actions?.on_delete || actions.delete_disabled}
      title={actions?.on_delete ? `Delete ${actions.target}` : 'Select an item to delete'} aria-label={actions?.on_delete ? `Delete ${actions.target}` : 'Delete unavailable'} onClick={() => void invoke('delete')}>
      <svg viewBox="0 0 24 24" fill="none" aria-hidden><path d="M4 6h16M9 6V3h6v3M6 6l1 15h10l1-15M10 10v7m4-7v7" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" /></svg>
    </button>
  </aside>;
  return modal_host ? createPortal(dock, modal_host) : dock;
}
