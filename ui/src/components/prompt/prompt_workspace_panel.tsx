import type { Prompt_workspaces_controller } from './use_prompt_workspaces.js';
import './prompt_workspace_panel.css';

const file_states: Record<string, string> = {
  ready: 'Ready', missing: 'Missing — no text will be included', file_too_large: 'Too large (32 KiB limit)',
  file_unsafe: 'Blocked linked or invalid file', file_unavailable: 'Unavailable', file_invalid_utf8: 'Invalid UTF-8',
  file_ambiguous: 'More than one case variant exists', directory_limit: 'Folder has too many root entries',
  context_too_large: 'Combined context exceeds 48 KiB',
};

export function Prompt_workspace_panel({ controller: state, busy }: { controller: Prompt_workspaces_controller; busy: boolean }) {
  const disabled = busy || Boolean(state.operation) || state.loading;
  return (
    <section className="prompt_workspace_panel" data-testid="prompt_workspace_panel" aria-labelledby="prompt_workspace_heading">
      <header className="prompt_stage_heading">
        <div><h3 id="prompt_workspace_heading">Workspace context</h3><p className="prompt_muted">Choose conventions for the next expansion or a new English-only task.</p></div>
        <span className="prompt_workspace_state" role="status" data-testid="prompt_workspace_state">{state.loading ? 'Loading' : state.operation ? 'Saving…' : state.dirty ? 'Unsaved workspace' : state.selected ? 'Workspace selected' : 'No workspace selected'}</span>
      </header>
      {state.error && <p className="prompt_error" role="alert" data-testid="prompt_workspace_error">{state.error}</p>}
      {state.notice && <p className="prompt_notice" role="status">{state.notice}</p>}
      <div className="prompt_workspace_selection">
        <label className="prompt_field"><span>Selected workspace</span>
          <select data-testid="prompt_workspace_select" disabled={disabled} value={state.registry?.selectedId ?? ''} onChange={(event) => { void state.choose(event.target.value); }}>
            <option value="">No workspace context</option>
            {state.registry?.workspaces.map((workspace) => <option key={workspace.id} value={workspace.id}>{workspace.name}</option>)}
          </select>
        </label>
        <button type="button" className="btn small" disabled={disabled} onClick={state.add} data-testid="prompt_workspace_add">Add workspace</button>
        <button type="button" className="btn small" disabled={busy || Boolean(state.operation)} onClick={() => { void state.refresh(); }} data-testid="prompt_workspace_refresh">Refresh</button>
      </div>
      {state.selected && <p className="prompt_hint prompt_workspace_path" data-testid="prompt_workspace_path">{state.selected.root}</p>}
      <details className="prompt_workspace_manage" open={state.managing} onToggle={(event) => state.set_managing(event.currentTarget.open)}>
        <summary>Manage workspaces</summary>
        <div className="prompt_workspace_fields">
          <label className="prompt_field"><span>Name</span><input data-testid="prompt_workspace_name" value={state.name} maxLength={80} disabled={disabled} onChange={(event) => state.set_name(event.target.value)} /></label>
          <label className="prompt_field"><span>Existing project folder</span><input data-testid="prompt_workspace_root" value={state.root} maxLength={4096} disabled={disabled} placeholder="Absolute path to your project folder" onChange={(event) => state.set_root(event.target.value)} /></label>
        </div>
        <div className="prompt_actions">
          <button type="button" className="btn small primary" data-testid="prompt_workspace_save" disabled={disabled || !state.dirty || !state.name.trim() || !state.root.trim()} onClick={() => { void state.save(); }}>{state.operation === 'saved' ? 'Saving…' : 'Save workspace'}</button>
          <button type="button" className="btn small danger" data-testid="prompt_workspace_remove" disabled={disabled || !state.selected || state.draft_id !== state.selected.id} onClick={() => { void state.remove(); }}>Remove selected workspace</button>
          <span className="prompt_hint">Removing a workspace only removes this saved entry; project files stay on disk.</span>
        </div>
      </details>
      <label className="prompt_workspace_choice"><input data-testid="prompt_workspace_use_context" type="checkbox" checked={state.use_context} disabled={disabled || !state.selected} onChange={(event) => state.set_use_context(event.target.checked)} /><span>Read workspace instructions for this task</span></label>
      <div className="prompt_workspace_files">
        <label className="prompt_workspace_choice"><input data-testid="prompt_workspace_read_agents" type="checkbox" checked={state.read_agents} disabled={disabled || !state.use_context} onChange={(event) => state.set_read_agents(event.target.checked)} /><span>AGENTS.md</span></label>
        <label className="prompt_workspace_choice"><input data-testid="prompt_workspace_read_claude" type="checkbox" checked={state.read_claude} disabled={disabled || !state.use_context} onChange={(event) => state.set_read_claude(event.target.checked)} /><span>CLAUDE.md</span></label>
      </div>
      <p className="prompt_hint" role="status" data-testid="prompt_workspace_context_state">
        {!state.use_context ? 'Off for new tasks — no project files are read or sent.' : state.preview_loading ? 'Checking selected instruction files…' : state.context_error ? state.context_error : !state.read_agents && !state.read_claude ? 'Select at least one instruction file.' : state.preview?.canUse ? state.preview.files.some((file) => file.state === 'ready') ? 'Selected file text will be sent to the model backend chosen above for both stages of this task.' : 'No selected instruction files exist; this task will run without workspace text.' : 'Review file states before running.'}
      </p>
      {state.use_context && <>
        <ul className="prompt_workspace_file_states" data-testid="prompt_workspace_file_states">
          {state.preview?.files.map((file) => <li key={file.name}><strong>{file.name}</strong><span data-state={file.state}>{file_states[file.state] ?? 'Unavailable'}</span>{file.state === 'ready' && <span>{file.bytes.toLocaleString()} bytes</span>}</li>)}
        </ul>
        <button type="button" className="btn small" disabled={disabled || state.preview_loading} onClick={state.refresh_context} data-testid="prompt_workspace_context_refresh">Refresh context</button>
      </>}
    </section>
  );
}
