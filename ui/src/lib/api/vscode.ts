/// VS Code extension Tauri command wrappers.
/// Commands defined in `src-tauri/src/commands/launcher.rs`
/// (`vscode_open_with_extension`, `vscode_extension_status`).
import { invoke } from '@tauri-apps/api/core';

export interface VsCodeLaunchResult {
  binary: string;
  appType: string;
  baseUrl: string;
  authTokenSet: boolean;
  /** Variable names are retained; values are redacted by the backend. */
  env: Array<[string, string]>;
}

export interface VsCodeExtensionStatus {
  binary: string | null;
  extensionsDir: string | null;
  claudeCodeInstalled: boolean;
  kiloCodeInstalled: boolean;
}

/** Open an isolated editor profile with AAG injected as the upstream
 * (ANTHROPIC_BASE_URL for claude_vscode, KILO_CONFIG / KILO_CONFIG_CONTENT
 * for kilo_vscode). */
export async function vscodeOpenWithExtension(
  appType: 'claude_vscode' | 'kilo_vscode',
): Promise<VsCodeLaunchResult> {
  return invoke<VsCodeLaunchResult>('vscode_open_with_extension', { appType });
}

/** Which VS Code binary the launcher would use + whether the Claude Code /
 * Kilo Code extensions are installed. */
export async function vscodeExtensionStatus(): Promise<VsCodeExtensionStatus> {
  return invoke<VsCodeExtensionStatus>('vscode_extension_status');
}
