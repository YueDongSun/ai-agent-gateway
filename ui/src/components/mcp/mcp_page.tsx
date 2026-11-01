import { useEffect, useRef, useState } from 'react';
import { mcp_delete_server, mcp_discard_review, mcp_error_message, mcp_list_servers, mcp_preview_sync, mcp_set_enabled, type Mcp_client, type Mcp_server_summary, type Mcp_sync_preview } from '../../lib/api/mcp.js';
import { Mcp_editor } from './mcp_editor.js';
import { Mcp_review } from './mcp_review.js';
import { Mcp_dialog } from './mcp_dialog.js';
import { Workflow_panel } from '../workflow/workflow_panel.js';
import './mcp_page.css';
import { use_page_actions } from '../page_actions.js';

export function Mcp_page() {
  const [servers, set_servers] = useState<Mcp_server_summary[]>([]);
  const [query, set_query] = useState('');
  const [busy, set_busy] = useState(true);
  const [error, set_error] = useState<string | null>(null);
  const [notice, set_notice] = useState<string | null>(null);
  const [editor, set_editor] = useState<Mcp_server_summary | null | undefined>(undefined);
  const [preview, set_preview] = useState<Mcp_sync_preview | null>(null);
  const [removing, set_removing] = useState<Mcp_server_summary | null>(null);
  /** Server ids whose redacted definition is currently expanded. The details
   *  sections are uncontrolled, so their toggle events drive this set and the
   *  pretty-printed JSON is only built (and only present as DOM text) for
   *  sections the user actually opened. */
  const [expanded_config_ids, set_expanded_config_ids] = useState<Set<string>>(() => new Set());
  const mounted = useRef(false);
  const request_sequence = useRef(0);
  async function refresh() {
    const sequence = ++request_sequence.current;
    set_busy(true); set_error(null);
    try { const next = await mcp_list_servers(); if (mounted.current && sequence === request_sequence.current) set_servers(next); }
    catch (cause) { if (mounted.current && sequence === request_sequence.current) set_error(mcp_error_message(cause)); }
    finally { if (mounted.current && sequence === request_sequence.current) set_busy(false); }
  }
  useEffect(() => { mounted.current = true; void refresh(); return () => { mounted.current = false; ++request_sequence.current; }; }, []);
  async function perform(action: () => Promise<void>) {
    if (busy) return false;
    set_busy(true); set_error(null); set_notice(null);
    try { await action(); return true; }
    catch (cause) { if (mounted.current) set_error(mcp_error_message(cause)); return false; }
    finally { if (mounted.current) set_busy(false); }
  }
  async function confirm_remove() {
    const chosen = removing;
    if (!chosen) return false;
    return perform(async () => {
      await mcp_delete_server(chosen.id);
      if (mounted.current) {
        set_servers((current) => current.filter((item) => item.id !== chosen.id));
        set_editor((current) => current?.id === chosen.id ? undefined : current);
        set_removing(null);
        set_notice('Definition removed from AAG. Client files were not changed.');
      }
    });
  }
  use_page_actions(removing ? {
    target: `Remove MCP ${removing.name}`, state: busy ? 'busy' : error ? 'error' : 'dirty',
    on_delete: async () => { if (!await confirm_remove()) throw new Error('MCP removal failed'); }, delete_disabled: busy,
  } : null, 30);
  async function toggle(server: Mcp_server_summary, client: Mcp_client, enabled: boolean) {
    await perform(async () => {
      const saved = await mcp_set_enabled(server.id, client, enabled);
      if (!mounted.current) return;
      set_servers((current) => current.map((item) => item.id === saved.id ? saved : item));
      set_notice('Client selection saved in AAG only. Review sync to update Claude Code; Codex remains preview-only.');
    });
  }
  function track_config_expansion(server_id: string, open: boolean) {
    set_expanded_config_ids((current) => {
      if (current.has(server_id) === open) return current;
      const next = new Set(current);
      if (open) next.add(server_id);
      else next.delete(server_id);
      return next;
    });
  }
  const visible_servers = servers.filter((server) => `${server.name} ${server.id} ${server.description ?? ''}`.toLowerCase().includes(query.trim().toLowerCase()));
  async function prepare_preview(client: Mcp_client) {
    await perform(async () => {
      const next = await mcp_preview_sync(client);
      if (mounted.current) set_preview(next);
      else if (next.review_token) await mcp_discard_review(next.review_token);
    });
  }
  return <div className="mcp_page" data-testid="mcp_page">
    <header className="mcp_page_heading"><div><p className="mcp_eyebrow">WORKSPACE CONNECTIONS</p><h2>A little more capable.</h2><p className="mcp_muted">Bring your tools together, with you in control.</p></div><button className="btn primary" disabled={busy} data-testid="mcp_add" onClick={() => set_editor(null)}>+ Add server</button></header>
    <section className="mcp_overview" aria-label="MCP overview">
      <div><strong>{servers.length}</strong><span>Registered servers</span></div><div><strong>{servers.filter((server) => server.enabled_claude).length}</strong><span>Selected for Claude Code</span></div><div><strong>{servers.filter((server) => server.enabled_codex).length}</strong><span>Selected for Codex preview</span></div>
    </section>
    <Workflow_panel />
    <div className="mcp_notice mcp_boundary"><strong>Configure once. Review before connecting.</strong><p>AAG stores definitions, not running servers. Selecting a client does not install packages, run commands, contact endpoints, or change a client file.</p></div>
    {error && <div role="alert" className="mcp_error" data-testid="mcp_error">{error}</div>}
    {notice && <div role="status" className="mcp_notice" data-testid="mcp_notice">{notice}</div>}
    <div className="mcp_toolbar"><label className="mcp_search">Search servers<input type="search" data-testid="mcp_search" value={query} onChange={(event) => set_query(event.target.value)} placeholder="Search by name or description" /></label><div className="mcp_actions">
      <button className="btn" disabled={busy} data-testid="mcp_refresh" onClick={() => { void refresh(); }}>Refresh</button>
      <button className="btn" disabled={busy} data-testid="mcp_preview_codex" onClick={() => { void prepare_preview('codex'); }}>Preview Codex</button>
      <button className="btn" disabled={busy} data-testid="mcp_preview_claude" onClick={() => { void prepare_preview('claude'); }}>Review Claude sync</button>
    </div></div>
    <section className="mcp_server_list" aria-label="Registered MCP servers" aria-busy={busy}>
      {busy && servers.length === 0 ? <p role="status" className="mcp_empty">Loading MCP definitions…</p> : visible_servers.length === 0 ? <div className="mcp_empty"><h3>{query ? 'No matching servers' : 'Your tools start here'}</h3><p>{query ? 'Try a different name or description.' : 'Add a stdio or HTTP MCP definition, select its clients, then review the configuration.'}</p></div> : visible_servers.map((server) => <article className="mcp_server" key={server.id} data-testid={`mcp_server_${server.id}`}>
        <div className="mcp_server_heading"><div className="mcp_server_identity"><span className="mcp_server_icon" aria-hidden="true">{server.transport === 'http' ? '↗' : '⌘'}</span><div><h3>{server.name}</h3><code>{server.id}</code></div></div><span className="mcp_tag">{server.transport === 'http' ? 'HTTP' : server.transport === 'stdio' ? 'STDIO' : 'INVALID'}</span></div>
        <p className="mcp_muted">{server.description || 'No description added.'}</p>
        {server.validation_error && <p className="mcp_error">This definition needs attention before it can be synced.</p>}
        <details className="mcp_details" onToggle={(event) => { track_config_expansion(server.id, event.currentTarget.open); }}><summary>Inspect redacted definition</summary>{expanded_config_ids.has(server.id) && <pre className="mcp_code">{JSON.stringify(server.redacted_config, null, 2)}</pre>}</details>
        <div className="mcp_server_footer"><div className="mcp_client_choices">
          <label className="mcp_check"><input type="checkbox" data-testid={`mcp_claude_${server.id}`} disabled={busy} checked={server.enabled_claude} onChange={(event) => { void toggle(server, 'claude', event.target.checked); }} />Claude Code</label>
          <label className="mcp_check"><input type="checkbox" data-testid={`mcp_codex_${server.id}`} disabled={busy} checked={server.enabled_codex} onChange={(event) => { void toggle(server, 'codex', event.target.checked); }} />Codex</label>
        </div><div className="mcp_actions"><button className="btn" disabled={busy} data-testid={`mcp_edit_${server.id}`} onClick={() => set_editor(server)}>Edit</button><button className="btn" disabled={busy} data-testid={`mcp_delete_${server.id}`} onClick={() => set_removing(server)}>Remove</button></div></div>
      </article>)}
    </section>
    <aside className="mcp_companion"><span className="mcp_eyebrow">MADE FOR AAG</span><h3>A read-only companion for your coding agent.</h3><p>Use the bundled <code>scripts/mcp/aag_mcp.mjs</code> stdio server to discover feature-aware verification plans and AAG safety workflows. It cannot execute commands, read client credentials, or approve configuration changes.</p><p className="mcp_muted">Register it with <code>node</code> and the absolute script path in <code>args</code>. See <code>docs/mcp_companion.md</code> for setup and capability boundaries.</p></aside>
    {editor !== undefined && <Mcp_editor server={editor} on_close={() => set_editor(undefined)} on_remove={editor ? () => set_removing(editor) : undefined} on_saved={(server) => { set_editor(undefined); set_servers((current) => [...current.filter((item) => item.id !== server.id), server].sort((a, b) => a.id.localeCompare(b.id))); set_notice('Definition saved. Client files are unchanged until you review and apply a sync.'); }} />}
    {preview && <Mcp_review key={preview.review_token ?? 'codex_preview'} preview={preview} on_close={() => set_preview(null)} on_applied={(receipt) => { set_preview(null); set_notice(`Claude Code MCP configuration updated for ${receipt.applied_server_count} server(s). ${receipt.backup_created ? 'Previous content was backed up.' : 'A new configuration file was created.'} Restart the client to load changes.`); }} />}
    {removing && <Mcp_dialog title="Remove this definition?" busy={busy} on_close={() => set_removing(null)} test_id="mcp_delete_dialog"><p>Remove <strong>{removing.name}</strong> from the AAG registry?</p><p className="mcp_notice">Existing client entries are not removed by this action. To remove a Claude entry, cancel, deselect Claude Code, then review and apply sync before deleting the definition.</p><div className="mcp_actions"><button className="btn" disabled={busy} data-testid="mcp_cancel_delete" onClick={() => set_removing(null)}>Cancel</button><button className="btn" disabled={busy} data-testid="mcp_confirm_delete" onClick={() => { void confirm_remove(); }}>Remove from AAG</button></div></Mcp_dialog>}
  </div>;
}
