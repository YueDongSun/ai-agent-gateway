import { useState, type FormEvent } from 'react';
import { mcp_error_message, mcp_save_server, type Mcp_save_input, type Mcp_server_summary } from '../../lib/api/mcp.js';
import { Mcp_dialog } from './mcp_dialog.js';

interface Mcp_editor_props {
  server: Mcp_server_summary | null;
  on_close: () => void;
  on_saved: (server: Mcp_server_summary) => void;
}

export function Mcp_editor({ server, on_close, on_saved }: Mcp_editor_props) {
  const [id, set_id] = useState(server?.id ?? '');
  const [name, set_name] = useState(server?.name ?? '');
  const [description, set_description] = useState(server?.description ?? '');
  const [configuration, set_configuration] = useState('');
  const [replace_config, set_replace_config] = useState(!server);
  const [enabled_claude, set_enabled_claude] = useState(server?.enabled_claude ?? false);
  const [enabled_codex, set_enabled_codex] = useState(server?.enabled_codex ?? false);
  const [busy, set_busy] = useState(false);
  const [error, set_error] = useState<string | null>(null);

  async function save(event: FormEvent) {
    event.preventDefault();
    if (busy) return;
    set_error(null);
    const input: Mcp_save_input = { create_only: !server, expected_revision: server?.revision, id: id.trim(), name: name.trim(), description: description.trim() || null, enabled_claude, enabled_codex };
    if (replace_config) {
      try { input.server_config = JSON.parse(configuration); }
      catch { set_error('Enter a valid JSON server definition. The registry was not changed.'); return; }
      if (!input.server_config || typeof input.server_config !== 'object' || Array.isArray(input.server_config)) {
        set_error('The server definition must be a JSON object.'); return;
      }
    }
    set_busy(true);
    try { on_saved(await mcp_save_server(input)); }
    catch (cause) { set_error(mcp_error_message(cause)); }
    finally { set_busy(false); }
  }

  return <Mcp_dialog title={server ? 'Edit MCP server' : 'Add an MCP server'} on_close={on_close} busy={busy} test_id="mcp_editor">
    <p className="mcp_muted">Save a definition to AAG. No server is installed, started, or connected here.</p>
    <form onSubmit={(event) => { void save(event); }}>
      <div className="mcp_form_grid">
        <label>Server ID<input data-testid="mcp_id" required pattern="[a-z][a-z0-9_]*" title="Start with a lowercase letter; use lowercase letters, numbers, and underscores." maxLength={64} disabled={busy || Boolean(server)} value={id} onChange={(event) => set_id(event.target.value)} placeholder="my_server" autoComplete="off" /></label>
        <label>Display name<input data-testid="mcp_name" required maxLength={128} disabled={busy} value={name} onChange={(event) => set_name(event.target.value)} placeholder="My MCP server" autoComplete="off" /></label>
      </div>
      <label className="mcp_field">Description<input data-testid="mcp_description" maxLength={1000} disabled={busy} value={description} onChange={(event) => set_description(event.target.value)} placeholder="What does this server help with?" autoComplete="off" /></label>
      {server && <label className="mcp_check"><input type="checkbox" data-testid="mcp_replace_config" checked={replace_config} disabled={busy} onChange={(event) => { set_replace_config(event.target.checked); set_configuration(''); }} />Replace the complete server configuration</label>}
      {replace_config ? <label className="mcp_field">Server definition (JSON)<textarea data-testid="mcp_config" rows={9} maxLength={32768} required spellCheck={false} autoComplete="off" disabled={busy} value={configuration} onChange={(event) => set_configuration(event.target.value)} placeholder={'{"command": "node", "args": ["/absolute/path/server.mjs"], "env": {}}'} /></label> : <p className="mcp_notice">Saved credentials and connection settings will be preserved. Redacted values are never written back.</p>}
      <p className="mcp_muted small">Use a stdio command with an argument array, or an HTTP URL. Values are stored in the local AAG database; prefer environment references where your client supports them.</p>
      <fieldset disabled={busy} className="mcp_targets"><legend>Include in client configuration</legend>
        <label className="mcp_check"><input data-testid="mcp_editor_claude" type="checkbox" checked={enabled_claude} onChange={(event) => set_enabled_claude(event.target.checked)} />Claude Code</label>
        <label className="mcp_check"><input data-testid="mcp_editor_codex" type="checkbox" checked={enabled_codex} onChange={(event) => set_enabled_codex(event.target.checked)} />Codex (preview only)</label>
      </fieldset>
      {error && <p role="alert" className="mcp_error" data-testid="mcp_editor_error">{error}</p>}
      <div className="mcp_actions"><button type="button" className="btn" disabled={busy} onClick={on_close} data-testid="mcp_cancel_editor">Cancel</button><button className="btn primary" disabled={busy} data-testid="mcp_save">{busy ? 'Saving…' : 'Save definition'}</button></div>
    </form>
  </Mcp_dialog>;
}
