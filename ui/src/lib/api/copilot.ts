/// Copilot CLI Tauri command wrappers.
/// Commands defined in `src-tauri/src/commands/launcher.rs`
/// (`copilot_open_cli_terminal`).
import { invoke } from '@tauri-apps/api/core';

export interface CopilotCliTerminalResult {
  copilotBinary: string;
  providerId: string;
  /** Variable names are retained; values are redacted by the backend. */
  env: Array<[string, string]>;
}

/** Launch Copilot CLI in an isolated terminal (injects COPILOT_PROVIDER_* + outbound proxy). */
export async function copilotOpenCliTerminal(
  cwd?: string,
): Promise<CopilotCliTerminalResult> {
  return invoke<CopilotCliTerminalResult>('copilot_open_cli_terminal', {
    cwd: cwd ?? null,
  });
}
