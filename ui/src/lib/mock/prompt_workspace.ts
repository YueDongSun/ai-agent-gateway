/** Synthetic browser-only workspace metadata. This module never reads disk. */
import type { Prompt_workspace, Workspace_context_preview, Workspace_context_selection, Workspace_registry, Workspace_save_input } from '../api/prompt_workspace.js';

const storage_key = 'aag.prompt.workspaces.demo';
const workspace_limit = 32;
let sequence = 0;

function registry(): Workspace_registry {
  const raw = window.localStorage.getItem(storage_key);
  if (!raw) return { revision: 0, selectedId: null, workspaces: [] };
  let value: Workspace_registry;
  try { value = JSON.parse(raw) as Workspace_registry; } catch { throw new Error('prompt_workspace_registry_invalid'); }
  if (!Number.isSafeInteger(value.revision) || value.revision < 0 || !Array.isArray(value.workspaces) || value.workspaces.length > workspace_limit
    || value.workspaces.some((item) => !item || typeof item.id !== 'string' || typeof item.name !== 'string' || typeof item.root !== 'string' || !Number.isSafeInteger(item.revision))) {
    throw new Error('prompt_workspace_registry_invalid');
  }
  return value;
}

function persist(value: Workspace_registry): Workspace_registry {
  value.revision += 1;
  window.localStorage.setItem(storage_key, JSON.stringify(value));
  return structuredClone(value);
}

function check_revision(value: Workspace_registry, expected: unknown) {
  if (value.revision !== expected) throw new Error('prompt_workspace_registry_changed');
}

function validated_root(value: string): string {
  const root = value.trim();
  if (root.length > 4096 || /[\u0000-\u001f]/.test(root) || !(/^[a-z]:[\\/]/i.test(root) || root.startsWith('/')) || root.split(/[\\/]/).includes('..')) throw new Error('prompt_workspace_path_invalid');
  return root.replace(/[\\/]$/, '');
}

export function mock_prompt_workspace_command(command: string, payload: Record<string, unknown>): unknown {
  const value = registry();
  if (command === 'prompt_workspace_list') return structuredClone(value);
  if (command === 'prompt_workspace_save') {
    const input = payload.input as Workspace_save_input;
    check_revision(value, input.expectedRevision);
    const name = String(input.name ?? '').trim();
    if (!name || name.length > 80 || /[\u0000-\u001f]/.test(name)) throw new Error('prompt_workspace_name_invalid');
    const root = validated_root(String(input.root ?? ''));
    if (value.workspaces.some((item) => item.id !== input.id && item.root.toLowerCase() === root.toLowerCase())) throw new Error('prompt_workspace_duplicate_root');
    if (input.id) {
      const item = value.workspaces.find((item) => item.id === input.id);
      if (!item) throw new Error('prompt_workspace_not_found');
      item.name = name; item.root = root; item.revision += 1;
    } else {
      if (value.workspaces.length >= workspace_limit) throw new Error('prompt_workspace_limit');
      const item: Prompt_workspace = { id: `demo_workspace_${Date.now()}_${++sequence}`, name, root, revision: 1 };
      value.workspaces.push(item); value.selectedId = item.id;
    }
    return persist(value);
  }
  if (command === 'prompt_workspace_select') {
    check_revision(value, payload.expectedRevision);
    const id = payload.id === null ? null : String(payload.id);
    if (id && !value.workspaces.some((item) => item.id === id)) throw new Error('prompt_workspace_not_found');
    value.selectedId = id;
    return persist(value);
  }
  if (command === 'prompt_workspace_remove') {
    check_revision(value, payload.expectedRevision);
    const id = String(payload.id);
    if (!value.workspaces.some((item) => item.id === id)) throw new Error('prompt_workspace_not_found');
    value.workspaces = value.workspaces.filter((item) => item.id !== id);
    if (value.selectedId === id) value.selectedId = null;
    return persist(value);
  }
  if (command === 'prompt_workspace_preview') return preview(payload.selection as Workspace_context_selection);
  throw new Error('demo_command_unsupported');
}

function preview(selection: Workspace_context_selection): Workspace_context_preview {
  const value = registry();
  const workspace = value.workspaces.find((item) => item.id === selection.workspaceId);
  if (!workspace) throw new Error('prompt_workspace_not_found');
  if (workspace.revision !== selection.workspaceRevision) throw new Error('prompt_workspace_registry_changed');
  const files = [selection.readAgents ? { name: 'AGENTS.md', state: 'ready', bytes: 48 } : null, selection.readClaude ? { name: 'CLAUDE.md', state: 'ready', bytes: 52 } : null]
    .filter((item): item is { name: string; state: string; bytes: number } => item !== null);
  if (!files.length) throw new Error('prompt_workspace_files_not_selected');
  const context_version = `demo:${workspace.id}:${workspace.revision}:${Number(selection.readAgents)}:${Number(selection.readClaude)}`;
  if (selection.expectedContextVersion && selection.expectedContextVersion !== context_version) throw new Error('prompt_workspace_context_changed');
  return { workspaceId: workspace.id, workspaceName: workspace.name, workspaceRevision: workspace.revision, files, contextVersion: context_version, canUse: true };
}

export function mock_workspace_receipt(selection: unknown): Workspace_context_preview | undefined {
  if (!selection || typeof selection !== 'object') return undefined;
  const choice = selection as Workspace_context_selection;
  if (!choice.readAgents && !choice.readClaude) return undefined;
  if (!choice.expectedContextVersion) throw new Error('prompt_workspace_preview_required');
  return preview(choice);
}
