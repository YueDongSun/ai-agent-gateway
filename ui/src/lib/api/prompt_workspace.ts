import { invoke } from '@tauri-apps/api/core';

export interface Prompt_workspace { id: string; name: string; root: string; revision: number }
export interface Workspace_registry { revision: number; selectedId: string | null; workspaces: Prompt_workspace[] }
export interface Workspace_save_input { expectedRevision: number; id: string | null; name: string; root: string }
export interface Workspace_context_selection {
  workspaceId: string;
  workspaceRevision: number;
  readAgents: boolean;
  readClaude: boolean;
  expectedContextVersion?: string | null;
}
export interface Workspace_file_state { name: string; state: string; bytes: number }
export interface Workspace_context_preview {
  workspaceId: string;
  workspaceName: string;
  workspaceRevision: number;
  files: Workspace_file_state[];
  contextVersion: string;
  canUse: boolean;
}

export const list_prompt_workspaces = () => invoke<Workspace_registry>('prompt_workspace_list');
export const save_prompt_workspace = (input: Workspace_save_input) => invoke<Workspace_registry>('prompt_workspace_save', { input });
export const select_prompt_workspace = (id: string | null, expectedRevision: number) => invoke<Workspace_registry>('prompt_workspace_select', { id, expectedRevision });
export const remove_prompt_workspace = (id: string, expectedRevision: number) => invoke<Workspace_registry>('prompt_workspace_remove', { id, expectedRevision });
export const preview_workspace_context = (selection: Workspace_context_selection) => invoke<Workspace_context_preview>('prompt_workspace_preview', { selection });

const workspace_messages: Record<string, string> = {
  name_invalid: 'Enter a workspace name of at most 80 characters.',
  path_invalid: 'Enter an absolute workspace folder path.',
  path_unsafe: 'The workspace must be a normal folder without linked path components.',
  path_unavailable: 'The workspace folder is unavailable.',
  duplicate_root: 'This folder is already registered as a workspace.',
  registry_changed: 'The workspace changed elsewhere. Refresh and try again.',
  registry_invalid: 'The saved workspace registry could not be read.',
  storage_unavailable: 'Workspace settings could not be saved or read.',
  not_found: 'This workspace has been removed. Select a current workspace.',
  limit: 'At most 32 workspaces can be registered.',
  context_changed: 'Instruction files changed after preview. Refresh context and expand again.',
  context_unavailable: 'Selected instruction files are unavailable. Review their states below.',
  preview_required: 'Preview the selected workspace files before running.',
  files_not_selected: 'Select at least one instruction file.',
  worker_failed: 'The workspace operation did not finish. Try again.',
};

export function workspace_error_message(error: unknown): string {
  const code = String(error).replace(/^Error:\s*/, '').trim().replace(/^prompt_workspace_/, '');
  return workspace_messages[code] ?? 'Workspace context could not be prepared. Refresh and try again.';
}
