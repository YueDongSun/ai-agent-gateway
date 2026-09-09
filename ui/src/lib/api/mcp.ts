/// MCP Tauri command wrappers.
/// Commands defined in `src-tauri/src/lib.rs` (mcp_list, mcp_save, mcp_delete, mcp_toggle_app).
import { invoke } from '@tauri-apps/api/core';

export interface McpServer {
  id: string;
  name: string;
  serverConfig: Record<string, unknown>;
  description?: string | null;
  homepage?: string | null;
  docs?: string | null;
  tags: string[];
  enabledClaude: boolean;
  enabledCodex: boolean;
}

export async function listMcpServers(): Promise<McpServer[]> {
  return invoke<McpServer[]>('mcp_list');
}

export async function saveMcpServer(server: McpServer): Promise<void> {
  await invoke('mcp_save', { server });
}

export async function deleteMcpServer(id: string): Promise<void> {
  await invoke('mcp_delete', { id });
}

export async function toggleMcpApp(id: string, app: string, enabled: boolean): Promise<void> {
  await invoke('mcp_toggle_app', { id, app, enabled });
}