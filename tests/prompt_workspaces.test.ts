import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { mock_prompt_workspace_command, mock_workspace_receipt } from '../ui/src/lib/mock/prompt_workspace.js';
import { mock_prompt_command } from '../ui/src/lib/mock/prompt.js';
import type { Workspace_context_preview, Workspace_registry } from '../ui/src/lib/api/prompt_workspace.js';

beforeEach(() => {
  const storage = new Map<string, string>();
  vi.stubGlobal('window', { localStorage: { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) } });
});
afterEach(() => vi.unstubAllGlobals());

function save(name: string, root: string) {
  const registry = mock_prompt_workspace_command('prompt_workspace_list', {}) as Workspace_registry;
  return mock_prompt_workspace_command('prompt_workspace_save', { input: { expectedRevision: registry.revision, id: null, name, root } }) as Workspace_registry;
}

describe('prompt workspace browser contract', () => {
  it('registers, selects, renames and removes workspace metadata without changing another workspace', () => {
    const first = save('First', '/example_project/first');
    const second = save('Second', '/example_project/second');
    const a = second.workspaces[0]; const b = second.workspaces[1];
    const selected = mock_prompt_workspace_command('prompt_workspace_select', { id: a.id, expectedRevision: second.revision }) as Workspace_registry;
    expect(selected.selectedId).toBe(a.id);
    const renamed = mock_prompt_workspace_command('prompt_workspace_save', { input: { expectedRevision: selected.revision, id: a.id, name: 'Renamed', root: a.root } }) as Workspace_registry;
    expect(renamed.workspaces[1]).toEqual(b);
    const removed = mock_prompt_workspace_command('prompt_workspace_remove', { id: a.id, expectedRevision: renamed.revision }) as Workspace_registry;
    expect(removed.workspaces).toEqual([b]);
    expect(removed.selectedId).toBeNull();
    expect(first.workspaces).toHaveLength(1);
  });
  it('rejects duplicate roots, traversal and stale changes', () => {
    const first = save('First', '/example_project/first');
    expect(() => save('Duplicate', '/example_project/first')).toThrow('prompt_workspace_duplicate_root');
    expect(() => save('Invalid', '/example_project/../outside')).toThrow('prompt_workspace_path_invalid');
    expect(() => mock_prompt_workspace_command('prompt_workspace_select', { id: first.selectedId, expectedRevision: 0 })).toThrow('prompt_workspace_registry_changed');
    expect((mock_prompt_workspace_command('prompt_workspace_list', {}) as Workspace_registry).workspaces).toHaveLength(1);
  });
  it('does not request context unless the task explicitly opts in', () => {
    expect(mock_workspace_receipt(undefined)).toBeUndefined();
    expect(mock_workspace_receipt({ workspaceId: 'missing', workspaceRevision: 1, readAgents: false, readClaude: false })).toBeUndefined();
    const result = mock_prompt_command('prompt_enhance_run', { request: { stage: 'zh_expand', text: 'Task', target: { kind: 'gateway', model: 'route' } } }) as { workspaceContext?: unknown };
    expect(result.workspaceContext).toBeUndefined();
  });
  it('binds both stage requests to one selected snapshot without crossing workspaces', () => {
    const registered = save('First', '/example_project/first');
    const a = registered.workspaces[0];
    const selection = { workspaceId: a.id, workspaceRevision: a.revision, readAgents: true, readClaude: false };
    const preview = mock_prompt_workspace_command('prompt_workspace_preview', { selection }) as Workspace_context_preview;
    const context = { ...selection, expectedContextVersion: preview.contextVersion };
    save('Second', '/example_project/second');
    for (const stage of ['zh_expand', 'en_optimize']) {
      const output = mock_prompt_command('prompt_enhance_run', { request: { stage, text: 'Task', target: { kind: 'gateway', model: 'route' }, workspaceContext: context } }) as { workspaceContext: Workspace_context_preview };
      expect(output.workspaceContext.workspaceName).toBe('First');
      expect(output.workspaceContext.files.map((file) => file.name)).toEqual(['AGENTS.md']);
    }
    expect(() => mock_workspace_receipt({ ...context, readClaude: true })).toThrow('prompt_workspace_context_changed');
  });
  it('rejects removed workspace snapshots instead of falling back to the new selection', () => {
    const registered = save('First', '/example_project/first');
    const a = registered.workspaces[0];
    const selection = { workspaceId: a.id, workspaceRevision: a.revision, readAgents: true, readClaude: true };
    const info = mock_prompt_workspace_command('prompt_workspace_preview', { selection }) as Workspace_context_preview;
    mock_prompt_workspace_command('prompt_workspace_remove', { id: a.id, expectedRevision: registered.revision });
    expect(() => mock_workspace_receipt({ ...selection, expectedContextVersion: info.contextVersion })).toThrow('prompt_workspace_not_found');
  });
});
