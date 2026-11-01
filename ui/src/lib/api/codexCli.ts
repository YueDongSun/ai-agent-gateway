import { invoke } from '@tauri-apps/api/core';

export interface Codex_reasoning_model {
  model: string;
  supportedEfforts: string[];
  isDefault?: boolean;
}

export function codex_reasoning_models(provider_id?: string): Promise<Codex_reasoning_model[]> {
  return invoke('codex_reasoning_models', { providerId: provider_id ?? null });
}

export interface CodexCliStatus {
  isolationEnabled: boolean;
  configDir: string;
  initialized: boolean;
  codexCliHome: string | null;
  outboundProxy: string | null;
  codexBinary: string | null;
  currentProviderId: string | null;
  currentProviderName: string | null;
  /** Mirror config mode: 'exclusive' (default) | 'coexist'. */
  mirrorMode: string;
  desktopSupported: boolean;
}

export interface CodexMirrorModeResult {
  mode: string;
  written: string[];
}

export interface CodexCliTerminalResult {
  codexHome: string;
  codexBinary: string;
  /** Variable names are retained; values are redacted by the backend. */
  env: Array<[string, string]>;
}

export interface CodexDesktopResult {
  codexHome: string;
  codexBinary: string;
  mirror: boolean;
  /** Explicit AAG Desktop preparation selects AAG in the isolated home. */
  mirrorMode?: 'exclusive';
  mirrorModeChanged?: boolean;
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

/** Switch the Codex mirror between exclusive and coexist mode (rewrites
 *  the mirror config.toml immediately; auth.json is never touched). */
export async function codexMirrorSetMode(mode: string): Promise<CodexMirrorModeResult> {
  return invoke<CodexMirrorModeResult>('codex_mirror_set_mode', { mode });
}
