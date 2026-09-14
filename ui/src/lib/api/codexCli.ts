import { invoke } from '@tauri-apps/api/core';

export interface CodexCliStatus {
  isolationEnabled: boolean;
  configDir: string;
  initialized: boolean;
  codexCliHome: string | null;
  outboundProxy: string | null;
  codexBinary: string | null;
  currentProviderId: string | null;
  currentProviderName: string | null;
}

export interface CodexCliTerminalResult {
  codexHome: string;
  codexBinary: string;
  env: Array<[string, string]>;
}

export interface CodexDesktopResult {
  codexHome: string;
  codexBinary: string;
  mirror: boolean;
}

/** Read a device-level setting (app_meta table). */
export async function settingsGet(key: string): Promise<string | null> {
  return invoke<string | null>('settings_get', { key });
}

/** Write a device-level setting. An empty value deletes the key. */
export async function settingsSet(key: string, value: string): Promise<void> {
  return invoke('settings_set', { key, value });
}

/** Read the Codex CLI isolated-agent status. */
export async function codexCliStatus(): Promise<CodexCliStatus> {
  return invoke<CodexCliStatus>('codex_cli_status');
}

/** Launch Codex CLI in an isolated terminal (injects CODEX_HOME + outbound proxy). */
export async function codexOpenCliTerminal(
  cwd?: string,
): Promise<CodexCliTerminalResult> {
  return invoke<CodexCliTerminalResult>('codex_open_cli_terminal', {
    cwd: cwd ?? null,
  });
}

/** Launch a NEW Codex Desktop instance against AAG's mirror CODEX_HOME. */
export async function codexOpenDesktopWindow(): Promise<CodexDesktopResult> {
  return invoke<CodexDesktopResult>('codex_open_desktop_window');
}
