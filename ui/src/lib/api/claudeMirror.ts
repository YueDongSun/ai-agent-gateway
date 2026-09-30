import { invoke } from '@tauri-apps/api/core';

export interface ClaudeMirrorStatus {
  mirrorDir: string;
  initialized: boolean;
  baseUrlWritten: boolean;
  /** Key-presence only — the token value never crosses the IPC boundary. */
  authTokenWritten: boolean;
  /** Mirror mode: 'token' (default) | 'passthrough'. */
  mode: string;
  claudeBinary: string | null;
  desktopSupported: boolean;
}

export interface ClaudeDesktopResult {
  claudeHome: string;
  claudeBinary: string;
  mirror: boolean;
}

export interface ClaudeMirrorModeResult {
  mode: string;
  written: string[];
}

export interface Claude_cli_result {
  claudeHome: string;
  claudeBinary: string;
  /** Variable names only; the backend redacts environment values. */
  env: Array<[string, string]>;
}

/** Read the Claude mirror (CLAUDE_CONFIG_DIR) status. */
export async function claudeMirrorStatus(): Promise<ClaudeMirrorStatus> {
  return invoke<ClaudeMirrorStatus>('claude_mirror_status');
}

/** Switch the Claude mirror between token and passthrough mode (rewrites
 *  the mirror settings.json immediately; the native ~/.claude is never
 *  touched). */
export async function claudeMirrorSetMode(mode: string): Promise<ClaudeMirrorModeResult> {
  return invoke<ClaudeMirrorModeResult>('claude_mirror_set_mode', { mode });
}

/** Launch a Claude Desktop instance against AAG's CLAUDE_CONFIG_DIR
 *  mirror. Errors with `claude_desktop_not_found` when no install is
 *  detected. */
export async function claudeOpenDesktop(): Promise<ClaudeDesktopResult> {
  return invoke<ClaudeDesktopResult>('claude_open_desktop');
}

/** Prepare the isolated configuration and launch Claude Code in a terminal. */
export async function open_claude_cli_terminal(): Promise<Claude_cli_result> {
  return invoke<Claude_cli_result>('claude_open_cli_terminal');
}
