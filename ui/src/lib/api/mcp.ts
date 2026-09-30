import { invoke } from '@tauri-apps/api/core';

export type Mcp_client = 'claude' | 'codex';
export interface Mcp_server_summary {
  revision: string;
  id: string;
  name: string;
  description: string | null;
  transport: 'stdio' | 'http' | 'invalid';
  enabled_claude: boolean;
  enabled_codex: boolean;
  redacted_config: unknown;
  validation_error: string | null;
}
export interface Mcp_save_input {
  create_only: boolean;
  expected_revision?: string;
  id: string;
  name: string;
  description?: string | null;
  server_config?: unknown;
  enabled_claude: boolean;
  enabled_codex: boolean;
}
export interface Mcp_sync_preview {
  client: Mcp_client;
  format: 'json' | 'toml';
  redacted_config: string;
  server_ids: string[];
  warnings: string[];
  review_token: string | null;
  expires_at: string | null;
  changes: { server_id: string; action: 'add' | 'replace' | 'remove' | 'unchanged' }[];
  can_apply: boolean;
}
export interface Mcp_sync_receipt {
  client: Mcp_client;
  applied_server_count: number;
  backup_created: boolean;
  restart_required: boolean;
}
export const mcp_list_servers = () => invoke<Mcp_server_summary[]>('mcp_list_servers');
export const mcp_save_server = (input: Mcp_save_input) => invoke<Mcp_server_summary>('mcp_save_server', { input });
export const mcp_delete_server = (id: string) => invoke<void>('mcp_delete_server', { id });
export const mcp_set_enabled = (id: string, client: Mcp_client, enabled: boolean) => invoke<Mcp_server_summary>('mcp_set_enabled', { id, client, enabled });
export const mcp_preview_sync = (client: Mcp_client) => invoke<Mcp_sync_preview>('mcp_preview_sync', { client });
export const mcp_apply_sync = (review_token: string) => invoke<Mcp_sync_receipt>('mcp_apply_sync', { review_token });
export const mcp_discard_review = (review_token: string) => invoke<void>('mcp_discard_review', { review_token });

/** Never forward arbitrary backend error text that might contain configuration. */
export function mcp_error_message(error: unknown): string {
  const code = String(error).match(/MCP_[A-Z_]+|mcp:[a-z_]+/)?.[0];
  return code ? `Request rejected (${code}). Refresh and review the configuration before retrying.` : 'MCP request failed. No success was confirmed. Refresh and try again.';
}
