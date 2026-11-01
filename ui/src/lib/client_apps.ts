import type { AppType } from './api/types.js';

/** Navigation metadata only. Connection forms and supply state belong to Suppliers. */
export const client_apps: Partial<Record<AppType, { label: string; enabled: boolean }>> = {
  claude_code: { label: 'Claude', enabled: true },
  codex: { label: 'Codex', enabled: true },
  copilot: { label: 'Copilot CLI', enabled: true },
  gemini: { label: 'Gemini CLI', enabled: true },
  opencode: { label: 'OpenCode', enabled: true },
  pi: { label: 'pi', enabled: false },
  kilo_cli: { label: 'Kilo CLI', enabled: true },
  cline_cli: { label: 'Cline CLI', enabled: true },
  claude_vscode: { label: 'Claude Code (VS Code)', enabled: true },
  kilo_vscode: { label: 'Kilo Code (VS Code)', enabled: true },
};

export const client_page_order = (Object.keys(client_apps) as AppType[])
  .filter((app_type) => client_apps[app_type]?.enabled);

export function client_label(app_type: AppType): string {
  return client_apps[app_type]?.label ?? app_type;
}
