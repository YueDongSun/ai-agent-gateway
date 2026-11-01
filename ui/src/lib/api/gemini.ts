/// Gemini CLI Tauri command wrappers.
/// Command defined in `src-tauri/src/commands/launcher.rs`
/// (`gemini_open_cli_terminal`).
import { invoke } from '@tauri-apps/api/core';

export interface GeminiCliTerminalResult {
  geminiBinary: string;
  providerId: string;
  /** Variable names are retained; values are redacted by the backend. */
  env: Array<[string, string]>;
}

/** Launch the Gemini CLI in an isolated terminal after applying the current
 *  provider to `~/.gemini/.env` (the CLI reads the file itself). */
export async function geminiOpenCliTerminal(
  cwd?: string,
): Promise<GeminiCliTerminalResult> {
  return invoke<GeminiCliTerminalResult>('gemini_open_cli_terminal', {
    cwd: cwd ?? null,
  });
}
