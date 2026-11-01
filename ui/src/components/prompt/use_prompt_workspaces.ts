import { useEffect, useRef, useState } from 'react';
import {
  list_prompt_workspaces, preview_workspace_context, remove_prompt_workspace,
  save_prompt_workspace, select_prompt_workspace, workspace_error_message,
  type Workspace_context_preview, type Workspace_context_selection, type Workspace_registry,
} from '../../lib/api/prompt_workspace.js';

export function use_prompt_workspaces() {
  const [registry, set_registry] = useState<Workspace_registry | null>(null);
  const [loading, set_loading] = useState(true);
  const [operation, set_operation] = useState<string | null>(null);
  const [error, set_error] = useState<string | null>(null);
  const [notice, set_notice] = useState<string | null>(null);
  const [draft_id, set_draft_id] = useState<string | null>(null);
  const [name, set_name] = useState('');
  const [root, set_root] = useState('');
  const [managing, set_managing] = useState(false);
  const [use_context, set_use_context] = useState(false);
  const [read_agents, set_read_agents] = useState(true);
  const [read_claude, set_read_claude] = useState(true);
  const [preview, set_preview] = useState<Workspace_context_preview | null>(null);
  const [preview_loading, set_preview_loading] = useState(false);
  const [context_error, set_context_error] = useState<string | null>(null);
  const [preview_version, set_preview_version] = useState(0);
  const mounted = useRef(false);
  const registry_ref = useRef(registry);
  const operation_ref = useRef(false);
  registry_ref.current = registry;

  function apply_registry(next: Workspace_registry, fill_form: boolean) {
    if (!mounted.current) return;
    if (registry_ref.current && next.revision < registry_ref.current.revision) return;
    const previous = registry_ref.current;
    if (previous && (previous.selectedId !== next.selectedId
      || previous.workspaces.find((item) => item.id === previous.selectedId)?.revision !== next.workspaces.find((item) => item.id === next.selectedId)?.revision)) set_use_context(false);
    registry_ref.current = next;
    set_registry(next);
    if (fill_form) {
      const selected = next.workspaces.find((item) => item.id === next.selectedId);
      set_draft_id(selected?.id ?? null);
      set_name(selected?.name ?? '');
      set_root(selected?.root ?? '');
      if (next.workspaces.length === 0) set_managing(true);
    }
  }

  async function refresh() {
    set_loading(true);
    const original = registry_ref.current?.workspaces.find((item) => item.id === draft_id);
    const preserve_form = Boolean(registry_ref.current && (draft_id ? name !== original?.name || root !== original?.root : name.trim() || root.trim()));
    try { apply_registry(await list_prompt_workspaces(), !preserve_form); set_error(null); }
    catch (cause) { if (mounted.current) set_error(workspace_error_message(cause)); }
    finally { if (mounted.current) set_loading(false); }
  }

  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => { mounted.current = false; };
  }, []);

  const selected = registry?.workspaces.find((item) => item.id === registry.selectedId) ?? null;
  const edited = registry?.workspaces.find((item) => item.id === draft_id);
  const dirty = draft_id ? name !== edited?.name || root !== edited?.root : Boolean(name.trim() || root.trim());

  function reset_consent() { set_use_context(false); }
  async function mutate(label: string, action: () => Promise<Workspace_registry>) {
    if (operation_ref.current) return false;
    operation_ref.current = true;
    set_operation(label); set_error(null); set_notice(null);
    try {
      const next = await action();
      apply_registry(next, true);
      if (mounted.current) { reset_consent(); set_notice(`Workspace ${label}.`); }
      return true;
    } catch (cause) { if (mounted.current) set_error(workspace_error_message(cause)); return false; }
    finally { operation_ref.current = false; if (mounted.current) set_operation(null); }
  }

  const save = () => mutate('saved', () => save_prompt_workspace({ expectedRevision: registry_ref.current?.revision ?? 0, id: draft_id, name, root }));
  const choose = (id: string) => mutate('selected', () => select_prompt_workspace(id || null, registry_ref.current?.revision ?? 0));
  const remove = () => selected ? mutate('removed', () => remove_prompt_workspace(selected.id, registry_ref.current?.revision ?? 0)) : Promise.resolve(false);
  const add = () => { set_draft_id(null); set_name(''); set_root(''); set_notice(null); set_error(null); set_managing(true); };

  useEffect(() => {
    let active = true;
    set_preview(null); set_context_error(null);
    if (!use_context || !selected || (!read_agents && !read_claude)) { set_preview_loading(false); return; }
    set_preview_loading(true);
    preview_workspace_context({ workspaceId: selected.id, workspaceRevision: selected.revision, readAgents: read_agents, readClaude: read_claude }).then((value) => {
      if (active) set_preview(value);
    }).catch((cause) => { if (active) set_context_error(workspace_error_message(cause)); })
      .finally(() => { if (active) set_preview_loading(false); });
    return () => { active = false; };
  }, [use_context, selected?.id, selected?.revision, read_agents, read_claude, preview_version]);

  const context_ready = !use_context || Boolean(selected && preview?.canUse && !preview_loading && (read_agents || read_claude));
  function capture_context(): Workspace_context_selection | undefined {
    if (!use_context) return undefined;
    if (!context_ready || !selected || !preview) throw new Error('prompt_workspace_context_unavailable');
    return { workspaceId: selected.id, workspaceRevision: selected.revision, readAgents: read_agents, readClaude: read_claude, expectedContextVersion: preview.contextVersion };
  }

  return { registry, selected, loading, operation, error, notice, draft_id, name, root, dirty, set_name, set_root, managing, set_managing,
    use_context, set_use_context, read_agents, set_read_agents, read_claude, set_read_claude,
    preview, preview_loading, context_error, context_ready, capture_context, reset_consent,
    refresh, refresh_context: () => set_preview_version((version) => version + 1), save, choose, remove, add };
}

export type Prompt_workspaces_controller = ReturnType<typeof use_prompt_workspaces>;
