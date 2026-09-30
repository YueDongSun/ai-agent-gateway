import { useEffect, useRef, useState } from 'react';
import { mcp_apply_sync, mcp_discard_review, mcp_error_message, type Mcp_sync_preview, type Mcp_sync_receipt } from '../../lib/api/mcp.js';
import { Mcp_dialog } from './mcp_dialog.js';

export function Mcp_review({ preview, on_close, on_applied }: { preview: Mcp_sync_preview; on_close: () => void; on_applied: (receipt: Mcp_sync_receipt) => void }) {
  const [acknowledged, set_acknowledged] = useState(false);
  const [busy, set_busy] = useState(false);
  const [error, set_error] = useState<string | null>(null);
  const [now, set_now] = useState(Date.now());
  const review_active = useRef(false);
  const deadline = preview.expires_at ? Date.parse(preview.expires_at) : null;
  const expired = deadline !== null && (!Number.isFinite(deadline) || deadline <= now);
  useEffect(() => { const timer = window.setInterval(() => set_now(Date.now()), 1000); return () => window.clearInterval(timer); }, []);
  useEffect(() => {
    // Defer disposal past StrictMode's setup/cleanup rehearsal. A real close or
    // page unmount releases the bounded native slot; used tokens are harmless.
    review_active.current = true;
    return () => {
      review_active.current = false;
      queueMicrotask(() => { if (!review_active.current && preview.review_token) void mcp_discard_review(preview.review_token).catch(() => { /* The native expiry remains the fallback if disposal cannot reach IPC. */ }); });
    };
  }, [preview.review_token]);
  async function apply() {
    if (!acknowledged || !preview.can_apply || !preview.review_token || expired || busy) return;
    set_busy(true);
    try { on_applied(await mcp_apply_sync(preview.review_token)); }
    catch (cause) { set_error(mcp_error_message(cause)); }
    finally { set_busy(false); }
  }
  return <Mcp_dialog title={preview.client === 'claude' ? 'Review Claude Code sync' : 'Review Codex configuration'} on_close={on_close} busy={busy} test_id="mcp_review">
    <p className="mcp_muted">{preview.client === 'claude' ? 'Only AAG-managed MCP entries are changed. Other client settings are preserved.' : 'Codex is preview-only. AAG will not write or merge your TOML configuration.'}</p>
    <div className="mcp_notice"><strong>Redacted review, not a runnable export</strong><p>Credentials and private connection details are hidden. Do not paste this preview into a client configuration.</p></div>
    <ul className="mcp_changes">{preview.changes.map((change) => <li key={change.server_id}><code>{change.server_id}</code><span className="mcp_tag">{change.action}</span></li>)}</ul>
    <pre className="mcp_code" data-testid="mcp_preview_config">{typeof preview.redacted_config === 'string' ? preview.redacted_config : JSON.stringify(preview.redacted_config, null, 2)}</pre>
    <ul className="mcp_muted">{preview.warnings.map((warning, index) => <li key={index}>{warning}</li>)}</ul>
    {preview.can_apply && <label className="mcp_check mcp_ack"><input type="checkbox" data-testid="mcp_acknowledge" checked={acknowledged} disabled={busy || Boolean(error)} onChange={(event) => set_acknowledged(event.target.checked)} />I reviewed these changes and approve writing the Claude Code MCP configuration with a backup.</label>}
    {expired && <p role="alert" className="mcp_error">This review expired. Close it and create a fresh preview.</p>}
    {error && <p role="alert" className="mcp_error" data-testid="mcp_review_error">{error} Close this dialog to request a new review.</p>}
    <div className="mcp_actions"><button className="btn" disabled={busy} onClick={on_close} data-testid="mcp_cancel_review">{preview.can_apply ? 'Cancel' : 'Close preview'}</button>
      {preview.can_apply && <button className="btn primary" data-testid="mcp_apply" disabled={!acknowledged || expired || busy || Boolean(error) || !preview.review_token} onClick={() => { void apply(); }}>{busy ? 'Applying…' : 'Back up and apply'}</button>}
    </div>
  </Mcp_dialog>;
}
