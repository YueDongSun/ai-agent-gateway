/** Synthetic registry and client file; native IPC never dispatches here. */
import type { Mcp_save_input, Mcp_server_summary, Mcp_sync_preview } from '../api/mcp.js';

const servers = new Map<string, Mcp_save_input>();
const server_revisions = new Map<string, number>();
const client_entries = new Map<string, unknown>();
const plans = new Map<string, { revision: number; expires_at: number; preview: Mcp_sync_preview }>();
let revision = 0;
let sequence = 0;
function redact(config: unknown): unknown {
  if (!config || typeof config !== 'object' || Array.isArray(config)) return null;
  return Object.fromEntries(Object.entries(config).map(([key, value]) => {
    if (key === 'args' && Array.isArray(value)) return [key, value.map(() => '[REDACTED]')];
    if ((key === 'env' || key === 'headers') && value && typeof value === 'object') return [key, Object.fromEntries(Object.keys(value).map((entry) => [entry, '[REDACTED]']))];
    if (key === 'type') return [key, value];
    return [key, '[REDACTED]'];
  }));
}
function summary(server: Mcp_save_input): Mcp_server_summary {
  const config = server.server_config as Record<string, unknown>;
  return { revision: 'demo_revision_' + (server_revisions.get(server.id) ?? 0), id: server.id, name: server.name, description: server.description ?? null, enabled_claude: server.enabled_claude, enabled_codex: server.enabled_codex,
    transport: typeof config?.command === 'string' ? 'stdio' : typeof config?.url === 'string' ? 'http' : 'invalid', redacted_config: redact(config), validation_error: null };
}
function validate(input: Mcp_save_input) {
  if (!/^[a-z][a-z0-9_]{0,63}$/.test(input.id) || !input.name.trim()) throw new Error('MCP_INVALID_INPUT');
  const config = input.server_config as Record<string, unknown>;
  if (!config || typeof config !== 'object' || Array.isArray(config)) throw new Error('MCP_INVALID_CONFIG');
  if (JSON.stringify(config).length > 32768 || JSON.stringify(config).includes('[REDACTED]')) throw new Error('MCP_INVALID_CONFIG');
  if (typeof config.command === 'string') {
    if (!config.command.trim() || /[\r\n;&|]/.test(config.command) || config.url || (config.args !== undefined && (!Array.isArray(config.args) || config.args.some((arg) => typeof arg !== 'string')))) throw new Error('MCP_INVALID_CONFIG');
  } else if (typeof config.url === 'string') {
    let url: URL;
    try { url = new URL(config.url); } catch { throw new Error('MCP_INVALID_CONFIG'); }
    if (url.protocol !== 'https:' || url.username || url.password || url.hash) throw new Error('MCP_INVALID_CONFIG');
  } else throw new Error('MCP_INVALID_CONFIG');
}
export async function mock_mcp_command(command: string, args: Record<string, unknown>): Promise<unknown> {
  if (command === 'mcp_list_servers') return [...servers.values()].sort((a, b) => a.id.localeCompare(b.id)).map(summary);
  if (command === 'mcp_save_server') {
    const input = structuredClone(args.input) as Mcp_save_input;
    const previous = servers.get(input.id);
    if (!previous && input.expected_revision !== undefined) throw new Error('MCP_REVISION_MISMATCH');
    if (previous && input.create_only) throw new Error('MCP_SERVER_EXISTS');
    if (previous && input.expected_revision !== summary(previous).revision) throw new Error('MCP_REVISION_MISMATCH');
    if (input.server_config == null) input.server_config = servers.get(input.id)?.server_config;
    validate(input); servers.set(input.id, input); server_revisions.set(input.id, ++revision); return summary(input);
  }
  if (command === 'mcp_delete_server') { servers.delete(String(args.id)); ++revision; return null; }
  if (command === 'mcp_set_enabled') {
    const server = servers.get(String(args.id));
    if (!server || !['claude', 'codex'].includes(String(args.client)) || typeof args.enabled !== 'boolean') throw new Error('MCP_INVALID_INPUT');
    if (args.client === 'claude') server.enabled_claude = args.enabled; else server.enabled_codex = args.enabled;
    server_revisions.set(server.id, ++revision); return summary(server);
  }
  if (command === 'mcp_preview_sync') {
    if (args.client !== 'claude' && args.client !== 'codex') throw new Error('MCP_INVALID_CLIENT');
    const client = args.client;
    const selected = [...servers.values()].filter((server) => client === 'claude' ? server.enabled_claude : server.enabled_codex);
    const changes: Mcp_sync_preview['changes'] = [];
    for (const server of servers.values()) {
      const key = 'aag_' + server.id;
      if (selected.includes(server)) changes.push({ server_id: server.id, action: client_entries.has(key) ? 'replace' : 'add' });
      else if (client === 'claude' && client_entries.has(key)) changes.push({ server_id: key, action: 'remove' });
    }
    const token = `demo_mcp_${++sequence}`;
    const expires_at = Date.now() + 300000;
    const preview: Mcp_sync_preview = { client, format: client === 'claude' ? 'json' : 'toml', server_ids: selected.map((server) => server.id),
      redacted_config: client === 'claude' ? JSON.stringify({ mcpServers: Object.fromEntries(selected.map((server) => ['aag_' + server.id, redact(server.server_config)])) }, null, 2) : selected.map((server) => `[mcp_servers.aag_${server.id}]\ncommand = "[REDACTED]"`).join('\n\n'),
      changes, warnings: ['Synthetic browser demo: no real client file is accessed.', ...(client === 'codex' ? ['Preview only. No TOML writes are supported.'] : ['The existing configuration is backed up before replacement.'])],
      review_token: client === 'claude' ? token : null, expires_at: client === 'claude' ? new Date(expires_at).toISOString() : null, can_apply: client === 'claude' };
    if (client === 'claude') plans.set(token, { revision, expires_at, preview });
    return preview;
  }
  if (command === 'mcp_discard_review') { plans.delete(String(args.review_token)); return null; }
  if (command === 'mcp_apply_sync') {
    const plan = plans.get(String(args.review_token)); plans.delete(String(args.review_token));
    if (!plan || plan.expires_at <= Date.now()) throw new Error('MCP_REVIEW_EXPIRED');
    if (plan.revision !== revision) throw new Error('MCP_REVIEW_STALE');
    const backup_created = client_entries.size > 0;
    for (const server of servers.values()) { if (server.enabled_claude) client_entries.set('aag_' + server.id, structuredClone(server.server_config)); else client_entries.delete('aag_' + server.id); }
    ++revision;
    return { client: 'claude', applied_server_count: plan.preview.changes.filter((change) => change.action !== 'unchanged').length, backup_created, restart_required: true };
  }
  throw new Error('MCP_UNKNOWN_COMMAND');
}
